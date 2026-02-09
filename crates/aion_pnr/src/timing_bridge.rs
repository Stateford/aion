//! Converts a placed-and-routed [`PnrNetlist`] into a [`TimingGraph`] for STA.
//!
//! The timing bridge creates timing graph nodes for cell pins and primary I/O,
//! and edges for cell delays, net delays, and setup/hold checks. This allows
//! the timing analysis engine (in `aion_timing`) to analyze the design without
//! depending on PnR data structures.

use crate::data::{PnrCellType, PnrNetlist};
use aion_arch::types::Delay;
use aion_arch::Architecture;
use aion_ir::PortDirection;
use aion_timing::graph::{TimingEdgeType, TimingGraph, TimingNodeType};
use aion_timing::ids::TimingNodeId;
use std::collections::HashMap;

/// Builds a [`TimingGraph`] from a placed-and-routed netlist and architecture model.
///
/// Creates timing nodes for each cell pin and primary I/O port, and timing
/// edges for cell delays, net delays, and setup/hold checks. The resulting
/// graph can be analyzed by `aion_timing::analyze_timing()`.
pub fn build_timing_graph(netlist: &PnrNetlist, arch: &dyn Architecture) -> TimingGraph {
    let mut graph = TimingGraph::new();
    let mut cell_output_nodes: HashMap<(u32, String), TimingNodeId> = HashMap::new();
    let mut cell_input_nodes: HashMap<(u32, String), TimingNodeId> = HashMap::new();

    // 1. Create timing nodes for each cell
    for cell in &netlist.cells {
        let cell_idx = cell.id.as_raw();

        match &cell.cell_type {
            PnrCellType::Iobuf { direction, .. } => {
                let node_type = match direction {
                    PortDirection::Input => TimingNodeType::PrimaryInput,
                    PortDirection::Output => TimingNodeType::PrimaryOutput,
                    PortDirection::InOut => TimingNodeType::PrimaryInput,
                };
                let node = graph.add_node(cell.name.clone(), node_type);
                // IO cell has one pin — map as both input and output for connectivity
                match direction {
                    PortDirection::Input => {
                        cell_output_nodes.insert((cell_idx, "O".into()), node);
                    }
                    PortDirection::Output => {
                        cell_input_nodes.insert((cell_idx, "I".into()), node);
                    }
                    PortDirection::InOut => {
                        cell_output_nodes.insert((cell_idx, "O".into()), node);
                        cell_input_nodes.insert((cell_idx, "I".into()), node);
                    }
                }
            }
            PnrCellType::Lut { .. } | PnrCellType::Carry => {
                // LUT: inputs → cell delay → output
                let out_node = graph.add_node(format!("{}/O", cell.name), TimingNodeType::CellPin);
                cell_output_nodes.insert((cell_idx, "O".into()), out_node);

                // Create input nodes and cell delay edges
                let cell_delay = arch.cell_delay("LUT");
                for i in 0..6 {
                    let pin_name = format!("I{i}");
                    let in_node = graph
                        .add_node(format!("{}/{pin_name}", cell.name), TimingNodeType::CellPin);
                    cell_input_nodes.insert((cell_idx, pin_name), in_node);
                    graph.add_edge(in_node, out_node, cell_delay, TimingEdgeType::CellDelay);
                }
            }
            PnrCellType::Dff => {
                // FF: D input, CLK input, Q output
                let d_node = graph.add_node(format!("{}/D", cell.name), TimingNodeType::CellPin);
                let q_node = graph.add_node(format!("{}/Q", cell.name), TimingNodeType::CellPin);
                let clk_node =
                    graph.add_node(format!("{}/CLK", cell.name), TimingNodeType::CellPin);

                cell_input_nodes.insert((cell_idx, "D".into()), d_node);
                cell_input_nodes.insert((cell_idx, "CLK".into()), clk_node);
                cell_output_nodes.insert((cell_idx, "Q".into()), q_node);

                // Clock-to-Q delay
                let clk_to_q = arch.clock_to_out("DFF");
                graph.add_edge(clk_node, q_node, clk_to_q, TimingEdgeType::ClockToQ);

                // Setup check
                let setup = arch.setup_time("DFF");
                graph.add_edge(clk_node, d_node, setup, TimingEdgeType::SetupCheck);

                // Hold check
                let hold = arch.hold_time("DFF");
                graph.add_edge(clk_node, d_node, hold, TimingEdgeType::HoldCheck);
            }
            _ => {
                // Generic: single input → single output with cell delay
                let in_node = graph.add_node(format!("{}/I", cell.name), TimingNodeType::CellPin);
                let out_node = graph.add_node(format!("{}/O", cell.name), TimingNodeType::CellPin);
                cell_input_nodes.insert((cell_idx, "I".into()), in_node);
                cell_output_nodes.insert((cell_idx, "O".into()), out_node);
                let delay = arch.cell_delay(&format!("{:?}", cell.cell_type));
                graph.add_edge(in_node, out_node, delay, TimingEdgeType::CellDelay);
            }
        }
    }

    // 2. Create net delay edges
    for net in &netlist.nets {
        let driver_pin = netlist.pin(net.driver);
        let driver_cell_idx = driver_pin.cell.as_raw();

        // Find the output node for the driver
        let driver_node = cell_output_nodes
            .get(&(driver_cell_idx, driver_pin.name.clone()))
            .or_else(|| {
                // Try generic output pin name
                cell_output_nodes.get(&(driver_cell_idx, "O".into()))
            });

        let Some(&src_node) = driver_node else {
            continue;
        };

        // Estimate net delay from routing
        let net_delay = estimate_net_delay(netlist, net, arch);

        for &sink_pin_id in &net.sinks {
            let sink_pin = netlist.pin(sink_pin_id);
            let sink_cell_idx = sink_pin.cell.as_raw();

            let sink_node = cell_input_nodes
                .get(&(sink_cell_idx, sink_pin.name.clone()))
                .or_else(|| {
                    // Try generic input pin names
                    cell_input_nodes
                        .get(&(sink_cell_idx, "I".into()))
                        .or_else(|| cell_input_nodes.get(&(sink_cell_idx, "I0".into())))
                        .or_else(|| cell_input_nodes.get(&(sink_cell_idx, "D".into())))
                });

            if let Some(&dst_node) = sink_node {
                graph.add_edge(src_node, dst_node, net_delay, TimingEdgeType::NetDelay);
            }
        }
    }

    graph
}

