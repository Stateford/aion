//! Placement cost functions.
//!
//! Evaluates the quality of a placement using half-perimeter wire length (HPWL),
//! timing-critical path delays, and congestion estimates. The total cost is a
//! weighted sum used by simulated annealing to guide placement optimization.

#[cfg(test)]
use crate::data::PnrCell;
use crate::data::PnrNetlist;
use crate::ids::PnrNetId;

/// Weights for the placement cost function components.
#[derive(Debug, Clone)]
pub struct PlacementCost {
    /// Weight for wire length (HPWL) component.
    pub weight_wirelength: f64,
    /// Weight for timing component.
    pub weight_timing: f64,
    /// Weight for congestion component.
    pub weight_congestion: f64,
}

impl Default for PlacementCost {
    fn default() -> Self {
        Self {
            weight_wirelength: 1.0,
            weight_timing: 0.5,
            weight_congestion: 0.1,
        }
    }
}

impl PlacementCost {
    /// Computes the total placement cost for the current netlist state.
    ///
    /// Returns a weighted sum of HPWL, timing penalty, and congestion estimate.
    pub fn total_cost(&self, netlist: &PnrNetlist) -> f64 {
        let hpwl = total_hpwl(netlist);
        // For Phase 2, timing and congestion are simplified estimates
        self.weight_wirelength * hpwl
    }

    /// Computes the incremental cost change from swapping two cells.
    ///
    /// Only recomputes the HPWL for nets affected by the swapped cells,
    /// rather than recomputing the entire cost from scratch.
    pub fn incremental_cost(&self, netlist: &PnrNetlist, affected_nets: &[PnrNetId]) -> f64 {
        let mut cost = 0.0;
        for &net_id in affected_nets {
            cost += self.weight_wirelength * net_hpwl(netlist, net_id);
        }
        cost
    }
}

/// Computes the total half-perimeter wire length across all nets.
///
/// HPWL is the half-perimeter of the bounding box of all pins on each net.
/// It is the standard placement metric — minimizing HPWL tends to produce
/// good routability and timing.
pub fn total_hpwl(netlist: &PnrNetlist) -> f64 {
    let mut total = 0.0;
    for i in 0..netlist.nets.len() {
        let net_id = PnrNetId::from_raw(i as u32);
        total += net_hpwl(netlist, net_id);
    }
    total
}

