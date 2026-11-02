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

mod binding;
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

    // VHDL entity names are stored case-folded, so `top = "Top"` must still
    // find `entity top` (exact matches, e.g. Verilog modules, win first).
    let top_name = registry.canonical_name(interner.get_or_intern(&config.project.top), interner);

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

    // ========================================================================
    // VHDL identifier case-insensitivity (issue #15)
    // ========================================================================

    /// Parses every `(file_name, source)` pair (language chosen by extension)
    /// and elaborates them with top `top`. Returns the design, the interner,
    /// and all diagnostics (parse + elaboration).
    fn elaborate_sources(
        files: &[(&str, &str)],
        top: &str,
    ) -> (Design, Interner, Vec<aion_diagnostics::Diagnostic>) {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let mut parsed = ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![],
        };
        for (file_name, src) in files {
            let file_id = source_db.add_source(file_name, src.to_string());
            if file_name.ends_with(".vhd") {
                parsed.vhdl_files.push(aion_vhdl_parser::parse_file(
                    file_id, &source_db, &interner, &sink,
                ));
            } else if file_name.ends_with(".sv") {
                parsed.sv_files.push(aion_sv_parser::parse_file(
                    file_id, &source_db, &interner, &sink,
                ));
            } else {
                parsed.verilog_files.push(aion_verilog_parser::parse_file(
                    file_id, &source_db, &interner, &sink,
                ));
            }
        }
        let design = elaborate(&parsed, &make_config(top), &source_db, &interner, &sink).unwrap();
        let diags = sink.diagnostics();
        (design, interner, diags)
    }

    /// Returns the messages of all error diagnostics.
    fn error_messages(diags: &[aion_diagnostics::Diagnostic]) -> Vec<String> {
        diags
            .iter()
            .filter(|d| d.severity == aion_diagnostics::Severity::Error)
            .map(|d| d.message.clone())
            .collect()
    }

    const MIXED_CASE_VHDL: &str = r#"
library IEEE;
use IEEE.STD_LOGIC_1164.ALL;

entity Top is
  port (
    Clk : in  STD_LOGIC;
    Rst : in  std_logic;
    Q   : out Std_Logic
  );
end entity TOP;

architecture Rtl of TOP is
  signal Reg_Q : STD_LOGIC;
begin
  Main : process (clk)
  begin
    if RISING_EDGE(CLK) then
      if rst = '1' then
        reg_q <= '0';
      else
        REG_Q <= not Reg_q;
      end if;
    end if;
  end process MAIN;

  q <= reg_q;
