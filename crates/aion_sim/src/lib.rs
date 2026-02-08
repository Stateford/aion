//! Event-driven HDL simulator for the Aion FPGA toolchain.
//!
//! This crate implements a delta-cycle-accurate simulation engine that
//! consumes an elaborated `Design` from `aion_ir` and executes it with
//! 4-state logic (IEEE 1164), multi-driver resolution, edge-sensitive
//! process scheduling, and VCD waveform output.
//!
//! # Architecture
//!
//! The simulator flattens the module hierarchy at construction time, mapping
//! all signals to flat `SimSignalId` identifiers. Processes are scheduled via
//! an event-driven loop with delta cycles for combinational propagation and
//! edge detection for sequential logic.
//!
//! # Usage
//!
//! ```ignore
//! use aion_sim::{simulate, SimConfig};
//!
//! let config = SimConfig::default();
//! let result = simulate(&design, &config, &interner)?;
//! println!("Simulation ended at {}", result.final_time);
//! ```
//!
//! # Modules
//!
//! - `error` — Simulation error types
//! - `time` — Femtosecond-precision time with delta cycles
//! - `value` — Signal state, driver resolution, drive strength
//! - `evaluator` — Expression evaluation and statement execution
//! - `waveform` — Waveform recording (VCD format)
//! - `kernel` — Simulation kernel with event queue and delta-cycle loop

#![warn(missing_docs)]

pub mod error;
pub mod evaluator;
pub mod fst;
pub mod interactive;
pub mod kernel;
pub mod time;
pub mod value;
pub mod waveform;

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use aion_common::Interner;
use aion_ir::Design;

pub use error::SimError;
pub use fst::FstRecorder;
pub use interactive::InteractiveSim;
pub use kernel::{SimKernel, SimResult, StepResult};
pub use time::SimTime;
pub use value::{DriveStrength, Driver, SimSignalId, SimSignalState};
pub use waveform::{VcdRecorder, WaveformRecorder};

/// Waveform output format selection.
///
/// Controls which waveform recorder implementation is used during simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveformOutputFormat {
    /// Value Change Dump (IEEE 1364) — human-readable text format.
    Vcd,
    /// Fast Signal Trace — compressed binary format (GTKWave native).
    Fst,
}

/// Configuration for a simulation run.
///
/// Controls time limits, waveform output, and other simulation parameters.
#[derive(Debug, Clone, Default)]
pub struct SimConfig {
    /// Optional simulation time limit in femtoseconds.
    /// If `None`, simulation runs until event queue empties or `$finish`.
    pub time_limit: Option<u64>,
    /// Optional path for waveform output.
    pub waveform_path: Option<PathBuf>,
    /// Whether to record waveform data. Ignored if `waveform_path` is `None`.
    pub record_waveform: bool,
    /// Waveform output format. Defaults to VCD if not specified.
    pub waveform_format: Option<WaveformOutputFormat>,
}

