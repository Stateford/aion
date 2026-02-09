//! Place and route engine for the Aion FPGA toolchain.
//!
//! This crate takes a technology-mapped [`MappedDesign`] (from `aion_synth`) and
//! assigns each cell to a physical device site (placement) then connects all
//! nets through the routing fabric (routing). The output is a fully placed and
//! routed [`PnrNetlist`] ready for bitstream generation.
//!
//! # Pipeline
//!
//! 1. **Convert** — flatten `MappedDesign` to a flat `PnrNetlist`
//! 2. **Place** — random initial placement + simulated annealing refinement
//! 3. **Route** — PathFinder negotiated congestion routing (stub in Phase 2)
//! 4. **Timing bridge** — convert to `TimingGraph` for STA feedback
//!
//! # Usage
//!
//! ```ignore
//! use aion_pnr::place_and_route;
//!
//! let netlist = place_and_route(&mapped_design, &*arch, &constraints, &interner, &sink)?;
//! assert!(netlist.is_fully_placed());
//! assert!(netlist.is_fully_routed());
//! ```

#![warn(missing_docs)]

pub mod convert;
pub mod data;
pub mod ids;
pub mod placement;
pub mod route_tree;
pub mod routing;
pub mod timing_bridge;

pub use convert::convert_to_pnr;
pub use data::{
    BramConfig, DspConfig, PllConfig, PnrCell, PnrCellType, PnrNet, PnrNetlist, PnrPin,
};
pub use ids::{PnrCellId, PnrNetId, PnrPinId};
pub use placement::PlacementCost;
pub use route_tree::{RouteNode, RouteResource, RouteTree};
pub use timing_bridge::build_timing_graph;

use aion_arch::Architecture;
use aion_common::{AionResult, Interner};
use aion_diagnostics::DiagnosticSink;
use aion_synth::MappedDesign;
use aion_timing::TimingConstraints;

