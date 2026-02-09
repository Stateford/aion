//! Synthesis engine for the Aion FPGA toolchain.
//!
//! This crate transforms elaborated AionIR (behavioral processes, concurrent
//! assignments) into a technology-mapped netlist of LUTs, FFs, BRAMs, and DSPs
//! ready for place-and-route.
//!
//! The synthesis pipeline has three phases:
//! 1. **Behavioral lowering** — converts processes and assignments to generic cells
//! 2. **Logic optimization** — constant propagation, dead code elimination, CSE
//! 3. **Technology mapping** — maps generic cells to device-specific primitives
//!
//! # Usage
//!
//! ```ignore
//! use aion_synth::synthesize;
//! let mapped = synthesize(&design, &*architecture, &config, &sink);
//! ```

#![warn(missing_docs)]

mod const_prop;
mod cse;
mod dce;
mod lower;
mod lower_expr;
mod netlist;
mod optimize;
mod resource;
mod tech_map;

use aion_arch::{Architecture, ResourceUsage};
use aion_common::{ContentHash, Ident, Interner};
use aion_config::OptLevel;
use aion_diagnostics::DiagnosticSink;
use aion_ir::{Arena, Cell, CellId, Design, Module, ModuleId, Port, Signal, SignalId, TypeDb};
use aion_source::Span;
use serde::{Deserialize, Serialize};

/// A fully synthesized and technology-mapped design.
///
/// Contains one [`MappedModule`] per module in the input design, with all
/// behavioral code lowered to cells and all generic cells mapped to
/// device-specific primitives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappedDesign {
    /// All mapped modules, indexed by [`ModuleId`].
    pub modules: Arena<ModuleId, MappedModule>,
    /// The top-level module ID.
    pub top: ModuleId,
    /// Shared type database.
    pub types: TypeDb,
    /// Total resource usage across all modules.
    pub resource_usage: ResourceUsage,
}

/// A single module after synthesis — all behavior lowered to cells.
///
/// Unlike the input [`Module`], a `MappedModule` has no processes or
/// assignments. All logic is represented as cells with signal connections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappedModule {
    /// The module ID (same as input).
    pub id: ModuleId,
    /// The module name.
    pub name: Ident,
    /// Module ports (preserved from input).
    pub ports: Vec<Port>,
    /// All signals in the module (original + synthesis temporaries).
    pub signals: Arena<SignalId, Signal>,
    /// All cells after synthesis and technology mapping.
    pub cells: Arena<CellId, Cell>,
    /// Resource usage for this module.
    pub resource_usage: ResourceUsage,
    /// Content hash of the synthesized module.
    pub content_hash: ContentHash,
    /// Source span for the module declaration.
    pub span: Span,
}

/// Synthesizes a design: behavioral lowering, optimization, and technology mapping.
///
/// Takes an elaborated [`Design`], the [`Interner`] used during elaboration,
/// an [`Architecture`] for the target device, the optimization level, and a
/// [`DiagnosticSink`] for warnings. Returns a [`MappedDesign`] with all
/// modules transformed to technology-mapped netlists.
pub fn synthesize(
    design: &Design,
    interner: &Interner,
    arch: &dyn Architecture,
    opt_level: &OptLevel,
    sink: &DiagnosticSink,
) -> MappedDesign {
    let mapper = arch.tech_mapper();

    let mut mapped_modules = Arena::new();
    let mut total_usage = ResourceUsage::default();

    for (_mod_id, module) in design.modules.iter() {
        let mapped = synthesize_module(module, &design.types, interner, &*mapper, opt_level, sink);
        total_usage.luts += mapped.resource_usage.luts;
        total_usage.ffs += mapped.resource_usage.ffs;
        total_usage.bram += mapped.resource_usage.bram;
        total_usage.dsp += mapped.resource_usage.dsp;
        total_usage.io += mapped.resource_usage.io;
        total_usage.pll += mapped.resource_usage.pll;
        mapped_modules.alloc(mapped);
    }

    MappedDesign {
        modules: mapped_modules,
        top: design.top,
        types: design.types.clone(),
        resource_usage: total_usage,
    }
}