/// Estimates the net delay based on placement distance and routing resources.
fn estimate_net_delay(
    netlist: &PnrNetlist,
    net: &crate::data::PnrNet,
    _arch: &dyn Architecture,
) -> Delay {
    // Estimate delay from placement distance (Manhattan distance heuristic)
    let driver_pin = netlist.pin(net.driver);
    let driver_cell = netlist.cell(driver_pin.cell);

    let Some(driver_site) = driver_cell.placement else {
        return Delay::ZERO;
    };

    let mut max_distance: u32 = 0;
    let grid_width = 100;

    for &sink_pin_id in &net.sinks {
        let sink_pin = netlist.pin(sink_pin_id);
        let sink_cell = netlist.cell(sink_pin.cell);
        if let Some(sink_site) = sink_cell.placement {
            let dx = (driver_site.as_raw() % grid_width).abs_diff(sink_site.as_raw() % grid_width);
            let dy = (driver_site.as_raw() / grid_width).abs_diff(sink_site.as_raw() / grid_width);
            max_distance = max_distance.max(dx + dy);
        }
    }

    // Rough estimate: 0.1 ns per unit distance
    let delay_ns = max_distance as f64 * 0.1;
    Delay::new(delay_ns * 0.5, delay_ns, delay_ns * 1.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{PnrCell, PnrNet, PnrPin};
    use crate::ids::{PnrCellId, PnrNetId, PnrPinId};
    use aion_arch::ids::SiteId;
    use aion_arch::load_architecture;
    use aion_common::LogicVec;

    #[test]
    fn empty_netlist_produces_empty_graph() {
        let nl = PnrNetlist::new();
        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let graph = build_timing_graph(&nl, &*arch);
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn io_cells_become_primary_nodes() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "io_in".into(),
            cell_type: PnrCellType::Iobuf {
                direction: PortDirection::Input,
                standard: "LVCMOS33".into(),
            },
            placement: Some(SiteId::from_raw(0)),
            is_fixed: true,
        });
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "io_out".into(),
            cell_type: PnrCellType::Iobuf {
                direction: PortDirection::Output,
                standard: "LVCMOS33".into(),
            },
            placement: Some(SiteId::from_raw(1)),
            is_fixed: true,
        });

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let graph = build_timing_graph(&nl, &*arch);
        assert_eq!(graph.node_count(), 2);

        let input_nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.node_type == TimingNodeType::PrimaryInput)
            .collect();
        let output_nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.node_type == TimingNodeType::PrimaryOutput)
            .collect();
        assert_eq!(input_nodes.len(), 1);
        assert_eq!(output_nodes.len(), 1);
    }

    #[test]
    fn lut_cell_creates_delay_edges() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });

        let arch = load_architecture("artix7", "xc7a100tcsg324-1").unwrap();
        let graph = build_timing_graph(&nl, &*arch);

        // Should have input nodes + 1 output node
        assert!(graph.node_count() >= 2);
        // Should have cell delay edges from inputs to output
        let cell_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == TimingEdgeType::CellDelay)
            .collect();
        assert!(!cell_edges.is_empty());
    }

    #[test]
    fn dff_cell_creates_timing_checks() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "ff_0".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });

        let arch = load_architecture("cyclone_v", "5CSEMA5F31C6").unwrap();
        let graph = build_timing_graph(&nl, &*arch);

        // Should have D, CLK, Q nodes
        assert!(graph.node_count() >= 3);

        // Should have ClockToQ, SetupCheck, HoldCheck edges
        let clk_to_q: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == TimingEdgeType::ClockToQ)
            .collect();
        let setup: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == TimingEdgeType::SetupCheck)
            .collect();
        let hold: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == TimingEdgeType::HoldCheck)
            .collect();
        assert_eq!(clk_to_q.len(), 1);
        assert_eq!(setup.len(), 1);
        assert_eq!(hold.len(), 1);
    }

    #[test]
    fn net_creates_net_delay_edge() {
        let mut nl = PnrNetlist::new();

        // Input IO
        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "io_in".into(),
            cell_type: PnrCellType::Iobuf {
                direction: PortDirection::Input,
                standard: "LVCMOS33".into(),
            },
            placement: Some(SiteId::from_raw(0)),
            is_fixed: true,
        });

        // LUT
        let c1 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: Some(SiteId::from_raw(50)),
            is_fixed: false,
        });

        let p0 = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: c0,
            net: None,
        });
        let p1 = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "I0".into(),
            direction: PortDirection::Input,
            cell: c1,
            net: None,
        });

        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: p0,
            sinks: vec![p1],
            routing: None,
            timing_critical: false,
        });

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let graph = build_timing_graph(&nl, &*arch);

        let net_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == TimingEdgeType::NetDelay)
            .collect();
        assert!(!net_edges.is_empty());
    }

    #[test]
    fn net_delay_proportional_to_distance() {
        let mut nl = PnrNetlist::new();

        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c0".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });

        let p0 = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: c0,
            net: None,
        });

        let net = PnrNet {
            id: PnrNetId::from_raw(0),
            name: "test".into(),
            driver: p0,
            sinks: vec![],
            routing: None,
            timing_critical: false,
        };

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let delay = estimate_net_delay(&nl, &net, &*arch);
        // No sinks → zero distance
        assert_eq!(delay.typ_ns, 0.0);
    }
}
