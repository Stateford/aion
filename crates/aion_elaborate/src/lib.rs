//! AST-to-AionIR elaboration engine.
//!
//! Transforms parsed HDL ASTs (Verilog-2005, SystemVerilog-2017, VHDL-2008) into
//! the unified `aion_ir` intermediate representation. Handles hierarchy resolution,
//! type resolution, parameter evaluation, and generate expansion.
//!
//! # Usage
//!
//! ```ignore
//! let design = elaborate(&parsed, &config, &source_db, &interner, &sink)?;
//! ```

#![warn(missing_docs)]

pub mod const_eval;
pub mod context;
pub mod errors;
pub mod expr;
pub mod registry;
pub mod stmt;
pub mod sv;
pub mod types;
pub mod verilog;
pub mod vhdl;

use aion_common::{AionResult, Interner};
use aion_config::ProjectConfig;
use aion_diagnostics::DiagnosticSink;
use aion_ir::Design;
use aion_source::SourceDb;
use aion_sv_parser::ast::SvSourceFile;
use aion_verilog_parser::ast::VerilogSourceFile;
use aion_vhdl_parser::ast::VhdlDesignFile;

use context::ElaborationContext;
use registry::{ModuleEntry, ModuleRegistry};

/// Collection of parsed source files from all supported HDL languages.
///
/// Constructed by the caller after parsing each source file, then passed
/// to [`elaborate`] for conversion to a unified IR [`Design`].
pub struct ParsedDesign {
    /// Parsed Verilog-2005 source files.
    pub verilog_files: Vec<VerilogSourceFile>,
    /// Parsed SystemVerilog-2017 source files.
    pub sv_files: Vec<SvSourceFile>,
    /// Parsed VHDL-2008 design files.
    pub vhdl_files: Vec<VhdlDesignFile>,
}