/// Synthesizes a single module through all three phases.
fn synthesize_module(
    module: &Module,
    types: &TypeDb,
    interner: &Interner,
    mapper: &dyn aion_arch::TechMapper,
    opt_level: &OptLevel,
    sink: &DiagnosticSink,
) -> MappedModule {
    // Phase 1: Build mutable netlist and lower behavior to cells
    let mut nl = netlist::Netlist::from_module(module, types, interner);
    lower::lower_module(module, &mut nl, sink);

    // Phase 2: Run optimization passes (skip if opt_level demands minimum work)
    match opt_level {
        OptLevel::Area | OptLevel::Speed | OptLevel::Balanced => {
            optimize::run_passes(&mut nl, sink);
        }
    }

    // Phase 3: Technology mapping
    tech_map::tech_map(&mut nl, mapper, sink);

    // Count resources
    let usage = resource::count_resources(&nl);

    // Build output MappedModule
    // Filter out dead cells
    let mut out_cells: Arena<CellId, Cell> = Arena::new();
    for (cell_id, cell) in nl.cells.iter() {
        if !nl.is_dead(cell_id) {
            out_cells.alloc(cell.clone());
        }
    }

    MappedModule {
        id: module.id,
        name: module.name,
        ports: nl.ports.clone(),
        signals: nl.signals,
        cells: out_cells,
        resource_usage: usage,
        content_hash: ContentHash::from_bytes(b"synth"), // TODO: hash actual content
        span: module.span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_arch::{ArithmeticPattern, LutMapping, MapResult, MemoryCell, TechMapper};
    use aion_common::Interner;
    use aion_ir::{
        Arena, Assignment, BinaryOp, CellKind, Design, Edge, EdgeSensitivity, Expr, Module, Port,
        PortDirection, Process, ProcessId, ProcessKind, Sensitivity, Signal, SignalId, SignalKind,
        SignalRef, SourceMap, Statement, Type, TypeDb,
    };
    use aion_source::Span;

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
        fn map_cell(&self, cell_kind: &CellKind) -> MapResult {
            match cell_kind {
                CellKind::And { width } if *width == 1 => MapResult::Luts(vec![LutMapping {
                    input_count: 2,
                    init_bits: vec![0, 0, 0, 1],
                }]),
                CellKind::Or { width } if *width == 1 => MapResult::Luts(vec![LutMapping {
                    input_count: 2,
                    init_bits: vec![0, 1, 1, 1],
                }]),
                CellKind::Not { width } if *width == 1 => MapResult::Luts(vec![LutMapping {
                    input_count: 1,
                    init_bits: vec![1, 0],
                }]),
                CellKind::Dff { .. } => MapResult::Ff,
                CellKind::Mux { .. } => MapResult::Luts(vec![LutMapping {
                    input_count: 3,
                    init_bits: vec![0, 0, 1, 1, 0, 1, 0, 1],
                }]),
                CellKind::Eq { .. } => MapResult::Luts(vec![LutMapping {
                    input_count: 2,
                    init_bits: vec![1, 0, 0, 1],
                }]),
                _ => MapResult::Unmappable,
            }
        }
        fn infer_bram(&self, mem: &MemoryCell) -> bool {
            mem.depth <= 9216
        }
        fn infer_dsp(&self, pat: &ArithmeticPattern) -> bool {
            pat.width_a <= 18 && pat.width_b <= 18
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

    fn make_simple_design() -> (Design, Interner) {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);

        let clk_name = interner.get_or_intern("clk");
        let in_name = interner.get_or_intern("in_a");
        let out_name = interner.get_or_intern("out_b");
        let mod_name = interner.get_or_intern("top");

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

        // Sequential process: always_ff @(posedge clk) out_b <= in_a
        let mut processes = Arena::new();
        processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: clk_id,
                edge: Edge::Posedge,
            }]),
            body: Statement::Assign {
                target: SignalRef::Signal(out_id),
                value: Expr::Signal(SignalRef::Signal(in_id)),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(Module {
            id: ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports,
            signals,
            cells: Arena::new(),
            processes,
            assignments: vec![],
            clock_domains: vec![],
            content_hash: ContentHash::from_bytes(b"top"),
        });

        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        (design, interner)
    }

    fn make_combinational_design() -> (Design, Interner) {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);

        let a_name = interner.get_or_intern("a");
        let b_name = interner.get_or_intern("b");
        let y_name = interner.get_or_intern("y");
        let mod_name = interner.get_or_intern("comb");

        let mut signals = Arena::new();
        let a_id = signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: a_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let b_id = signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: b_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let y_id = signals.alloc(Signal {
            id: SignalId::from_raw(2),
            name: y_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let ports = vec![
            Port {
                id: aion_ir::PortId::from_raw(0),
                name: a_name,
                direction: PortDirection::Input,
                ty: bit_ty,
                signal: a_id,
                span: Span::DUMMY,
            },
            Port {
                id: aion_ir::PortId::from_raw(1),
                name: b_name,
                direction: PortDirection::Input,
                ty: bit_ty,
                signal: b_id,
                span: Span::DUMMY,
            },
            Port {
                id: aion_ir::PortId::from_raw(2),
                name: y_name,
                direction: PortDirection::Output,
                ty: bit_ty,
                signal: y_id,
                span: Span::DUMMY,
            },
        ];

        // assign y = a & b
        let assignments = vec![Assignment {
            target: SignalRef::Signal(y_id),
            value: Expr::Binary {
                op: BinaryOp::And,
                lhs: Box::new(Expr::Signal(SignalRef::Signal(a_id))),
                rhs: Box::new(Expr::Signal(SignalRef::Signal(b_id))),
                ty: types.intern(Type::Bit),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        }];

        let mut modules = Arena::new();
        modules.alloc(Module {
            id: ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports,
            signals,
            cells: Arena::new(),
            processes: Arena::new(),
            assignments,
            clock_domains: vec![],
            content_hash: ContentHash::from_bytes(b"comb"),
        });

        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        (design, interner)
    }

    #[test]
    fn synthesize_simple_register() {
        let (design, interner) = make_simple_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);

        assert_eq!(mapped.modules.len(), 1);
        let top = mapped.modules.get(mapped.top);
        assert!(!top.cells.is_empty(), "Should have synthesized cells");

        // Should have at least one DFF
        let has_dff = top
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Dff { .. }));
        assert!(has_dff, "Sequential design should have DFF");
    }

    #[test]
    fn synthesize_combinational_and() {
        let (design, interner) = make_combinational_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);

        let top = mapped.modules.get(mapped.top);
        assert!(!top.cells.is_empty(), "Should have synthesized cells");
    }

    #[test]
    fn synthesize_empty_module() {
        let interner = Interner::new();
        let types = TypeDb::new();
        let mod_name = interner.get_or_intern("empty");
        let mut modules = Arena::new();
        modules.alloc(Module {
            id: ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![],
            clock_domains: vec![],
            content_hash: ContentHash::from_bytes(b"empty"),
        });
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);

        let top = mapped.modules.get(mapped.top);
        assert!(top.cells.is_empty());
        assert_eq!(mapped.resource_usage.luts, 0);
        assert_eq!(mapped.resource_usage.ffs, 0);
    }

    #[test]
    fn synthesize_preserves_ports() {
        let (design, interner) = make_simple_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);

        let top = mapped.modules.get(mapped.top);
        assert_eq!(top.ports.len(), 3);
    }

    #[test]
    fn synthesize_has_resource_usage() {
        let (design, interner) = make_simple_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);

        // A simple register should use at least 1 FF
        // (exact count depends on lowering details)
        let top = mapped.modules.get(mapped.top);
        assert!(
            top.resource_usage.ffs > 0 || top.resource_usage.luts > 0,
            "Should use some resources"
        );
    }

    #[test]
    fn synthesize_multi_module() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);

        let mod1_name = interner.get_or_intern("mod1");
        let mod2_name = interner.get_or_intern("mod2");

        let mut modules = Arena::new();
        modules.alloc(Module {
            id: ModuleId::from_raw(0),
            name: mod1_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![],
            clock_domains: vec![],
            content_hash: ContentHash::from_bytes(b"m1"),
        });
        modules.alloc(Module {
            id: ModuleId::from_raw(1),
            name: mod2_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![],
            clock_domains: vec![],
            content_hash: ContentHash::from_bytes(b"m2"),
        });

        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);
        assert_eq!(mapped.modules.len(), 2);
    }

    #[test]
    fn mapped_design_serde_roundtrip() {
        let (design, interner) = make_combinational_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);

        let json = serde_json::to_string(&mapped).unwrap();
        let restored: MappedDesign = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.modules.len(), mapped.modules.len());
    }

    #[test]
    fn synthesize_with_area_opt() {
        let (design, interner) = make_simple_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Area, &sink);
        assert_eq!(mapped.modules.len(), 1);
    }

    #[test]
    fn synthesize_with_speed_opt() {
        let (design, interner) = make_simple_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Speed, &sink);
        assert_eq!(mapped.modules.len(), 1);
    }

    #[test]
    fn synthesize_io_count_from_ports() {
        let (design, interner) = make_combinational_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);

        // 2 inputs + 1 output = 3 IOs
        let top = mapped.modules.get(mapped.top);
        assert_eq!(top.resource_usage.io, 3);
    }

    #[test]
    fn synthesize_top_module_id_preserved() {
        let (design, interner) = make_simple_design();
        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);
        assert_eq!(mapped.top, ModuleId::from_raw(0));
    }

    #[test]
    fn synthesize_total_resources_aggregated() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);

        let mod_name = interner.get_or_intern("top");
        let a_name = interner.get_or_intern("a");
        let b_name = interner.get_or_intern("b");

        let mut signals = Arena::new();
        let a_id = signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: a_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let b_id = signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: b_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let ports = vec![
            Port {
                id: aion_ir::PortId::from_raw(0),
                name: a_name,
                direction: PortDirection::Input,
                ty: bit_ty,
                signal: a_id,
                span: Span::DUMMY,
            },
            Port {
                id: aion_ir::PortId::from_raw(1),
                name: b_name,
                direction: PortDirection::Output,
                ty: bit_ty,
                signal: b_id,
                span: Span::DUMMY,
            },
        ];

        let mut modules = Arena::new();
        modules.alloc(Module {
            id: ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports,
            signals,
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![Assignment {
                target: SignalRef::Signal(b_id),
                value: Expr::Signal(SignalRef::Signal(a_id)),
                span: Span::DUMMY,
            }],
            clock_domains: vec![],
            content_hash: ContentHash::from_bytes(b"top"),
        });

        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let arch = TestArch;
        let sink = DiagnosticSink::new();
        let mapped = synthesize(&design, &interner, &arch, &OptLevel::Balanced, &sink);

        // Total should equal the module's usage
        let top_usage = mapped.modules.get(mapped.top).resource_usage;
        assert_eq!(mapped.resource_usage.io, top_usage.io);
    }
}