/// Performs the complete place-and-route pipeline on a synthesized design.
///
/// Converts the [`MappedDesign`] to a flat netlist, places all cells using
/// simulated annealing, routes all nets, and optionally builds a timing graph
/// for static timing analysis feedback.
///
/// Returns the placed and routed [`PnrNetlist`].
pub fn place_and_route(
    mapped: &MappedDesign,
    arch: &dyn Architecture,
    _constraints: &TimingConstraints,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> AionResult<PnrNetlist> {
    // 1. Convert MappedDesign → PnrNetlist
    let mut netlist = convert_to_pnr(mapped, interner);

    // 2. Placement
    placement::place(&mut netlist, arch, sink);

    // 3. Routing
    routing::route(&mut netlist, arch, sink);

    Ok(netlist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_arch::{Architecture, ResourceUsage, TechMapper};
    use aion_arch::{ArithmeticPattern, LutMapping, MapResult, MemoryCell};
    use aion_common::{ContentHash, Interner};
    use aion_ir::{
        Arena, Cell, CellId, CellKind, Connection, ModuleId, Port, PortDirection, Signal, SignalId,
        SignalKind, SignalRef, Type, TypeDb,
    };
    use aion_source::Span;
    use aion_synth::{MappedDesign, MappedModule};
    use aion_timing::TimingConstraints;

    #[derive(Debug)]
    struct TestArch;

    impl Architecture for TestArch {
        fn family_name(&self) -> &str {
            "test"
        }
        fn device_name(&self) -> &str {
            "test_device"
        }
        fn total_luts(&self) -> u32 {
            1000
        }
        fn total_ffs(&self) -> u32 {
            1000
        }
        fn total_bram(&self) -> u32 {
            10
        }
        fn total_dsp(&self) -> u32 {
            4
        }
        fn total_io(&self) -> u32 {
            100
        }
        fn total_pll(&self) -> u32 {
            2
        }
        fn lut_input_count(&self) -> u32 {
            4
        }
        fn resource_summary(&self) -> ResourceUsage {
            ResourceUsage {
                luts: 1000,
                ffs: 1000,
                bram: 10,
                dsp: 4,
                io: 100,
                pll: 2,
            }
        }
        fn tech_mapper(&self) -> Box<dyn TechMapper> {
            Box::new(TestMapper)
        }
    }

    struct TestMapper;

    impl TechMapper for TestMapper {
        fn map_cell(&self, _: &CellKind) -> MapResult {
            MapResult::Unmappable
        }
        fn infer_bram(&self, _: &MemoryCell) -> bool {
            false
        }
        fn infer_dsp(&self, _: &ArithmeticPattern) -> bool {
            false
        }
        fn map_to_luts(&self, _: &CellKind) -> Vec<LutMapping> {
            vec![]
        }
        fn lut_input_count(&self) -> u32 {
            4
        }
        fn max_bram_depth(&self) -> u32 {
            9216
        }
        fn max_bram_width(&self) -> u32 {
            36
        }
        fn max_dsp_width_a(&self) -> u32 {
            18
        }
        fn max_dsp_width_b(&self) -> u32 {
            18
        }
    }

    fn make_test_mapped_design() -> (MappedDesign, Interner) {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);

        let clk_name = interner.get_or_intern("clk");
        let in_name = interner.get_or_intern("din");
        let out_name = interner.get_or_intern("dout");
        let d_pin = interner.get_or_intern("D");
        let clk_pin = interner.get_or_intern("CLK");
        let q_pin = interner.get_or_intern("Q");
        let dff_name = interner.get_or_intern("dff_0");

        let mut signals = Arena::new();
        let clk_id = signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: clk_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let in_id = signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: in_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let out_id = signals.alloc(Signal {
            id: SignalId::from_raw(2),
            name: out_name,
            ty: bit_ty,
            kind: SignalKind::Reg,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let ports = vec![
            Port {
                id: aion_ir::PortId::from_raw(0),
                name: clk_name,
                direction: PortDirection::Input,
                ty: bit_ty,
                signal: clk_id,
                span: Span::DUMMY,
            },
            Port {
                id: aion_ir::PortId::from_raw(1),
                name: in_name,
                direction: PortDirection::Input,
                ty: bit_ty,
                signal: in_id,
                span: Span::DUMMY,
            },
            Port {
                id: aion_ir::PortId::from_raw(2),
                name: out_name,
                direction: PortDirection::Output,
                ty: bit_ty,
                signal: out_id,
                span: Span::DUMMY,
            },
        ];

        let mut cells: Arena<CellId, Cell> = Arena::new();
        cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: dff_name,
            kind: CellKind::Dff {
                width: 1,
                has_reset: false,
                has_enable: false,
            },
            connections: vec![
                Connection {
                    port_name: d_pin,
                    direction: PortDirection::Input,
                    signal: SignalRef::Signal(in_id),
                },
                Connection {
                    port_name: clk_pin,
                    direction: PortDirection::Input,
                    signal: SignalRef::Signal(clk_id),
                },
                Connection {
                    port_name: q_pin,
                    direction: PortDirection::Output,
                    signal: SignalRef::Signal(out_id),
                },
            ],
            span: Span::DUMMY,
        });

        let mut modules: Arena<ModuleId, MappedModule> = Arena::new();
        modules.alloc(MappedModule {
            id: ModuleId::from_raw(0),
            name: interner.get_or_intern("top"),
            ports,
            signals,
            cells,
            resource_usage: ResourceUsage::default(),
            content_hash: ContentHash::from_bytes(b"test"),
            span: Span::DUMMY,
        });

        let design = MappedDesign {
            modules,
            top: ModuleId::from_raw(0),
            types,
            resource_usage: ResourceUsage::default(),
        };

        (design, interner)
    }

    #[test]
    fn place_and_route_simple_design() {
        let (design, interner) = make_test_mapped_design();
        let arch = TestArch;
        let constraints = TimingConstraints::new();
        let sink = DiagnosticSink::new();

        let result = place_and_route(&design, &arch, &constraints, &interner, &sink);
        assert!(result.is_ok());

        let netlist = result.unwrap();
        assert!(netlist.is_fully_placed());
        assert!(netlist.is_fully_routed());
        assert!(netlist.cell_count() > 0);
    }

    #[test]
    fn place_and_route_empty_design() {
        let interner = Interner::new();
        let types = TypeDb::new();
        let mod_name = interner.get_or_intern("empty");

        let mut modules: Arena<ModuleId, MappedModule> = Arena::new();
        modules.alloc(MappedModule {
            id: ModuleId::from_raw(0),
            name: mod_name,
            ports: vec![],
            signals: Arena::new(),
            cells: Arena::new(),
            resource_usage: ResourceUsage::default(),
            content_hash: ContentHash::from_bytes(b"empty"),
            span: Span::DUMMY,
        });

        let design = MappedDesign {
            modules,
            top: ModuleId::from_raw(0),
            types,
            resource_usage: ResourceUsage::default(),
        };

        let arch = TestArch;
        let constraints = TimingConstraints::new();
        let sink = DiagnosticSink::new();

        let result = place_and_route(&design, &arch, &constraints, &interner, &sink);
        assert!(result.is_ok());
        let netlist = result.unwrap();
        assert_eq!(netlist.cell_count(), 0);
    }

    #[test]
    fn place_and_route_with_real_arch() {
        let (design, interner) = make_test_mapped_design();
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let constraints = TimingConstraints::new();
        let sink = DiagnosticSink::new();

        let result = place_and_route(&design, &*arch, &constraints, &interner, &sink);
        assert!(result.is_ok());
        let netlist = result.unwrap();
        assert!(netlist.is_fully_placed());
        assert!(netlist.is_fully_routed());
    }

    #[test]
    fn timing_graph_from_placed_netlist() {
        let (design, interner) = make_test_mapped_design();
        let arch = TestArch;
        let constraints = TimingConstraints::new();
        let sink = DiagnosticSink::new();

        let netlist = place_and_route(&design, &arch, &constraints, &interner, &sink).unwrap();
        let graph = build_timing_graph(&netlist, &arch);

        assert!(graph.node_count() > 0);
        assert!(graph.edge_count() > 0);
    }

    #[test]
    fn full_pipeline_with_timing() {
        let (design, interner) = make_test_mapped_design();
        let arch = TestArch;
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(aion_timing::ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 100.0, // generous for placed design with estimated net delays
            port: interner.get_or_intern("clk"),
            waveform: None,
        });
        let sink = DiagnosticSink::new();

        let netlist = place_and_route(&design, &arch, &constraints, &interner, &sink).unwrap();
        let graph = build_timing_graph(&netlist, &arch);
        let report = aion_timing::analyze_timing(&graph, &constraints, &interner, &sink).unwrap();

        // Design with zero-delay arch stubs should meet timing
        assert!(report.met);
    }

    #[test]
    fn reexports_available() {
        let _ = PnrNetlist::new();
        let _ = RouteTree::stub();
        let _ = PnrCellId::from_raw(0);
        let _ = PnrNetId::from_raw(0);
        let _ = PnrPinId::from_raw(0);
        let _ = PlacementCost::default();
    }

    #[test]
    fn serde_roundtrip_pnr_netlist() {
        let (design, interner) = make_test_mapped_design();
        let arch = TestArch;
        let constraints = TimingConstraints::new();
        let sink = DiagnosticSink::new();

        let netlist = place_and_route(&design, &arch, &constraints, &interner, &sink).unwrap();

        let json = serde_json::to_string(&netlist).unwrap();
        let mut restored: PnrNetlist = serde_json::from_str(&json).unwrap();
        restored.rebuild_indices();

        assert_eq!(restored.cell_count(), netlist.cell_count());
        assert_eq!(restored.net_count(), netlist.net_count());
        assert!(restored.is_fully_placed());
        assert!(restored.is_fully_routed());
    }
}
