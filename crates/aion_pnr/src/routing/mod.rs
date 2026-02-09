//! Routing engine for the PnR pipeline.
//!
//! Routes all nets in the netlist, connecting each driver pin to its sink
//! pins through the device's routing fabric. Uses the PathFinder negotiated
//! congestion algorithm with A* search for individual net routing.

mod astar;
mod congestion;
mod pathfinder;

use crate::data::PnrNetlist;
use aion_arch::Architecture;
use aion_diagnostics::DiagnosticSink;

/// Routes all nets in the netlist.
///
/// In Phase 2, the device routing graph is not yet populated, so this
/// produces stub route trees (direct connections). When Phase 3 populates
/// the routing graph, this will use PathFinder + A* for full routing.
pub fn route(netlist: &mut PnrNetlist, arch: &dyn Architecture, sink: &DiagnosticSink) {
    if arch.routing_graph().wires.is_empty() {
        // Phase 2 stub: create direct route trees
        pathfinder::stub_routing(netlist, sink);
    } else {
        // Phase 3: full PathFinder routing
        pathfinder::pathfinder_route(netlist, arch, sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{PnrCell, PnrCellType, PnrNet, PnrPin};
    use crate::ids::{PnrCellId, PnrNetId, PnrPinId};
    use aion_arch::ids::SiteId;
    use aion_arch::load_architecture;
    use aion_common::LogicVec;
    use aion_ir::PortDirection;

    #[test]
    fn route_assigns_all_nets() {
        let mut nl = PnrNetlist::new();
        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c0".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });
        let c1 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "c1".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(1)),
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
            name: "net_0".into(),
            driver: p0,
            sinks: vec![p1],
            routing: None,
            timing_critical: false,
        });

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let sink = DiagnosticSink::new();
        route(&mut nl, &*arch, &sink);
        assert!(nl.is_fully_routed());
    }

    #[test]
    fn route_empty_netlist() {
        let mut nl = PnrNetlist::new();
        let arch = load_architecture("artix7", "xc7a100tcsg324-1").unwrap();
        let sink = DiagnosticSink::new();
        route(&mut nl, &*arch, &sink);
        assert!(nl.is_fully_routed());
    }

    #[test]
    fn route_net_with_fanout() {
        let mut nl = PnrNetlist::new();
        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "driver".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });
        let c1 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "sink1".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(10)),
            is_fixed: false,
        });
        let c2 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "sink2".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(20)),
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
            name: "D".into(),
            direction: PortDirection::Input,
            cell: c1,
            net: None,
        });
        let p2 = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "D".into(),
            direction: PortDirection::Input,
            cell: c2,
            net: None,
        });

        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "fanout_net".into(),
            driver: p0,
            sinks: vec![p1, p2],
            routing: None,
            timing_critical: false,
        });

        let arch = load_architecture("cyclone_v", "5CSEMA5F31C6").unwrap();
        let sink = DiagnosticSink::new();
        route(&mut nl, &*arch, &sink);
        assert!(nl.is_fully_routed());
    }
}
