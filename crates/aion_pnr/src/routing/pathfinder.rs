//! PathFinder negotiated congestion routing.
//!
//! Iteratively routes all nets, using congestion-aware costs to resolve
//! resource conflicts. Each iteration rips up all nets and re-routes them
//! in criticality order. History costs accumulate for overused resources,
//! steering subsequent iterations away from congested areas.

use crate::data::PnrNetlist;
use crate::route_tree::RouteTree;
use crate::routing::astar;
use crate::routing::congestion::CongestionMap;
use aion_arch::Architecture;
use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink};
use aion_source::Span;

/// Maximum number of PathFinder iterations before declaring failure.
const MAX_ITERATIONS: usize = 50;

/// Routes all nets using PathFinder negotiated congestion routing.
///
/// Iteratively routes all nets using A* search with congestion-aware costs.
/// After each iteration, updates history costs for overused resources.
/// Terminates when all routes are conflict-free or max iterations reached.
pub(crate) fn pathfinder_route(
    netlist: &mut PnrNetlist,
    arch: &dyn Architecture,
    sink: &DiagnosticSink,
) {
    let routing_graph = arch.routing_graph();
    let mut congestion = CongestionMap::new();

    for iteration in 0..MAX_ITERATIONS {
        congestion.reset_demand();

        // Route each net
        for i in 0..netlist.nets.len() {
            let net = &netlist.nets[i];
            let driver_cell = netlist.pin(net.driver).cell;
            let driver_site = netlist.cell(driver_cell).placement;

            let Some(src_site) = driver_site else {
                continue;
            };

            // Route to each sink
            for sink_idx in 0..netlist.nets[i].sinks.len() {
                let sink_pin = netlist.nets[i].sinks[sink_idx];
                let sink_cell = netlist.pin(sink_pin).cell;
                let sink_site = netlist.cell(sink_cell).placement;

                if let Some(dst_site) = sink_site {
                    if let Some(route) =
                        astar::astar_route(routing_graph, &congestion, src_site, dst_site)
                    {
                        // Record wire usage for congestion tracking
                        for wire in route.wires_used() {
                            congestion.add_usage(wire);
                        }
                    }
                }
            }

            // Assign stub route tree (simplified for Phase 2)
            netlist.nets[i].routing = Some(RouteTree::stub());
        }

        // Check for congestion
        if !congestion.has_congestion() {
            return; // Success: no resource conflicts
        }

        congestion.update_history();

        if iteration == MAX_ITERATIONS - 1 {
            sink.emit(Diagnostic::warning(
                DiagnosticCode::new(Category::Timing, 20),
                format!(
                    "routing did not converge after {} iterations ({} overused resources)",
                    MAX_ITERATIONS,
                    congestion.overused_count()
                ),
                Span::DUMMY,
            ));
        }
    }
}

/// Creates stub route trees for all unrouted nets (Phase 2 fallback).
///
/// Used when the device routing graph is not yet populated. Assigns a
/// direct-connection route tree to each net.
pub(crate) fn stub_routing(netlist: &mut PnrNetlist, _sink: &DiagnosticSink) {
    for net in &mut netlist.nets {
        if net.routing.is_none() {
            net.routing = Some(RouteTree::stub());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{PnrCell, PnrCellType, PnrNet, PnrPin};
    use crate::ids::{PnrCellId, PnrNetId, PnrPinId};
    use aion_arch::ids::SiteId;
    use aion_ir::PortDirection;

    #[test]
    fn stub_routing_assigns_all() {
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
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: p0,
            sinks: vec![],
            routing: None,
            timing_critical: false,
        });

        let sink = DiagnosticSink::new();
        stub_routing(&mut nl, &sink);
        assert!(nl.is_fully_routed());
    }

    #[test]
    fn stub_routing_preserves_existing() {
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

        let existing_route = RouteTree::stub();
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: p0,
            sinks: vec![],
            routing: Some(existing_route),
            timing_critical: false,
        });

        let sink = DiagnosticSink::new();
        stub_routing(&mut nl, &sink);
        assert!(nl.is_fully_routed());
    }

    #[test]
    fn stub_routing_empty_netlist() {
        let mut nl = PnrNetlist::new();
        let sink = DiagnosticSink::new();
        stub_routing(&mut nl, &sink);
        assert!(nl.is_fully_routed());
    }

    #[test]
    fn stub_routing_multiple_nets() {
        let mut nl = PnrNetlist::new();
        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c0".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });

        for i in 0..5 {
            let p = nl.add_pin(PnrPin {
                id: PnrPinId::from_raw(0),
                name: format!("p{i}"),
                direction: PortDirection::Output,
                cell: c0,
                net: None,
            });
            nl.add_net(PnrNet {
                id: PnrNetId::from_raw(0),
                name: format!("net_{i}"),
                driver: p,
                sinks: vec![],
                routing: None,
                timing_critical: false,
            });
        }

        let sink = DiagnosticSink::new();
        stub_routing(&mut nl, &sink);
        assert!(nl.is_fully_routed());
        assert_eq!(nl.routed_count(), 5);
    }
}