/// Elaborates parsed HDL sources into a unified IR [`Design`].
///
/// Builds a module registry from all parsed files, looks up the top-level
/// module from `config.project.top`, and recursively elaborates the hierarchy.
/// User-facing errors are emitted to `sink`; only internal compiler bugs
/// return `Err`.
pub fn elaborate(
    parsed: &ParsedDesign,
    config: &ProjectConfig,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> AionResult<Design> {
    let registry = ModuleRegistry::from_parsed_design(
        &parsed.verilog_files,
        &parsed.sv_files,
        &parsed.vhdl_files,
        interner,
        sink,
    );

    let top_name = interner.get_or_intern(&config.project.top);

    let entry = registry.lookup(top_name);
    if entry.is_none() {
        sink.emit(errors::error_top_not_found(
            &config.project.top,
            aion_source::Span::DUMMY,
        ));
        // Return a valid but empty design
        let ctx = ElaborationContext::new(&registry, interner, source_db, sink);
        return Ok(ctx.design);
    }

    let mut ctx = ElaborationContext::new(&registry, interner, source_db, sink);

    let top_mid = match entry.unwrap() {
        ModuleEntry::Verilog(decl) => verilog::elaborate_verilog_module(decl, &[], &mut ctx),
        ModuleEntry::Sv(decl) => sv::elaborate_sv_module(decl, &[], &mut ctx),
        ModuleEntry::Vhdl {
            entity,
            architecture,
        } => vhdl::elaborate_vhdl_entity(entity, architecture, &[], &mut ctx),
    };

    ctx.design.top = top_mid;
    Ok(ctx.design)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_ir::process::ProcessKind;
    use aion_source::{SourceDb, Span};
    use aion_sv_parser::ast as sv_ast;
    use aion_verilog_parser::ast as v_ast;
    use aion_vhdl_parser::ast as vhdl_ast;

    fn make_config(top: &str) -> ProjectConfig {
        let toml_str = format!(
            r#"
            [project]
            name = "test"
            version = "0.1.0"
            top = "{top}"
            "#
        );
        toml::from_str(&toml_str).unwrap()
    }

    /// Elaborates `child u(.a(x), .b());` from source and returns the names of
    /// the instance's connected ports.
    fn connected_ports(file_name: &str, src: &str) -> Vec<String> {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source(file_name, src.to_string());
        let mut parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![],
        };
        if file_name.ends_with(".sv") {
            parsed.sv_files.push(aion_sv_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else if file_name.ends_with(".vhd") {
            parsed.vhdl_files.push(aion_vhdl_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else {
            parsed.verilog_files.push(aion_verilog_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        }
        assert!(!sink.has_errors(), "parse errors: {:?}", sink.diagnostics());
        let design = elaborate(&parsed, &make_config("top"), &source_db, &interner, &sink).unwrap();
        let top = &design.modules[design.top];
        let (_, cell) = top.cells.iter().next().expect("one instance");
        let aion_ir::CellKind::Instance { .. } = cell.kind else {
            panic!("expected instance cell");
        };
        cell.connections
            .iter()
            .map(|c| interner.resolve(c.port_name).to_string())
            .collect()
    }

    #[test]
    fn verilog_unconnected_named_port_is_skipped() {
        let ports = connected_ports(
            "top.v",
            "module child(input wire a, output wire b); assign b = a; endmodule\n\
             module top; wire x; child u(.a(x), .b()); endmodule",
        );
        assert_eq!(ports, vec!["a".to_string()]);
    }

    #[test]
    fn sv_unconnected_named_port_is_skipped() {
        let ports = connected_ports(
            "top.sv",
            "module child(input logic a, output logic b); assign b = a; endmodule\n\
             module top; logic x; child u(.a(x), .b()); endmodule",
        );
        assert_eq!(ports, vec!["a".to_string()]);
    }

    /// Parses `src` as Verilog (`.v`) or SystemVerilog (`.sv`), elaborates it
    /// with top `tb`, and returns the top module's processes plus the source
    /// text of every diagnostic span with the given code.
    fn elaborate_tb(
        file_name: &str,
        src: &str,
        code: aion_diagnostics::DiagnosticCode,
    ) -> (Vec<aion_ir::Process>, Vec<String>) {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source(file_name, src.to_string());
        let mut parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![],
        };
        if file_name.ends_with(".sv") {
            parsed.sv_files.push(aion_sv_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else if file_name.ends_with(".vhd") {
            parsed.vhdl_files.push(aion_vhdl_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else {
            parsed.verilog_files.push(aion_verilog_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        }
        assert!(!sink.has_errors(), "parse errors: {:?}", sink.diagnostics());
        let design = elaborate(&parsed, &make_config("tb"), &source_db, &interner, &sink).unwrap();
        let procs = design.modules[design.top]
            .processes
            .iter()
            .map(|(_, p)| p.clone())
            .collect();
        let spans = sink
            .diagnostics()
            .into_iter()
            .filter(|d| d.code == code)
            .map(|d| source_db.snippet(d.primary_span).to_string())
            .collect();
        (procs, spans)
    }

    fn assert_empty_sensitivity(p: &aion_ir::Process) {
        assert!(
            matches!(&p.sensitivity, aion_ir::Sensitivity::SignalList(s) if s.is_empty()),
            "expected no sensitivity, got {:?}",
            p.sensitivity
        );
    }

    #[test]
    fn verilog_initial_block_has_empty_sensitivity() {
        let (procs, _) = elaborate_tb(
            "tb.v",
            "module tb; reg a; reg b; initial begin a = b; #1 a = ~b; end endmodule",
            errors::W203,
        );
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].kind, ProcessKind::Initial);
        assert_empty_sensitivity(&procs[0]);
    }

    #[test]
    fn sv_initial_block_has_empty_sensitivity() {
        let (procs, _) = elaborate_tb(
            "tb.sv",
            "module tb; logic a; logic b; initial a = b; endmodule",
            errors::W203,
        );
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].kind, ProcessKind::Initial);
        assert_empty_sensitivity(&procs[0]);
    }

    /// Asserts `p` is a looping `always` block: a `forever` loop around the
    /// original body, started once (combinational, no event control).
    fn assert_looping_always(p: &aion_ir::Process) -> &aion_ir::Statement {
        assert_eq!(p.kind, ProcessKind::Combinational);
        match &p.body {
            aion_ir::Statement::Forever { body, span } => {
                assert_eq!(*span, p.span, "loop spans the always block");
                body
            }
            other => panic!("expected Forever, got {other:?}"),
        }
    }

    #[test]
    fn verilog_always_without_event_control_is_a_loop() {
        let (procs, w203) = elaborate_tb(
            "tb.v",
            "module tb; reg clk; always #5 clk = ~clk; endmodule",
            errors::W203,
        );
        assert_eq!(procs.len(), 1);
        let body = assert_looping_always(&procs[0]);
        assert!(matches!(body, aion_ir::Statement::Delay { .. }), "{body:?}");
        assert!(w203.is_empty(), "{w203:?}");
    }

    #[test]
    fn verilog_always_block_with_two_delays_is_a_loop() {
        let (procs, w203) = elaborate_tb(
            "tb.v",
            "module tb; reg clk; always begin #5 clk = 0; #5 clk = 1; end endmodule",
            errors::W203,
        );
        let body = assert_looping_always(&procs[0]);
        assert!(
            matches!(body, aion_ir::Statement::Block { stmts, .. } if stmts.len() == 2),
            "{body:?}"
        );
        assert!(w203.is_empty(), "{w203:?}");
    }

    #[test]
    fn sv_always_without_event_control_is_a_loop() {
        let (procs, w203) = elaborate_tb(
            "tb.sv",
            "module tb; logic clk; always #5 clk = ~clk; endmodule",
            errors::W203,
        );
        let body = assert_looping_always(&procs[0]);
        assert!(matches!(body, aion_ir::Statement::Delay { .. }), "{body:?}");
        assert!(w203.is_empty(), "{w203:?}");
    }

    #[test]
    fn w203_verilog_always_without_any_timing_control() {
        let (procs, w203) = elaborate_tb(
            "tb.v",
            "module tb; reg clk; always clk = ~clk; endmodule",
            errors::W203,
        );
        assert_looping_always(&procs[0]);
        assert_eq!(w203.len(), 1, "{w203:?}");
        assert!(
            w203[0].starts_with("always"),
            "span covers the block: {w203:?}"
        );
    }

    #[test]
    fn w203_sv_always_without_any_timing_control() {
        let (_, w203) = elaborate_tb(
            "tb.sv",
            "module tb; logic a; logic b; always begin a = b; end endmodule",
            errors::W203,
        );
        assert_eq!(w203.len(), 1, "{w203:?}");
    }

    #[test]
    fn always_with_event_control_is_not_a_loop() {
        // Event-controlled and SV-specific always blocks keep their shape.
        let (procs, w203) = elaborate_tb(
            "tb.sv",
            "module tb; logic clk; logic d; logic q; logic y; \
             always @(posedge clk) begin q = d; #2 q = 0; end \
             always @(*) y = d; \
             always_comb y = d; \
             always_ff @(posedge clk) q <= d; \
             endmodule",
            errors::W203,
        );
        assert_eq!(procs.len(), 4);
        for p in &procs {
            assert!(
                !matches!(p.body, aion_ir::Statement::Forever { .. }),
                "{:?}",
                p.body
            );
        }
        assert_eq!(procs[0].kind, ProcessKind::Sequential);
        assert!(matches!(procs[1].sensitivity, aion_ir::Sensitivity::All));
        assert!(matches!(procs[2].sensitivity, aion_ir::Sensitivity::All));
        assert!(w203.is_empty(), "{w203:?}");
    }

    #[test]
    fn verilog_always_with_event_control_keeps_sensitivity() {
        let (procs, w203) = elaborate_tb(
            "tb.v",
            "module tb; reg clk; reg q; always @(posedge clk) q = ~q; endmodule",
            errors::W203,
        );
        assert_eq!(procs[0].kind, ProcessKind::Sequential);
        assert!(matches!(
            procs[0].sensitivity,
            aion_ir::Sensitivity::EdgeList(_)
        ));
        assert!(!matches!(procs[0].body, aion_ir::Statement::Forever { .. }));
        assert!(w203.is_empty());
    }

    /// Parses `src` as Verilog (`.v`) or SystemVerilog (`.sv`), elaborates it
    /// with top `tb`, and returns the source text of every W202 span.
    fn forever_warnings(file_name: &str, src: &str) -> Vec<String> {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source(file_name, src.to_string());
        let mut parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![],
        };
        if file_name.ends_with(".sv") {
            parsed.sv_files.push(aion_sv_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else if file_name.ends_with(".vhd") {
            parsed.vhdl_files.push(aion_vhdl_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else {
            parsed.verilog_files.push(aion_verilog_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        }
        assert!(!sink.has_errors(), "parse errors: {:?}", sink.diagnostics());
        elaborate(&parsed, &make_config("tb"), &source_db, &interner, &sink).unwrap();
        sink.diagnostics()
            .into_iter()
            .filter(|d| d.code == errors::W202)
            .map(|d| source_db.snippet(d.primary_span).to_string())
            .collect()
    }

    #[test]
    fn w202_verilog_forever_without_timing() {
        let w = forever_warnings(
            "tb.v",
            "module tb; reg clk; initial forever clk = ~clk; endmodule",
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].starts_with("forever"),
            "span should cover the loop: {w:?}"
        );
    }

    #[test]
    fn w202_verilog_nested_no_timing() {
        // Nested statements without timing control still never yield.
        let w = forever_warnings(
            "tb.v",
            "module tb; reg a; reg b; initial forever begin if (a) b = 1; else b = 0; end endmodule",
        );
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn w202_silent_on_verilog_delay_clock() {
        let w = forever_warnings(
            "tb.v",
            "module tb; reg clk; initial begin clk = 0; forever #5 clk = ~clk; end endmodule",
        );
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn w202_silent_on_verilog_event_control_and_finish() {
        let w = forever_warnings(
            "tb.v",
            "module tb; reg clk; reg q; \
             initial forever @(posedge clk) q = ~q; \
             initial forever begin if (q) $finish; end \
             endmodule",
        );
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn w202_sv_forever_without_timing() {
        let w = forever_warnings(
            "tb.sv",
            "module tb; logic a; initial forever a = ~a; endmodule",
        );
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn w202_silent_on_sv_break_and_delay() {
        let w = forever_warnings(
            "tb.sv",
            "module tb; logic [3:0] i; logic clk; \
             initial forever begin i++; if (i > 3) break; end \
             initial forever #5 clk = ~clk; \
             endmodule",
        );
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn elaborate_simple_verilog_counter() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("counter");

        let name = interner.get_or_intern("counter");
        let clk = interner.get_or_intern("clk");
        let q = interner.get_or_intern("q");

        let parsed = ParsedDesign {
            verilog_files: vec![v_ast::VerilogSourceFile {
                items: vec![v_ast::VerilogItem::Module(v_ast::ModuleDecl {
                    name,
                    port_style: v_ast::PortStyle::Ansi,
                    params: vec![],
                    ports: vec![
                        v_ast::PortDecl {
                            direction: v_ast::Direction::Input,
                            net_type: None,
                            signed: false,
                            range: None,
                            names: vec![clk],
                            span: Span::DUMMY,
                        },
                        v_ast::PortDecl {
                            direction: v_ast::Direction::Output,
                            net_type: None,
                            signed: false,
                            range: None,
                            names: vec![q],
                            span: Span::DUMMY,
                        },
                    ],
                    port_names: vec![],
                    items: vec![],
                    span: Span::DUMMY,
                })],
                span: Span::DUMMY,
            }],
            sv_files: vec![],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors());
        let top = &design.modules[design.top];
        assert_eq!(interner.resolve(top.name), "counter");
        assert_eq!(top.ports.len(), 2);
        assert_eq!(top.signals.len(), 2);
    }

    #[test]
    fn elaborate_verilog_hierarchy() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("top");

        let top_name = interner.get_or_intern("top");
        let sub_name = interner.get_or_intern("sub");

        let sub = v_ast::ModuleDecl {
            name: sub_name,
            port_style: v_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![],
            span: Span::DUMMY,
        };

        let top = v_ast::ModuleDecl {
            name: top_name,
            port_style: v_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![v_ast::ModuleItem::Instantiation(v_ast::Instantiation {
                module_name: sub_name,
                param_overrides: vec![],
                instances: vec![v_ast::Instance {
                    name: interner.get_or_intern("u0"),
                    range: None,
                    connections: vec![],
                    span: Span::DUMMY,
                }],
                span: Span::DUMMY,
            })],
            span: Span::DUMMY,
        };

        let parsed = ParsedDesign {
            verilog_files: vec![v_ast::VerilogSourceFile {
                items: vec![
                    v_ast::VerilogItem::Module(sub),
                    v_ast::VerilogItem::Module(top),
                ],
                span: Span::DUMMY,
            }],
            sv_files: vec![],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors());
        assert_eq!(design.modules.len(), 2);
        let top_mod = &design.modules[design.top];
        assert_eq!(top_mod.cells.len(), 1);
    }

    #[test]
    fn elaborate_sv_module_with_always_ff() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("test");

        let name = interner.get_or_intern("test");
        let clk = interner.get_or_intern("clk");

        let parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![sv_ast::SvSourceFile {
                items: vec![sv_ast::SvItem::Module(sv_ast::SvModuleDecl {
                    name,
                    port_style: sv_ast::PortStyle::Ansi,
                    params: vec![],
                    ports: vec![sv_ast::SvPortDecl {
                        direction: sv_ast::Direction::Input,
                        port_type: sv_ast::SvPortType::Var(sv_ast::VarType::Logic),
                        signed: false,
                        range: None,
                        names: vec![clk],
                        span: Span::DUMMY,
                    }],
                    port_names: vec![],
                    items: vec![sv_ast::ModuleItem::AlwaysFf(sv_ast::AlwaysFfBlock {
                        sensitivity: sv_ast::SensitivityList::List(vec![sv_ast::SensitivityItem {
                            edge: Some(sv_ast::EdgeKind::Posedge),
                            signal: sv_ast::Expr::Identifier {
                                name: clk,
                                span: Span::DUMMY,
                            },
                            span: Span::DUMMY,
                        }]),
                        body: sv_ast::Statement::Null { span: Span::DUMMY },
                        span: Span::DUMMY,
                    })],
                    end_label: None,
                    span: Span::DUMMY,
                })],
                span: Span::DUMMY,
            }],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors());
        let top = &design.modules[design.top];
        assert_eq!(top.processes.len(), 1);
        // Verify sequential process kind
        let proc = top.processes.iter().next().unwrap().1;
        assert!(matches!(proc.kind, ProcessKind::Sequential));
    }

    #[test]
    fn elaborate_vhdl_entity_with_arch() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("counter");

        let name = interner.get_or_intern("counter");
        let arch_name = interner.get_or_intern("rtl");
        let clk = interner.get_or_intern("clk");
        let std_logic = interner.get_or_intern("std_logic");

        let parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![vhdl_ast::VhdlDesignFile {
                units: vec![
                    vhdl_ast::DesignUnit::ContextUnit {
                        context: vec![],
                        unit: vhdl_ast::DesignUnitKind::Entity(vhdl_ast::EntityDecl {
                            name,
                            generics: None,
                            ports: Some(vhdl_ast::PortClause {
                                decls: vec![vhdl_ast::InterfaceDecl {
                                    names: vec![clk],
                                    mode: Some(vhdl_ast::PortMode::In),
                                    ty: vhdl_ast::TypeIndication {
                                        type_mark: vhdl_ast::SelectedName {
                                            parts: vec![std_logic],
                                            span: Span::DUMMY,
                                        },
                                        constraint: None,
                                        span: Span::DUMMY,
                                    },
                                    default: None,
                                    span: Span::DUMMY,
                                }],
                                span: Span::DUMMY,
                            }),
                            decls: vec![],
                            stmts: vec![],
                            span: Span::DUMMY,
                        }),
                        span: Span::DUMMY,
                    },
                    vhdl_ast::DesignUnit::ContextUnit {
                        context: vec![],
                        unit: vhdl_ast::DesignUnitKind::Architecture(vhdl_ast::ArchitectureDecl {
                            name: arch_name,
                            entity_name: name,
                            decls: vec![],
                            stmts: vec![],
                            span: Span::DUMMY,
                        }),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            }],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors());
        let top = &design.modules[design.top];
        assert_eq!(top.ports.len(), 1);
        assert_eq!(top.signals.len(), 1);
    }

    #[test]
    fn unknown_top_emits_e206() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("nonexistent");

        let parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(sink.has_errors());
        assert_eq!(design.modules.len(), 0);
    }

    #[test]
    fn unknown_instantiation_emits_e200() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("top");

        let top_name = interner.get_or_intern("top");
        let bad = interner.get_or_intern("nonexistent_module");
        let u0 = interner.get_or_intern("u0");

        let parsed = ParsedDesign {
            verilog_files: vec![v_ast::VerilogSourceFile {
                items: vec![v_ast::VerilogItem::Module(v_ast::ModuleDecl {
                    name: top_name,
                    port_style: v_ast::PortStyle::Empty,
                    params: vec![],
                    ports: vec![],
                    port_names: vec![],
                    items: vec![v_ast::ModuleItem::Instantiation(v_ast::Instantiation {
                        module_name: bad,
                        param_overrides: vec![],
                        instances: vec![v_ast::Instance {
                            name: u0,
                            range: None,
                            connections: vec![],
                            span: Span::DUMMY,
                        }],
                        span: Span::DUMMY,
                    })],
                    span: Span::DUMMY,
                })],
                span: Span::DUMMY,
            }],
            sv_files: vec![],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(sink.has_errors());
        // Should still elaborate successfully (with blackbox)
        assert_eq!(design.modules.len(), 1);
        let top = &design.modules[design.top];
        assert_eq!(top.cells.len(), 1);
    }

    #[test]
    fn mixed_language_hierarchy() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("top");

        let top_name = interner.get_or_intern("top");
        let sub_name = interner.get_or_intern("sv_sub");
        let u0 = interner.get_or_intern("u0");

        // SV sub module
        let sv_sub = sv_ast::SvModuleDecl {
            name: sub_name,
            port_style: sv_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![],
            end_label: None,
            span: Span::DUMMY,
        };

        // Verilog top instantiating SV sub
        let v_top = v_ast::ModuleDecl {
            name: top_name,
            port_style: v_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![v_ast::ModuleItem::Instantiation(v_ast::Instantiation {
                module_name: sub_name,
                param_overrides: vec![],
                instances: vec![v_ast::Instance {
                    name: u0,
                    range: None,
                    connections: vec![],
                    span: Span::DUMMY,
                }],
                span: Span::DUMMY,
            })],
            span: Span::DUMMY,
        };

        let parsed = ParsedDesign {
            verilog_files: vec![v_ast::VerilogSourceFile {
                items: vec![v_ast::VerilogItem::Module(v_top)],
                span: Span::DUMMY,
            }],
            sv_files: vec![sv_ast::SvSourceFile {
                items: vec![sv_ast::SvItem::Module(sv_sub)],
                span: Span::DUMMY,
            }],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors());
        assert_eq!(design.modules.len(), 2);
    }

    #[test]
    fn cache_reuse_for_same_params() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("top");

        let top_name = interner.get_or_intern("top");
        let sub_name = interner.get_or_intern("sub");
        let u0 = interner.get_or_intern("u0");
        let u1 = interner.get_or_intern("u1");

        let sub = v_ast::ModuleDecl {
            name: sub_name,
            port_style: v_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![],
            span: Span::DUMMY,
        };

        // Top instantiates sub twice with same params
        let top = v_ast::ModuleDecl {
            name: top_name,
            port_style: v_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![
                v_ast::ModuleItem::Instantiation(v_ast::Instantiation {
                    module_name: sub_name,
                    param_overrides: vec![],
                    instances: vec![v_ast::Instance {
                        name: u0,
                        range: None,
                        connections: vec![],
                        span: Span::DUMMY,
                    }],
                    span: Span::DUMMY,
                }),
                v_ast::ModuleItem::Instantiation(v_ast::Instantiation {
                    module_name: sub_name,
                    param_overrides: vec![],
                    instances: vec![v_ast::Instance {
                        name: u1,
                        range: None,
                        connections: vec![],
                        span: Span::DUMMY,
                    }],
                    span: Span::DUMMY,
                }),
            ],
            span: Span::DUMMY,
        };

        let parsed = ParsedDesign {
            verilog_files: vec![v_ast::VerilogSourceFile {
                items: vec![
                    v_ast::VerilogItem::Module(sub),
                    v_ast::VerilogItem::Module(top),
                ],
                span: Span::DUMMY,
            }],
            sv_files: vec![],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors());
        // Only 2 modules: top + sub (sub reused from cache)
        assert_eq!(design.modules.len(), 2);
        let top_mod = &design.modules[design.top];
        assert_eq!(top_mod.cells.len(), 2);
    }

    #[test]
    fn empty_design_top_not_found() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("any");

        let parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(sink.has_errors());
        assert_eq!(design.modules.len(), 0);
    }

    #[test]
    fn serde_roundtrip_of_design() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("test");

        let name = interner.get_or_intern("test");

        let parsed = ParsedDesign {
            verilog_files: vec![v_ast::VerilogSourceFile {
                items: vec![v_ast::VerilogItem::Module(v_ast::ModuleDecl {
                    name,
                    port_style: v_ast::PortStyle::Empty,
                    params: vec![],
                    ports: vec![],
                    port_names: vec![],
                    items: vec![],
                    span: Span::DUMMY,
                })],
                span: Span::DUMMY,
            }],
            sv_files: vec![],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors());

        // Serialize to JSON and back
        let json = serde_json::to_string(&design).unwrap();
        let _roundtrip: Design = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn elaborate_always_comb_produces_combinational() {
        let interner = Interner::new();
        let source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let config = make_config("test");

        let name = interner.get_or_intern("test");

        let parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![sv_ast::SvSourceFile {
                items: vec![sv_ast::SvItem::Module(sv_ast::SvModuleDecl {
                    name,
                    port_style: sv_ast::PortStyle::Empty,
                    params: vec![],
                    ports: vec![],
                    port_names: vec![],
                    items: vec![sv_ast::ModuleItem::AlwaysComb(sv_ast::AlwaysCombBlock {
                        body: sv_ast::Statement::Null { span: Span::DUMMY },
                        span: Span::DUMMY,
                    })],
                    end_label: None,
                    span: Span::DUMMY,
                })],
                span: Span::DUMMY,
            }],
            vhdl_files: vec![],
        };

        let design = elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors());
        let top = &design.modules[design.top];
        let proc = top.processes.iter().next().unwrap().1;
        assert!(matches!(proc.kind, ProcessKind::Combinational));
    }

    // ---- PAR-1: every parameter of a list is elaborated ----
    use aion_ir::ConstValue;
    // ---- PAR-21 / SV time literals: timing controls ----

    /// Parses and elaborates `src` with top `top`, returning the design, the
    /// interner, and every diagnostic as `(code, span text)`.
    fn elaborate_src(
        file_name: &str,
        src: &str,
        top: &str,
    ) -> (
        Design,
        Interner,
        Vec<(aion_diagnostics::DiagnosticCode, String)>,
    ) {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source(file_name, src.to_string());
        let mut parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![],
        };
        if file_name.ends_with(".sv") {
            parsed.sv_files.push(aion_sv_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else if file_name.ends_with(".vhd") {
            parsed.vhdl_files.push(aion_vhdl_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else {
            parsed.verilog_files.push(aion_verilog_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        }
        assert!(!sink.has_errors(), "parse errors: {:?}", sink.diagnostics());
        let design = elaborate(&parsed, &make_config(top), &source_db, &interner, &sink).unwrap();
        let diags = sink
            .diagnostics()
            .into_iter()
            .map(|d| (d.code, source_db.snippet(d.primary_span).to_string()))
            .collect();
        (design, interner, diags)
    }

    fn errors_of(diags: &[(aion_diagnostics::DiagnosticCode, String)]) -> Vec<String> {
        diags
            .iter()
            .filter(|(c, _)| c.category == aion_diagnostics::Category::Error)
            .map(|(c, s)| format!("{c}: {s}"))
            .collect()
    }

    fn spans_with(
        diags: &[(aion_diagnostics::DiagnosticCode, String)],
        code: aion_diagnostics::DiagnosticCode,
    ) -> Vec<String> {
        diags
            .iter()
            .filter(|(c, _)| *c == code)
            .map(|(_, s)| s.clone())
            .collect()
    }

    fn module_named<'a>(
        design: &'a Design,
        interner: &Interner,
        name: &str,
    ) -> &'a aion_ir::Module {
        design
            .modules
            .iter()
            .map(|(_, m)| m)
            .find(|m| interner.resolve(m.name) == name)
            .unwrap_or_else(|| panic!("no module `{name}`"))
    }

    fn params_of(m: &aion_ir::Module, interner: &Interner) -> Vec<(String, ConstValue)> {
        m.params
            .iter()
            .map(|p| (interner.resolve(p.name).to_string(), p.value.clone()))
            .collect()
    }

    fn signal_width(design: &Design, m: &aion_ir::Module, interner: &Interner, name: &str) -> u32 {
        let (_, sig) = m
            .signals
            .iter()
            .find(|(_, s)| interner.resolve(s.name) == name)
            .unwrap_or_else(|| panic!("no signal `{name}`"));
        design.types.bit_width(sig.ty).expect("sized type")
    }

    #[test]
    fn verilog_localparam_list_every_name_is_usable() {
        let src =
            "module top; localparam A = 1, B = 2, C = 4; wire [C-1:0] w; wire [A+B:0] v; endmodule";
        let (design, interner, diags) = elaborate_src("top.v", src, "top");
        assert!(errors_of(&diags).is_empty(), "{diags:?}");
        let top = design.top_module();
        assert_eq!(
            params_of(top, &interner),
            vec![
                ("A".to_string(), ConstValue::Int(1)),
                ("B".to_string(), ConstValue::Int(2)),
                ("C".to_string(), ConstValue::Int(4)),
            ]
        );
        assert_eq!(signal_width(&design, top, &interner, "w"), 4);
        assert_eq!(signal_width(&design, top, &interner, "v"), 4);
    }

    #[test]
    fn sv_localparam_list_every_name_is_usable() {
        let src = "module top; localparam int A = 1, B = A + 1, C = 4; logic [C-1:0] w; logic [B:0] v; endmodule";
        let (design, interner, diags) = elaborate_src("top.sv", src, "top");
        assert!(errors_of(&diags).is_empty(), "{diags:?}");
        let top = design.top_module();
        let names: Vec<_> = params_of(top, &interner).into_iter().collect();
        assert_eq!(names.len(), 3);
        assert_eq!(names[1], ("B".to_string(), ConstValue::Int(2)));
        assert_eq!(signal_width(&design, top, &interner, "w"), 4);
        assert_eq!(signal_width(&design, top, &interner, "v"), 3);
    }

    #[test]
    fn verilog_body_parameter_override_rules() {
        // Without a `#(...)` list, a body `parameter` is overridable and later
        // names see the override; with one, a body `parameter` is local.
        let src = "module child; parameter W = 2, X = W + 1; wire [X-1:0] o; endmodule
            module child2 #(parameter P = 1) (); parameter Q = 2; endmodule
            module top; child #(.W(8)) u1(); child2 #(.P(3), .Q(5)) u2(); endmodule";
        let (design, interner, diags) = elaborate_src("top.v", src, "top");
        assert!(errors_of(&diags).is_empty(), "{diags:?}");
        let child = module_named(&design, &interner, "child");
        assert_eq!(
            params_of(child, &interner),
            vec![
                ("W".to_string(), ConstValue::Int(8)),
                ("X".to_string(), ConstValue::Int(9)),
            ]
        );
        assert_eq!(signal_width(&design, child, &interner, "o"), 9);
        let child2 = module_named(&design, &interner, "child2");
        assert_eq!(
            params_of(child2, &interner),
            vec![
                ("P".to_string(), ConstValue::Int(3)),
                ("Q".to_string(), ConstValue::Int(2)),
            ]
        );
    }

    #[test]
    fn verilog_bad_body_parameter_still_reports_e209() {
        let (_, _, diags) = elaborate_src(
            "top.v",
            "module top; localparam A = 1, B = NOPE; endmodule",
            "top",
        );
        assert_eq!(spans_with(&diags, errors::E209), vec!["NOPE".to_string()]);
    }

    /// Collects the kind of every `Assign` in `stmt`, in order.
    fn assign_kinds(stmt: &aion_ir::Statement, out: &mut Vec<aion_ir::stmt::AssignKind>) {
        match stmt {
            aion_ir::Statement::Assign { kind, .. } => out.push(*kind),
            aion_ir::Statement::Block { stmts, .. } => {
                stmts.iter().for_each(|s| assign_kinds(s, out))
            }
            aion_ir::Statement::Delay { body, .. } => assign_kinds(body, out),
            _ => {}
        }
    }

    #[test]
    fn w205_intra_assignment_timing_is_reported_and_lowered() {
        use aion_ir::stmt::AssignKind;
        for (file, decl) in [
            ("tb.v", "reg q, d, a, b, clk;"),
            ("tb.sv", "logic q, d, a, b, clk;"),
        ] {
            let src = format!(
                "module tb; {decl} initial begin q <= #1 d; a = @(posedge clk) b; q <= d; end endmodule"
            );
            let (procs, spans) = elaborate_tb(file, &src, errors::W205);
            assert_eq!(spans, vec!["#1", "@(posedge clk)"], "{file}");
            let mut kinds = Vec::new();
            assign_kinds(&procs[0].body, &mut kinds);
            assert_eq!(
                kinds,
                vec![
                    AssignKind::NonBlocking,
                    AssignKind::Blocking,
                    AssignKind::NonBlocking
                ],
                "{file}"
            );
        }
    }

    #[test]
    fn w205_silent_without_intra_assignment_timing() {
        let (_, spans) = elaborate_tb(
            "tb.v",
            "module tb; reg q, d; initial begin #1 q <= d; q = d; end endmodule",
            errors::W205,
        );
        assert!(spans.is_empty(), "{spans:?}");
    }

    #[test]
    fn w206_net_and_assign_delays_are_reported() {
        for (file, decl) in [("top.v", "reg a;"), ("top.sv", "logic a;")] {
            let src = format!(
                "module top; {decl} wire #1 w; wire y; wire z; assign #(2, 3) y = a; assign z = a; endmodule"
            );
            let (design, _, diags) = elaborate_src(file, &src, "top");
            assert!(errors_of(&diags).is_empty(), "{diags:?}");
            assert_eq!(
                spans_with(&diags, errors::W206),
                vec!["#1", "#(2, 3)"],
                "{file}"
            );
            // Both continuous assignments are still elaborated.
            assert_eq!(design.top_module().assignments.len(), 2, "{file}");
        }
    }

    /// Collects every `Delay` duration in `stmt`, in order.
    fn delays_fs(stmt: &aion_ir::Statement, out: &mut Vec<u64>) {
        match stmt {
            aion_ir::Statement::Delay {
                duration_fs, body, ..
            } => {
                out.push(*duration_fs);
                delays_fs(body, out);
            }
            aion_ir::Statement::Block { stmts, .. } => stmts.iter().for_each(|s| delays_fs(s, out)),
            _ => {}
        }
    }

    #[test]
    fn sv_time_literal_delays_convert_to_femtoseconds() {
        let src = "module tb; logic x; initial begin #10ns x = 1; #(1.5ps) x = 0; #2us x = 1; \
                   #1step x = 0; #3 x = 1; end endmodule";
        let (procs, spans) = elaborate_tb("tb.sv", src, errors::W207);
        let mut fs = Vec::new();
        delays_fs(&procs[0].body, &mut fs);
        assert_eq!(fs, vec![10_000_000, 1_500, 2_000_000_000, 1, 3_000_000]);
        // Only `1step` depends on the unknown precision.
        assert_eq!(spans, vec!["1step"]);
    }

    #[test]
    fn sv_time_literal_out_of_range_is_e209() {
        let (_, spans) = elaborate_tb(
            "tb.sv",
            "module tb; logic x; initial #20000s x = 1; endmodule",
            errors::E209,
        );
        assert_eq!(spans, vec!["20000s"]);
    }

    #[test]
    fn sv_time_literal_value_counts_assumed_ns_units() {
        let src = "module tb; logic [63:0] t; initial begin t = 5ns; t = 100ps; end endmodule";
        let (procs, spans) = elaborate_tb("tb.sv", src, errors::W207);
        assert_eq!(spans, vec!["100ps"]);
        let aion_ir::Statement::Block { stmts, .. } = &procs[0].body else {
            panic!("expected block, got {:?}", procs[0].body);
        };
        let values: Vec<u64> = stmts
            .iter()
            .map(|s| match s {
                aion_ir::Statement::Assign {
                    value: aion_ir::Expr::Literal(lv),
                    ..
                } => (0..lv.width())
                    .filter(|&i| lv.get(i) == aion_common::Logic::One)
                    .map(|i| 1u64 << i)
                    .sum(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(values, vec![5, 0]);
    }

    // ---- Parameter / generic names used in expressions ----

    /// Returns `(width, value)` of an IR literal or named constant, panicking
    /// on anything else.
    fn lit(e: &aion_ir::Expr) -> (u32, u64) {
        match e {
            aion_ir::Expr::Literal(lv) | aion_ir::Expr::NamedConst { value: lv, .. } => {
                (lv.width(), lv.to_u64().expect("2-state literal"))
            }
            other => panic!("expected literal, got {other:?}"),
        }
    }

    /// Returns the value of the module's only continuous assignment.
    fn only_assign_value(m: &aion_ir::Module) -> &aion_ir::Expr {
        assert_eq!(m.assignments.len(), 1, "{:?}", m.assignments);
        &m.assignments[0].value
    }

    /// Returns the name of the signal an IR expression reads.
    fn signal_name<'a>(m: &aion_ir::Module, interner: &'a Interner, e: &aion_ir::Expr) -> &'a str {
        match e {
            aion_ir::Expr::Signal(aion_ir::SignalRef::Signal(sid)) => {
                interner.resolve(m.signals.get(*sid).name)
            }
            other => panic!("expected signal, got {other:?}"),
        }
    }

    /// Finds the first `Case` statement in `stmt`.
    fn find_case(stmt: &aion_ir::Statement) -> Option<&aion_ir::Statement> {
        match stmt {
            aion_ir::Statement::Case { .. } => Some(stmt),
            aion_ir::Statement::Block { stmts, .. } => stmts.iter().find_map(find_case),
            aion_ir::Statement::If {
                then_body,
                else_body,
                ..
            } => find_case(then_body).or_else(|| else_body.as_deref().and_then(find_case)),
            _ => None,
        }
    }

    #[test]
    fn body_localparam_in_comparison_lowers_to_literal() {
        let src = "module m(input [1:0] s, output y); localparam IDLE = 0, RUN = 1; \
                   assign y = (s == RUN); endmodule";
        for file in ["m.v", "m.sv"] {
            let (design, interner, diags) = elaborate_src(file, src, "m");
            assert!(errors_of(&diags).is_empty(), "{file}: {diags:?}");
            let top = design.top_module();
            let aion_ir::Expr::Binary { op, lhs, rhs, .. } = only_assign_value(top) else {
                panic!("{file}: expected binary");
            };
            assert_eq!(*op, aion_ir::BinaryOp::Eq, "{file}");
            assert_eq!(signal_name(top, &interner, lhs), "s", "{file}");
            // The reference keeps its name, so lint can tell it from a
            // magic number.
            assert!(
                matches!(rhs.as_ref(), aion_ir::Expr::NamedConst { name, .. }
                    if interner.resolve(*name) == "RUN"),
                "{file}: {rhs:?}"
            );
            // An untyped parameter with an unsized integer value is 32 bits.
            assert_eq!(lit(rhs), (32, 1), "{file}");
        }
    }

    #[test]
    fn header_parameter_in_expression_lowers_to_literal() {
        let src = "module m #(parameter W = 8) (output [7:0] y); assign y = W; endmodule";
        for file in ["m.v", "m.sv"] {
            let (design, _, diags) = elaborate_src(file, src, "m");
            assert!(errors_of(&diags).is_empty(), "{file}: {diags:?}");
            assert_eq!(
                lit(only_assign_value(design.top_module())),
                (32, 8),
                "{file}"
            );
        }
    }

    #[test]
    fn localparam_fsm_case_labels_and_next_state_lower_to_literals() {
        for (file, decl, always) in [
            ("m.v", "input clk", "always @(posedge clk)"),
            ("m.sv", "input logic clk", "always_ff @(posedge clk)"),
        ] {
            let src = format!(
                "module m({decl}); localparam [1:0] IDLE = 2'd0, RUN = 2'd1, DONE = 2'd2; \
                 reg [1:0] state; {always} case (state) IDLE: state <= RUN; \
                 RUN: state <= DONE; default: state <= IDLE; endcase endmodule"
            );
            let (design, interner, diags) = elaborate_src(file, &src, "m");
            assert!(errors_of(&diags).is_empty(), "{file}: {diags:?}");
            let top = design.top_module();
            let (_, proc) = top.processes.iter().next().expect("one process");
            let Some(aion_ir::Statement::Case {
                subject,
                arms,
                default,
                ..
            }) = find_case(&proc.body)
            else {
                panic!("{file}: no case in {:?}", proc.body);
            };
            assert_eq!(signal_name(top, &interner, subject), "state", "{file}");
            // Ranged parameters use their declared 2-bit width.
            let labels: Vec<_> = arms.iter().map(|a| lit(&a.patterns[0])).collect();
            assert_eq!(labels, vec![(2, 0), (2, 1)], "{file}");
            let next: Vec<_> = arms
                .iter()
                .map(|a| match &a.body {
                    aion_ir::Statement::Assign { value, .. } => lit(value),
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(next, vec![(2, 1), (2, 2)], "{file}");
            assert!(default.is_some(), "{file}");
        }
    }

    #[test]
    fn parameter_override_at_instantiation_is_used_in_expressions() {
        // A literal override and an override naming the parent's parameter.
        let src = "module child #(parameter W = 8) (output [7:0] y); assign y = W + 1; endmodule
            module top #(parameter P = 6) (output [7:0] a, output [7:0] b);
              child #(.W(4)) u1(.y(a)); child #(.W(P)) u2(.y(b)); endmodule";
        for file in ["top.v", "top.sv"] {
            let (design, _, diags) = elaborate_src(file, src, "top");
            assert!(errors_of(&diags).is_empty(), "{file}: {diags:?}");
            let mut sums: Vec<u64> = design
                .modules
                .iter()
                .filter(|(_, m)| !m.assignments.is_empty())
                .map(|(_, m)| match only_assign_value(m) {
                    aion_ir::Expr::Binary { lhs, .. } => lit(lhs).1,
                    other => panic!("{other:?}"),
                })
                .collect();
            sums.sort_unstable();
            assert_eq!(sums, vec![4, 6], "{file}");
        }
    }

    #[test]
    fn parameter_in_range_and_expression() {
        let src =
            "module m #(parameter W = 4) (input [W-1:0] a, output [W-1:0] y, output [W-1:0] z);
              assign y = a + W; assign z = a[W-1:0]; endmodule";
        for file in ["m.v", "m.sv"] {
            let (design, interner, diags) = elaborate_src(file, src, "m");
            assert!(errors_of(&diags).is_empty(), "{file}: {diags:?}");
            let top = design.top_module();
            assert_eq!(signal_width(&design, top, &interner, "a"), 4, "{file}");
            let aion_ir::Expr::Binary { rhs, .. } = &top.assignments[0].value else {
                panic!("{file}");
            };
            assert_eq!(lit(rhs), (32, 4), "{file}");
            let aion_ir::Expr::Slice { high, low, .. } = &top.assignments[1].value else {
                panic!("{file}: {:?}", top.assignments[1].value);
            };
            let aion_ir::Expr::Binary { lhs, .. } = high.as_ref() else {
                panic!("{file}: {high:?}");
            };
            assert_eq!(lit(lhs).1, 4, "{file}");
            assert_eq!(lit(low).1, 0, "{file}");
        }
    }

    #[test]
    fn parameter_index_on_assignment_target_selects_that_bit() {
        // One-hot style `state[RUN] <= 1` must write bit RUN, not the whole
        // vector, and `q[W-1:0]` must write the constant range.
        let src = "module m(input clk); localparam RUN = 2, W = 3; reg [3:0] state, q;
              always @(posedge clk) begin state[RUN] <= 1'b1; q[W-1:0] <= 3'b101; end endmodule";
        let (design, _, diags) = elaborate_src("m.v", src, "m");
        assert!(errors_of(&diags).is_empty(), "{diags:?}");
        let (_, proc) = design.top_module().processes.iter().next().unwrap();
        let mut targets = Vec::new();
        fn collect(s: &aion_ir::Statement, out: &mut Vec<aion_ir::SignalRef>) {
            match s {
                aion_ir::Statement::Assign { target, .. } => out.push(target.clone()),
                aion_ir::Statement::Block { stmts, .. } => {
                    stmts.iter().for_each(|s| collect(s, out))
                }
                _ => {}
            }
        }
        collect(&proc.body, &mut targets);
        let bounds: Vec<_> = targets
            .iter()
            .map(|t| match t {
                aion_ir::SignalRef::Slice { high, low, .. } => (*high, *low),
                other => panic!("expected slice, got {other:?}"),
            })
            .collect();
        assert_eq!(bounds, vec![(2, 2), (2, 0)]);
    }

    #[test]
    fn parameter_widths_follow_declared_type_or_value() {
        let cases = [
            ("m.v", "localparam [3:0] P = 1;", (4, 1)),
            ("m.v", "localparam P = 2'b10;", (2, 2)),
            ("m.v", "localparam P = 3'd1 + 2'd1;", (3, 2)),
            ("m.v", "localparam P = -1;", (32, 0xFFFF_FFFF)),
            ("m.sv", "localparam logic [5:0] P = 1;", (6, 1)),
            ("m.sv", "localparam int P = 7;", (32, 7)),
            ("m.sv", "localparam bit P = 1;", (1, 1)),
            ("m.sv", "localparam byte P = 3;", (8, 3)),
            ("m.sv", "localparam P = 4'hA;", (4, 10)),
        ];
        for (file, decl, expected) in cases {
            let src = format!("module m(output [63:0] y); {decl} assign y = P; endmodule");
            let (design, _, diags) = elaborate_src(file, &src, "m");
            assert!(errors_of(&diags).is_empty(), "{decl}: {diags:?}");
            assert_eq!(
                lit(only_assign_value(design.top_module())),
                expected,
                "{file}: {decl}"
            );
        }
    }

    #[test]
    fn parameter_in_replication_and_delay() {
        let src = "module m(output [3:0] y); localparam W = 4, D = 3; reg r;
              assign y = {W{1'b1}}; initial begin #D r = 1; #(D*2) r = 0; end endmodule";
        for file in ["m.v", "m.sv"] {
            let (design, _, diags) = elaborate_src(file, src, "m");
            assert!(errors_of(&diags).is_empty(), "{file}: {diags:?}");
            let top = design.top_module();
            let aion_ir::Expr::Repeat { count, .. } = only_assign_value(top) else {
                panic!("{file}");
            };
            assert_eq!(*count, 4, "{file}");
            let (_, proc) = top.processes.iter().next().unwrap();
            let mut fs = Vec::new();
            delays_fs(&proc.body, &mut fs);
            assert_eq!(fs, vec![3_000_000, 6_000_000], "{file}");
        }
    }

    #[test]
    fn signal_shadows_parameter_of_same_name() {
        let src = "module m(input A, output y); localparam A = 5; assign y = A; endmodule";
        for file in ["m.v", "m.sv"] {
            let (design, interner, diags) = elaborate_src(file, src, "m");
            assert!(errors_of(&diags).is_empty(), "{file}: {diags:?}");
            let top = design.top_module();
            assert_eq!(signal_name(top, &interner, only_assign_value(top)), "A");
        }
    }

    #[test]
    fn unknown_name_in_expression_still_reports_e204() {
        let src = "module m(input [1:0] s, output y); localparam RUN = 1; \
                   assign y = (s == NOPE) | (s == RUN); endmodule";
        for file in ["m.v", "m.sv"] {
            let (_, _, diags) = elaborate_src(file, src, "m");
            assert_eq!(spans_with(&diags, errors::E204), vec!["NOPE"], "{file}");
        }
    }

    #[test]
    fn vhdl_generic_in_expression_lowers_to_literal() {
        let src = "library ieee; use ieee.std_logic_1164.all;
            entity m is generic (W : integer := 3; V : std_logic_vector(3 downto 0));
              port (s : in integer; y : out std_logic); end entity;
            architecture rtl of m is begin
              process (s) begin if s = W then y <= '1'; elsif s = V then y <= '0'; end if; end process;
            end architecture;";
        let (design, _, diags) = elaborate_src("m.vhd", src, "m");
        assert!(errors_of(&diags).is_empty(), "{diags:?}");
        let (_, proc) = design.top_module().processes.iter().next().unwrap();
        let aion_ir::Statement::If {
            condition,
            else_body,
            ..
        } = &proc.body
        else {
            panic!("{:?}", proc.body);
        };
        let aion_ir::Expr::Binary { rhs, .. } = condition else {
            panic!("{condition:?}");
        };
        assert_eq!(lit(rhs), (32, 3));
        let Some(aion_ir::Statement::If {
            condition: aion_ir::Expr::Binary { rhs, .. },
            ..
        }) = else_body.as_deref()
        else {
            panic!("{else_body:?}");
        };
        // A vector generic lowers with its declared width. (Its default would
        // need string-literal const evaluation, which is not supported yet.)
        assert_eq!(lit(rhs), (4, 0));
    }

    #[test]
    fn vhdl_generic_map_can_name_parent_generic() {
        let src = "library ieee; use ieee.std_logic_1164.all;
            entity child is generic (N : integer := 1); port (y : out integer); end entity;
            architecture rtl of child is begin
              process (y) begin if y = N then end if; end process; end architecture;
            entity m is generic (P : integer := 7); port (y : out integer); end entity;
            architecture rtl of m is begin
              u : entity work.child generic map (N => P) port map (y => y); end architecture;";
        let (design, interner, diags) = elaborate_src("m.vhd", src, "m");
        assert!(errors_of(&diags).is_empty(), "{diags:?}");
        let child = module_named(&design, &interner, "child");
        assert_eq!(
            params_of(child, &interner),
            vec![("N".to_string(), ConstValue::Int(7))]
        );
    }
}