end architecture rtl;
"#;

    #[test]
    fn vhdl_mixed_case_declaration_and_use_resolve() {
        // `Clk` declared, `clk`/`CLK` used; `architecture ... of TOP` binds to
        // `entity Top`. Previously every use was an unknown-signal error.
        let (design, interner, diags) = elaborate_sources(&[("top.vhd", MIXED_CASE_VHDL)], "top");
        let errors = error_messages(&diags);
        assert!(errors.is_empty(), "errors: {errors:?}");

        let top = &design.modules[design.top];
        assert_eq!(interner.resolve(top.name), "top");
        let ports: Vec<_> = top.ports.iter().map(|p| interner.resolve(p.name)).collect();
        assert_eq!(ports, vec!["clk", "rst", "q"]);
        // One clocked process sensitive to the (single) clk signal.
        assert_eq!(top.processes.len(), 1);
        let (_, proc) = top.processes.iter().next().unwrap();
        assert!(matches!(proc.kind, ProcessKind::Sequential));
    }

    #[test]
    fn vhdl_case_variant_top_name_finds_entity() {
        // `top = "TOP"` in aion.toml must find the (case-folded) VHDL entity.
        let (design, interner, diags) = elaborate_sources(&[("top.vhd", MIXED_CASE_VHDL)], "TOP");
        let errors = error_messages(&diags);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(interner.resolve(design.modules[design.top].name), "top");
    }

    #[test]
    fn vhdl_undeclared_name_still_reports_unknown_signal() {
        // Case folding must not make genuinely unknown names resolve.
        let src = r#"
library ieee;
use ieee.std_logic_1164.all;
entity Top is
  port (Clk : in std_logic; Q : out std_logic);
end entity;
architecture rtl of top is
begin
  Q <= Clock;
end architecture;
"#;
        let (_, _, diags) = elaborate_sources(&[("top.vhd", src)], "top");
        let errors = error_messages(&diags);
        assert!(
            errors.iter().any(|m| m.contains("unknown signal `clock`")),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn verilog_instantiates_vhdl_entity_case_insensitively() {
        let vhdl = r#"
library ieee;
use ieee.std_logic_1164.all;
entity My_Child is
  port (A : in std_logic);
end entity;
architecture rtl of MY_CHILD is
begin
end architecture;
"#;
        let verilog = "module tb; reg a; My_Child u0(.a(a)); endmodule\n";
        let (design, _, diags) = elaborate_sources(&[("child.vhd", vhdl), ("tb.v", verilog)], "tb");
        let errors = error_messages(&diags);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(design.modules.len(), 2);
    }

    #[test]
    fn vhdl_instantiates_verilog_module_case_insensitively() {
        let verilog = "module Sub_Mod(input a); endmodule\n";
        let vhdl = r#"
library ieee;
use ieee.std_logic_1164.all;
entity top is
end entity;
architecture rtl of top is
  signal s : std_logic;
begin
  u0 : entity work.Sub_Mod port map (a => s);
end architecture;
"#;
        let (design, _, diags) = elaborate_sources(&[("sub.v", verilog), ("top.vhd", vhdl)], "top");
        let errors = error_messages(&diags);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(design.modules.len(), 2);
    }

    #[test]
    fn vhdl_instantiation_of_ambiguous_verilog_case_variants_is_unknown() {
        // `Foo_X` and `FOO_X` are distinct Verilog modules; the case-insensitive
        // VHDL reference `foo_x` cannot pick one, so it stays unresolved (E200).
        let verilog = "module Foo_X; endmodule\nmodule FOO_X; endmodule\n";
        let vhdl = r#"
entity top is
end entity;
architecture rtl of top is
begin
  u0 : entity work.foo_x;
end architecture;
"#;
        let (_, _, diags) = elaborate_sources(&[("m.v", verilog), ("top.vhd", vhdl)], "top");
        let errors = error_messages(&diags);
        assert!(
            errors.iter().any(|m| m.contains("unknown module `foo_x`")),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn vhdl_instantiation_prefers_exact_folded_verilog_match() {
        // When a Verilog module is spelled exactly like the folded VHDL name,
        // that exact match wins over other case variants.
        let verilog = "module sub; endmodule\nmodule SUB; endmodule\n";
        let vhdl = r#"
entity top is
end entity;
architecture rtl of top is
begin
  u0 : entity work.Sub;
end architecture;
"#;
        let (design, interner, diags) =
            elaborate_sources(&[("m.v", verilog), ("top.vhd", vhdl)], "top");
        let errors = error_messages(&diags);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let names: Vec<_> = design
            .modules
            .iter()
            .map(|(_, m)| interner.resolve(m.name).to_string())
            .collect();
        assert!(names.contains(&"sub".to_string()), "{names:?}");
        assert!(!names.contains(&"SUB".to_string()), "{names:?}");
    }

    #[test]
    fn verilog_module_names_stay_case_sensitive() {
        // Verilog is case-sensitive: `top = "TB"` must not find `module tb`.
        let (_, _, diags) = elaborate_sources(&[("tb.v", "module tb; endmodule\n")], "TB");
        let errors = error_messages(&diags);
        assert!(
            errors.iter().any(|m| m.contains("`TB` not found")),
            "errors: {errors:?}"
        );
    }

    const EXTENDED_TOP_VHDL: &str = r#"
entity \Top\ is
end entity;
architecture rtl of \Top\ is
begin
end architecture;
"#;

    #[test]
    fn extended_entity_name_binds_only_exactly() {
        // `\Top\` is case-sensitive: the exact spelling is found...
        let (design, interner, diags) =
            elaborate_sources(&[("top.vhd", EXTENDED_TOP_VHDL)], "\\\\Top\\\\");
        let errors = error_messages(&diags);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(interner.resolve(design.modules[design.top].name), "\\Top\\");
        // ...but `\TOP\` is a different name.
        let (_, _, diags) = elaborate_sources(&[("top.vhd", EXTENDED_TOP_VHDL)], "\\\\TOP\\\\");
        let errors = error_messages(&diags);
        assert!(
            errors.iter().any(|m| m.contains("not found")),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn vhdl_instantiation_of_extended_case_variant_is_unknown() {
        let tb = r#"
entity tb is
end entity;
architecture sim of tb is
begin
  u0 : entity work.\TOP\;
end architecture;
"#;
        let (_, _, diags) =
            elaborate_sources(&[("top.vhd", EXTENDED_TOP_VHDL), ("tb.vhd", tb)], "tb");
        let errors = error_messages(&diags);
        assert!(
            errors
                .iter()
                .any(|m| m.contains("unknown module `\\TOP\\`")),
            "errors: {errors:?}"
        );
    }

    /// Returns `(port name, direction)` of every connection on the single
    /// instance cell of the top module.
    fn instance_connections(
        design: &Design,
        interner: &Interner,
    ) -> Vec<(String, aion_ir::PortDirection)> {
        let top = &design.modules[design.top];
        let (_, cell) = top
            .cells
            .iter()
            .find(|(_, c)| matches!(c.kind, aion_ir::CellKind::Instance { .. }))
            .expect("instance cell");
        cell.connections
            .iter()
            .map(|c| (interner.resolve(c.port_name).to_string(), c.direction))
            .collect()
    }

    const VHDL_CHILD: &str = r#"
library ieee;
use ieee.std_logic_1164.all;
entity child is
  generic (Width : integer := 1);
  port (Clk : in std_logic; Q : out std_logic);
end entity;
architecture rtl of child is
begin
end architecture;
"#;

    #[test]
    fn verilog_named_ports_bind_to_folded_vhdl_ports() {
        let tb = "module tb; reg c; wire q; child #(.Width(4)) u0(.Clk(c), .Q(q)); endmodule\n";
        for file in ["tb.v", "tb.sv"] {
            let (design, interner, diags) =
                elaborate_sources(&[("child.vhd", VHDL_CHILD), (file, tb)], "tb");
            let errors = error_messages(&diags);
            assert!(errors.is_empty(), "{file}: errors: {errors:?}");
            assert_eq!(
                instance_connections(&design, &interner),
                vec![
                    ("clk".to_string(), aion_ir::PortDirection::Input),
                    ("q".to_string(), aion_ir::PortDirection::Output),
                ],
                "{file}"
            );
            // The `Width` override reached the VHDL generic `width`.
            let child = design
                .modules
                .iter()
                .map(|(_, m)| m)
                .find(|m| interner.resolve(m.name) == "child")
                .expect("child module");
            assert_eq!(interner.resolve(child.params[0].name), "width");
            assert_eq!(child.params[0].value, aion_ir::ConstValue::Int(4), "{file}");
        }
    }

    #[test]
    fn verilog_unknown_port_on_vhdl_child_is_reported() {
        let tb = "module tb; reg c; child u0(.Clock(c)); endmodule\n";
        let (design, interner, diags) =
            elaborate_sources(&[("child.vhd", VHDL_CHILD), ("tb.v", tb)], "tb");
        let errors = error_messages(&diags);
        assert!(
            errors
                .iter()
                .any(|m| m.contains("unknown port `Clock` on module `child`")),
            "errors: {errors:?}"
        );
        assert!(instance_connections(&design, &interner).is_empty());
    }

    const VERILOG_CHILD: &str =
        "module Child #(parameter Width = 1) (input Clk, output Q); endmodule\n";

    #[test]
    fn vhdl_port_map_binds_to_case_sensitive_verilog_ports() {
        // The VHDL formals `Clk`/`Q` are folded to `clk`/`q`, yet must bind to
        // the Verilog ports `Clk`/`Q`.
        let tb = r#"
library ieee;
use ieee.std_logic_1164.all;
entity tb is
end entity;
architecture sim of tb is
  signal c, q : std_logic;
begin
  u0 : entity work.Child generic map (Width => 4) port map (Clk => c, Q => q);
end architecture;
"#;
        let (design, interner, diags) =
            elaborate_sources(&[("child.v", VERILOG_CHILD), ("tb.vhd", tb)], "tb");
        let errors = error_messages(&diags);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(
            instance_connections(&design, &interner),
            vec![
                ("Clk".to_string(), aion_ir::PortDirection::Input),
                ("Q".to_string(), aion_ir::PortDirection::Output),
            ]
        );
        let child = design
            .modules
            .iter()
            .map(|(_, m)| m)
            .find(|m| interner.resolve(m.name) == "Child")
            .expect("child module");
        assert_eq!(child.params[0].value, aion_ir::ConstValue::Int(4));
    }

    #[test]
    fn vhdl_port_map_to_ambiguous_verilog_ports_is_reported() {
        let child = "module amb(input Ab, input AB); endmodule\n";
        let tb = r#"
library ieee;
use ieee.std_logic_1164.all;
entity tb is
end entity;
architecture sim of tb is
  signal c : std_logic;
begin
  u0 : entity work.amb port map (ab => c);
end architecture;
"#;
        let (design, interner, diags) =
            elaborate_sources(&[("amb.v", child), ("tb.vhd", tb)], "tb");
        let errors = error_messages(&diags);
        assert!(
            errors.iter().any(|m| m.contains("port `ab` is ambiguous")),
            "errors: {errors:?}"
        );
        assert!(instance_connections(&design, &interner).is_empty());
    }

    #[test]
    fn verilog_to_verilog_ports_stay_case_sensitive() {
        // `.clk(x)` must not bind to `Clk`: the formal is kept as written and
        // its direction falls back as before (no case-insensitive match).
        let src = "module Child(output Clk); endmodule\n\
                   module tb; wire x; Child u0(.clk(x)); endmodule\n";
        let (design, interner, _) = elaborate_sources(&[("tb.v", src)], "tb");
        assert_eq!(
            instance_connections(&design, &interner),
            vec![("clk".to_string(), aion_ir::PortDirection::Input)]
        );
    }
}