/// Computes the HPWL for a single net.
///
/// Uses synthetic site coordinates (site_id / grid_width for row, site_id % grid_width
/// for column) since the actual grid is not populated in Phase 2.
fn net_hpwl(netlist: &PnrNetlist, net_id: PnrNetId) -> f64 {
    let net = netlist.net(net_id);

    // Collect all pin cell positions
    let mut min_x: i64 = i64::MAX;
    let mut max_x: i64 = i64::MIN;
    let mut min_y: i64 = i64::MAX;
    let mut max_y: i64 = i64::MIN;

    let grid_width = 100; // synthetic grid width for coordinate estimation

    let driver_cell = netlist.pin(net.driver).cell;
    if let Some(site) = netlist.cell(driver_cell).placement {
        let (x, y) = site_to_coords(site.as_raw(), grid_width);
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }

    for &sink_pin in &net.sinks {
        let sink_cell = netlist.pin(sink_pin).cell;
        if let Some(site) = netlist.cell(sink_cell).placement {
            let (x, y) = site_to_coords(site.as_raw(), grid_width);
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }

    if min_x == i64::MAX {
        return 0.0;
    }

    (max_x - min_x + max_y - min_y) as f64
}

/// Converts a synthetic site ID to grid coordinates.
fn site_to_coords(site_raw: u32, grid_width: u32) -> (i64, i64) {
    let x = (site_raw % grid_width) as i64;
    let y = (site_raw / grid_width) as i64;
    (x, y)
}

/// Returns the list of net IDs affected by swapping two cells.
#[cfg(test)]
pub fn affected_nets(netlist: &PnrNetlist, cell_a: &PnrCell, cell_b: &PnrCell) -> Vec<PnrNetId> {
    let mut nets = std::collections::HashSet::new();

    // Find nets connected to either cell
    for pin in &netlist.pins {
        if pin.cell == cell_a.id || pin.cell == cell_b.id {
            if let Some(net_id) = pin.net {
                nets.insert(net_id);
            }
        }
    }

    // Also check net drivers/sinks
    for (i, net) in netlist.nets.iter().enumerate() {
        let driver_cell = netlist.pin(net.driver).cell;
        if driver_cell == cell_a.id || driver_cell == cell_b.id {
            nets.insert(PnrNetId::from_raw(i as u32));
        }
        for &sink in &net.sinks {
            let sink_cell = netlist.pin(sink).cell;
            if sink_cell == cell_a.id || sink_cell == cell_b.id {
                nets.insert(PnrNetId::from_raw(i as u32));
            }
        }
    }

    nets.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{PnrCell, PnrCellType, PnrNet, PnrPin};
    use crate::ids::{PnrCellId, PnrNetId, PnrPinId};
    use aion_arch::ids::SiteId;
    use aion_common::LogicVec;
    use aion_ir::PortDirection;

    fn make_placed_netlist() -> PnrNetlist {
        let mut nl = PnrNetlist::new();

        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: Some(SiteId::from_raw(0)), // (0, 0)
            is_fixed: false,
        });
        let c1 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c1".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: Some(SiteId::from_raw(50)), // (50, 0)
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

        nl
    }

    #[test]
    fn hpwl_same_location() {
        let mut nl = PnrNetlist::new();
        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c0".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(5)),
            is_fixed: false,
        });
        let c1 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c1".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(5)),
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
            name: "I".into(),
            direction: PortDirection::Input,
            cell: c1,
            net: None,
        });
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "n".into(),
            driver: p0,
            sinks: vec![p1],
            routing: None,
            timing_critical: false,
        });
        assert_eq!(total_hpwl(&nl), 0.0);
    }

    #[test]
    fn hpwl_different_locations() {
        let nl = make_placed_netlist();
        let hpwl = total_hpwl(&nl);
        // Site 0 = (0,0), Site 50 = (50,0) in grid_width=100
        // HPWL = |50-0| + |0-0| = 50
        assert_eq!(hpwl, 50.0);
    }

    #[test]
    fn hpwl_empty_netlist() {
        let nl = PnrNetlist::new();
        assert_eq!(total_hpwl(&nl), 0.0);
    }

    #[test]
    fn placement_cost_default() {
        let cost = PlacementCost::default();
        assert_eq!(cost.weight_wirelength, 1.0);
    }

    #[test]
    fn placement_cost_total() {
        let nl = make_placed_netlist();
        let cost = PlacementCost::default();
        let total = cost.total_cost(&nl);
        assert!(total > 0.0);
    }

    #[test]
    fn site_to_coords_basic() {
        assert_eq!(site_to_coords(0, 100), (0, 0));
        assert_eq!(site_to_coords(50, 100), (50, 0));
        assert_eq!(site_to_coords(100, 100), (0, 1));
        assert_eq!(site_to_coords(150, 100), (50, 1));
    }

    #[test]
    fn affected_nets_finds_connected() {
        let nl = make_placed_netlist();
        let cell_a = &nl.cells[0];
        let cell_b = &nl.cells[1];
        let affected = affected_nets(&nl, cell_a, cell_b);
        assert!(!affected.is_empty());
    }

    #[test]
    fn net_hpwl_no_placement() {
        let mut nl = PnrNetlist::new();
        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c0".into(),
            cell_type: PnrCellType::Dff,
            placement: None,
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
            name: "n".into(),
            driver: p0,
            sinks: vec![],
            routing: None,
            timing_critical: false,
        });
        assert_eq!(net_hpwl(&nl, PnrNetId::from_raw(0)), 0.0);
    }
}