/// High-level entry point: runs a simulation on an elaborated design.
///
/// Creates a `SimKernel`, optionally attaches a VCD recorder, configures
/// time limits, and executes the simulation to completion. The `interner`
/// is used to resolve interned signal names for waveform output.
pub fn simulate(
    design: &Design,
    config: &SimConfig,
    interner: &Interner,
) -> Result<SimResult, SimError> {
    let mut kernel = SimKernel::new(design, interner)?;

    if let Some(limit) = config.time_limit {
        kernel.set_time_limit(limit);
    }

    if config.record_waveform {
        if let Some(path) = &config.waveform_path {
            let file = File::create(path)?;
            let writer = BufWriter::new(file);
            let format = config.waveform_format.unwrap_or(WaveformOutputFormat::Vcd);
            let recorder: Box<dyn WaveformRecorder> = match format {
                WaveformOutputFormat::Vcd => Box::new(VcdRecorder::new(writer)),
                WaveformOutputFormat::Fst => Box::new(FstRecorder::new(writer)),
            };
            kernel.set_recorder(recorder);
        }
    }

    if let Some(limit) = config.time_limit {
        kernel.run(limit)
    } else {
        kernel.run_to_completion()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, Interner, LogicVec};
    use aion_ir::arena::Arena;
    use aion_ir::{
        Assignment, CellKind, Connection, ConstValue, Design, Edge, EdgeSensitivity, Expr, Module,
        ModuleId, Port, PortDirection, Process, ProcessId, ProcessKind, Sensitivity, Signal,
        SignalId, SignalKind, SignalRef, SourceMap, Statement, Type, TypeDb,
    };
    use aion_source::Span;

    /// Creates a test interner pre-populated with names matching `Ident::from_raw()` indices.
    fn make_test_interner() -> Interner {
        let interner = Interner::new();
        interner.get_or_intern("__dummy__"); // 0
        interner.get_or_intern("top"); // 1
        interner.get_or_intern("clk"); // 2
        interner.get_or_intern("out"); // 3
        interner.get_or_intern("sel"); // 4
        interner.get_or_intern("rst"); // 5
        interner.get_or_intern("count"); // 6
        interner.get_or_intern("q"); // 7
        interner.get_or_intern("a"); // 8
        interner.get_or_intern("b"); // 9
        interner.get_or_intern("child"); // 10
        interner.get_or_intern("in_sig"); // 11
        interner.get_or_intern("out_sig"); // 12
        interner.get_or_intern("in_port"); // 13
        interner.get_or_intern("out_port"); // 14
        interner
    }

    fn make_type_db() -> TypeDb {
        let mut types = TypeDb::new();
        types.intern(Type::Bit); // 0: 1-bit
        types.intern(Type::BitVec {
            width: 8,
            signed: false,
        }); // 1: 8-bit
        types.intern(Type::BitVec {
            width: 4,
            signed: false,
        }); // 2: 4-bit
        types
    }

    fn empty_module(id: u32, name: Ident) -> Module {
        Module {
            id: ModuleId::from_raw(id),
            name,
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(b"test"),
        }
    }

    // ---- Integration tests ----

    #[test]
    fn sim_config_default() {
        let config = SimConfig::default();
        assert!(config.time_limit.is_none());
        assert!(config.waveform_path.is_none());
        assert!(!config.record_waveform);
        assert!(config.waveform_format.is_none());
    }

    #[test]
    fn simulate_empty_module() {
        let types = make_type_db();
        let top = empty_module(0, Ident::from_raw(1));
        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let config = SimConfig::default();
        let result = simulate(&design, &config, &make_test_interner()).unwrap();
        assert!(!result.finished_by_user);
    }

    #[test]
    fn simulate_combinational_chain() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));

        // Signal 0: a (input)
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(1)),
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Signal 1: b (input)
        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(1)),
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Signal 2: out
        top.signals.alloc(Signal {
            id: SignalId::from_raw(2),
            name: Ident::from_raw(4),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // assign out = a & b
        top.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(2)),
            value: Expr::Binary {
                op: aion_ir::BinaryOp::And,
                lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
                rhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
                ty: bit_ty,
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let config = SimConfig::default();
        let _result = simulate(&design, &config, &make_test_interner()).unwrap();

        // Create kernel to inspect values
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let _ = kernel.run_to_completion().unwrap();
        let out = kernel.find_signal("top.sel").unwrap();
        // a=1, b=1, out = 1&1 = 1
        assert_eq!(kernel.signal_value(out).to_u64(), Some(1));
    }

    #[test]
    fn simulate_counter() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);
        let bit4_ty = aion_ir::TypeId::from_raw(2);

        let mut top = empty_module(0, Ident::from_raw(1));

        // Signal 0: clk
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Signal 1: count (4-bit reg)
        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit4_ty,
            kind: SignalKind::Reg,
            init: Some(ConstValue::Int(0)),
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Sequential process: on posedge clk, count <= count + 1
        top.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(1)),
                value: Expr::Binary {
                    op: aion_ir::BinaryOp::Add,
                    lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
                    rhs: Box::new(Expr::Literal(LogicVec::from_u64(1, 4))),
                    ty: bit4_ty,
                    span: Span::DUMMY,
                },
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: SignalId::from_raw(0),
                edge: Edge::Posedge,
            }]),
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let clk_id = kernel.find_signal("top.clk").unwrap();

        // Generate 3 clock cycles
        for cycle in 0..3 {
            let t_rise = SimTime::from_ns(10 * cycle + 5);
            let t_fall = SimTime::from_ns(10 * cycle + 10);
            kernel.schedule_event(t_rise, clk_id, LogicVec::from_bool(true));
            kernel.schedule_event(t_fall, clk_id, LogicVec::from_bool(false));
        }

        let _result = kernel.run(50 * time::FS_PER_NS).unwrap();
        let count_id = kernel.find_signal("top.out").unwrap();
        // After 3 posedges, count should be 3
        assert_eq!(kernel.signal_value(count_id).to_u64(), Some(3));
    }

    #[test]
    fn simulate_finish_at_correct_time() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Initial: $display + $finish
        top.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Display {
                        format: "Hello, simulation!".into(),
                        args: Vec::new(),
                        span: Span::DUMMY,
                    },
                    Statement::Finish { span: Span::DUMMY },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let config = SimConfig::default();
        let result = simulate(&design, &config, &make_test_interner()).unwrap();
        assert!(result.finished_by_user);
        assert_eq!(result.display_output.len(), 1);
        assert_eq!(result.display_output[0], "Hello, simulation!");
    }

    #[test]
    fn simulate_assertion_failure() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        top.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Assertion {
                        kind: aion_ir::AssertionKind::Assert,
                        condition: Expr::Literal(LogicVec::from_bool(false)),
                        message: Some("expected true".into()),
                        span: Span::DUMMY,
                    },
                    Statement::Finish { span: Span::DUMMY },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let result = kernel.run_to_completion().unwrap();
        assert_eq!(result.assertion_failures.len(), 1);
        assert!(result.assertion_failures[0].contains("expected true"));
    }

    #[test]
    fn simulate_hierarchy() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        // Child: has input port, combinational assignment out = ~in
        let mut child = empty_module(1, Ident::from_raw(10));
        child.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(11),
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        child.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(12),
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        child.ports.push(Port {
            id: aion_ir::PortId::from_raw(0),
            name: Ident::from_raw(13), // "in_port"
            direction: PortDirection::Input,
            ty: bit_ty,
            signal: SignalId::from_raw(0),
            span: Span::DUMMY,
        });
        child.ports.push(Port {
            id: aion_ir::PortId::from_raw(1),
            name: Ident::from_raw(14), // "out_port"
            direction: PortDirection::Output,
            ty: bit_ty,
            signal: SignalId::from_raw(1),
            span: Span::DUMMY,
        });
        // assign out = ~in
        child.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(1)),
            value: Expr::Unary {
                op: aion_ir::UnaryOp::Not,
                operand: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
                ty: bit_ty,
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        });

        // Parent
        let mut parent = empty_module(0, Ident::from_raw(1));
        // Signal 0: wire_in
        parent.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(1)), // wire_in = 1
            clock_domain: None,
            span: Span::DUMMY,
        });
        // Signal 1: wire_out
        parent.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        parent.cells.alloc(aion_ir::Cell {
            id: aion_ir::CellId::from_raw(0),
            name: Ident::from_raw(4),
            kind: CellKind::Instance {
                module: ModuleId::from_raw(1),
                params: Vec::new(),
            },
            connections: vec![
                Connection {
                    port_name: Ident::from_raw(13), // in_port
                    direction: PortDirection::Input,
                    signal: SignalRef::Signal(SignalId::from_raw(0)),
                },
                Connection {
                    port_name: Ident::from_raw(14), // out_port
                    direction: PortDirection::Output,
                    signal: SignalRef::Signal(SignalId::from_raw(1)),
                },
            ],
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(parent);
        modules.alloc(child);

        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let _result = kernel.run_to_completion().unwrap();

        // wire_in = 1, child inverts, wire_out should be 0
        let out_id = kernel.find_signal("top.out").unwrap();
        assert_eq!(kernel.signal_value(out_id).to_u64(), Some(0));
    }

    #[test]
    fn simulate_vcd_output() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        // Test VCD recording via kernel directly
        let kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let sig = kernel.find_signal("top.clk").unwrap();

        let mut vcd_buf: Vec<u8> = Vec::new();
        // We can't easily set up the recorder without a reference issue,
        // so just test that VcdRecorder works standalone
        let mut rec = VcdRecorder::new(&mut vcd_buf);
        rec.begin_scope("top").unwrap();
        rec.register_signal(sig, "clk", 1).unwrap();
        rec.end_scope().unwrap();
        rec.record_change(0, sig, &LogicVec::from_bool(false))
            .unwrap();
        rec.record_change(5_000_000, sig, &LogicVec::from_bool(true))
            .unwrap();
        rec.finalize().unwrap();

        let output = String::from_utf8(vcd_buf).unwrap();
        assert!(output.contains("$timescale"));
        assert!(output.contains("#0"));
        assert!(output.contains("#5000000"));
    }

    #[test]
    fn simulate_if_else() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        // Signal 0: sel
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(1)), // sel = 1
            clock_domain: None,
            span: Span::DUMMY,
        });
        // Signal 1: out
        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Combinational process: if (sel) out = 1; else out = 0;
        top.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                }),
                else_body: Some(Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::from_bool(false)),
                    span: Span::DUMMY,
                })),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let _result = kernel.run_to_completion().unwrap();
        let out_id = kernel.find_signal("top.out").unwrap();
        assert_eq!(kernel.signal_value(out_id).to_u64(), Some(1));
    }

    #[test]
    fn simulate_with_time_limit() {
        let types = make_type_db();
        let top = empty_module(0, Ident::from_raw(1));
        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let config = SimConfig {
            time_limit: Some(100 * time::FS_PER_NS),
            waveform_path: None,
            record_waveform: false,
            waveform_format: None,
        };
        let result = simulate(&design, &config, &make_test_interner()).unwrap();
        assert!(!result.finished_by_user);
    }

    #[test]
    fn simulate_case_statement() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);
        let bit4_ty = aion_ir::TypeId::from_raw(2);

        let mut top = empty_module(0, Ident::from_raw(1));
        // Signal 0: sel (4-bit)
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit4_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(2)),
            clock_domain: None,
            span: Span::DUMMY,
        });
        // Signal 1: out (1-bit)
        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // case (sel) 2: out = 1; default: out = 0;
        top.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Case {
                subject: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                arms: vec![aion_ir::CaseArm {
                    patterns: vec![Expr::Literal(LogicVec::from_u64(2, 4))],
                    body: Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(1)),
                        value: Expr::Literal(LogicVec::from_bool(true)),
                        span: Span::DUMMY,
                    },
                    span: Span::DUMMY,
                }],
                default: Some(Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::from_bool(false)),
                    span: Span::DUMMY,
                })),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let _ = kernel.run_to_completion().unwrap();
        let out_id = kernel.find_signal("top.out").unwrap();
        assert_eq!(kernel.signal_value(out_id).to_u64(), Some(1));
    }
}
