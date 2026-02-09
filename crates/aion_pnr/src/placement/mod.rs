//! Placement engine for the PnR pipeline.
//!
//! Assigns each cell in the netlist to a physical site on the FPGA device.
//! Uses random initial placement followed by simulated annealing refinement
//! to minimize wire length and timing-critical path delays.

mod anneal;
mod cost;
mod random;

use crate::data::PnrNetlist;
use aion_arch::Architecture;
use aion_diagnostics::DiagnosticSink;

pub use cost::PlacementCost;

/// Performs placement on the netlist, assigning each cell to a device site.
///
/// First generates a random initial placement using resource counts from the
/// architecture, then refines it with simulated annealing to minimize
/// estimated wire length and timing cost.
pub fn place(netlist: &mut PnrNetlist, arch: &dyn Architecture, sink: &DiagnosticSink) {
    // Phase 1: Random initial placement using synthetic site IDs
    random::random_placement(netlist, arch, sink);

    // Phase 2: Simulated annealing refinement
    anneal::simulated_annealing(netlist, arch, sink);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{PnrCell, PnrCellType, PnrNet, PnrPin};
    use crate::ids::{PnrCellId, PnrNetId, PnrPinId};
    use aion_arch::load_architecture;
    use aion_common::LogicVec;
    use aion_ir::PortDirection;

    fn make_test_netlist() -> PnrNetlist {
        let mut nl = PnrNetlist::new();

        // Create some LUT cells
        for i in 0..5 {
            nl.add_cell(PnrCell {
                id: PnrCellId::from_raw(0),
                name: format!("lut_{i}"),
                cell_type: PnrCellType::Lut {
                    inputs: 4,
                    init: LogicVec::from_bool(false),
                },
                placement: None,
                is_fixed: false,
            });
        }

        // Create a DFF
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "ff_0".into(),
            cell_type: PnrCellType::Dff,
            placement: None,
            is_fixed: false,
        });

        // Create pins and nets
        let mut pins = Vec::new();
        for i in 0..6 {
            let p = nl.add_pin(PnrPin {
                id: PnrPinId::from_raw(0),
                name: format!("O_{i}"),
                direction: PortDirection::Output,
                cell: PnrCellId::from_raw(i),
                net: None,
            });
            pins.push(p);
        }

        // Some simple nets
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: pins[0],
            sinks: vec![pins[1]],
            routing: None,
            timing_critical: false,
        });
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_1".into(),
            driver: pins[2],
            sinks: vec![pins[3], pins[4]],
            routing: None,
            timing_critical: false,
        });

        nl
    }

    #[test]
    fn place_assigns_all_cells() {
        let mut nl = make_test_netlist();
        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let sink = DiagnosticSink::new();
        place(&mut nl, &*arch, &sink);
        assert!(nl.is_fully_placed());
    }

    #[test]
    fn place_empty_netlist() {
        let mut nl = PnrNetlist::new();
        let arch = load_architecture("artix7", "xc7a35ticpg236-1L").unwrap();
        let sink = DiagnosticSink::new();
        place(&mut nl, &*arch, &sink);
        assert!(nl.is_fully_placed());
    }

    #[test]
    fn place_single_cell() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "solo".into(),
            cell_type: PnrCellType::Dff,
            placement: None,
            is_fixed: false,
        });
        let arch = load_architecture("cyclone_v", "5CSEMA5F31C6").unwrap();
        let sink = DiagnosticSink::new();
        place(&mut nl, &*arch, &sink);
        assert!(nl.is_fully_placed());
    }
}
