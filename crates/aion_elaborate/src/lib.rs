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

    // --- Issue #30: member selects, fill literals, patterns, `++`/`--` ---

    /// Parses and elaborates `src` (top `top`) and returns the top module and
    /// every diagnostic as `(code, source text of its span)`.
    fn elaborate_top(
        file_name: &str,
        src: &str,
    ) -> (
        aion_ir::Module,
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
        } else {
            parsed.verilog_files.push(aion_verilog_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        }
        assert!(!sink.has_errors(), "parse errors: {:?}", sink.diagnostics());
        let design = elaborate(&parsed, &make_config("top"), &source_db, &interner, &sink).unwrap();
        let diags = sink
            .diagnostics()
            .into_iter()
            .map(|d| (d.code, source_db.snippet(d.primary_span).to_string()))
            .collect();
        (design.modules[design.top].clone(), diags)
    }

    /// Returns the values of every assignment in `stmt`, in order.
    fn assigned_values(stmt: &aion_ir::Statement) -> Vec<aion_ir::Expr> {
        let mut out = Vec::new();
        fn walk(s: &aion_ir::Statement, out: &mut Vec<aion_ir::Expr>) {
            match s {
                aion_ir::Statement::Assign { value, .. } => out.push(value.clone()),
                aion_ir::Statement::Block { stmts, .. } => stmts.iter().for_each(|s| walk(s, out)),
                aion_ir::Statement::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    walk(then_body, out);
                    if let Some(e) = else_body {
                        walk(e, out);
                    }
                }
                _ => {}
            }
        }
        walk(stmt, &mut out);
        out
    }

    /// Returns the single process body of `module`.
    fn only_body(module: &aion_ir::Module) -> aion_ir::Statement {
        assert_eq!(module.processes.len(), 1);
        module.processes.iter().next().unwrap().1.body.clone()
    }

    /// Returns the bit string (MSB first) of a literal expression.
    fn literal_bits(expr: &aion_ir::Expr) -> String {
        match expr {
            aion_ir::Expr::Literal(lv) => (0..lv.width())
                .rev()
                .map(|i| match lv.get(i) {
                    aion_common::Logic::Zero => '0',
                    aion_common::Logic::One => '1',
                    aion_common::Logic::X => 'x',
                    aion_common::Logic::Z => 'z',
                })
                .collect(),
            other => panic!("expected literal, got {other:?}"),
        }
    }

    fn codes_of(
        diags: &[(aion_diagnostics::DiagnosticCode, String)],
        code: aion_diagnostics::DiagnosticCode,
    ) -> Vec<String> {
        diags
            .iter()
            .filter(|(c, _)| *c == code)
            .map(|(_, s)| s.clone())
            .collect()
    }

    #[test]
    fn sv_member_on_index_is_diagnosed_not_dropped() {
        let (module, diags) = elaborate_top(
            "top.sv",
            "module top; logic arr; logic y; assign y = arr[2].valid; endmodule",
        );
        assert_eq!(codes_of(&diags, errors::E210), vec!["arr[2].valid"]);
        // The value is a poison literal, never the base select `arr[2]`.
        let value = &module.assignments[0].value;
        assert!(
            !matches!(value, aion_ir::Expr::Index { .. }),
            "member select must not fall back to its base: {value:?}"
        );
    }

    #[test]
    fn sv_member_lvalue_and_chain_are_diagnosed() {
        let (_, diags) = elaborate_top(
            "top.sv",
            "module top; logic d; logic clk; logic inst; logic a;\n\
             always_ff @(posedge clk) inst[0].port <= d;\n\
             always_comb a = inst[1].b[2].c; endmodule",
        );
        let e210 = codes_of(&diags, errors::E210);
        assert!(e210.contains(&"inst[0].port".to_string()), "{e210:?}");
        assert!(e210.contains(&"inst[1].b[2].c".to_string()), "{e210:?}");
    }

    #[test]
    fn verilog_member_on_index_is_diagnosed() {
        let (_, diags) = elaborate_top(
            "top.v",
            "module top; wire gen_blk; wire y; reg q; reg clk;\n\
             assign y = gen_blk[0].sig;\n\
             always @(posedge clk) gen_blk[1].r <= q; endmodule",
        );
        let e210 = codes_of(&diags, errors::E210);
        assert_eq!(e210, vec!["gen_blk[0].sig", "gen_blk[1].r"]);
    }

    #[test]
    fn sv_fill_literals_take_target_width() {
        let (module, diags) = elaborate_top(
            "top.sv",
            "module top; logic clk; logic rst; logic [7:0] q;\n\
             always_ff @(posedge clk) if (rst) q <= '0; else q <= '1; endmodule",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let values = assigned_values(&only_body(&module));
        assert_eq!(literal_bits(&values[0]), "00000000");
        assert_eq!(literal_bits(&values[1]), "11111111");
    }

    #[test]
    fn sv_fill_x_z_slices_concats_and_ternary() {
        let (module, diags) = elaborate_top(
            "top.sv",
            "module top; logic [7:0] q; logic [3:0] a; logic [1:0] b; logic s;\n\
             logic [7:0] d; logic [7:0] t;\n\
             always_comb begin q = 'x; q[3:0] = 'z; {a, b} = '1; end\n\
             assign t = s ? '1 : d; endmodule",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let values = assigned_values(&only_body(&module));
        assert_eq!(literal_bits(&values[0]), "xxxxxxxx");
        assert_eq!(literal_bits(&values[1]), "zzzz");
        assert_eq!(literal_bits(&values[2]), "111111");
        match &module.assignments[0].value {
            aion_ir::Expr::Ternary { true_val, .. } => {
                assert_eq!(literal_bits(true_val), "11111111")
            }
            other => panic!("expected ternary, got {other:?}"),
        }
    }

    #[test]
    fn sv_fill_one_without_context_width_is_diagnosed() {
        // `'1` in a comparison is sized by the other operand, which this
        // lowering cannot see: it must be reported, not lowered to 1'b1.
        let (_, diags) = elaborate_top(
            "top.sv",
            "module top; logic [7:0] q; logic y; logic [7:0] mem [0:3];\n\
             always_comb begin y = (q == '1); mem = '1; end endmodule",
        );
        let e210 = codes_of(&diags, errors::E210);
        assert_eq!(e210, vec!["'1", "'1"], "{diags:?}");
    }

    #[test]
    fn sv_fill_zero_without_context_is_one_bit_zero() {
        let (module, diags) = elaborate_top(
            "top.sv",
            "module top; logic [7:0] q; logic y; always_comb y = (q == '0); endmodule",
        );
        assert!(diags.is_empty(), "{diags:?}");
        match &assigned_values(&only_body(&module))[0] {
            aion_ir::Expr::Binary { rhs, .. } => assert_eq!(literal_bits(rhs), "0"),
            other => panic!("expected comparison, got {other:?}"),
        }
    }

    #[test]
    fn sv_assignment_pattern_default_and_positional() {
        let (module, diags) = elaborate_top(
            "top.sv",
            "module top; logic [7:0] q; logic [3:0] n; logic [3:0] m;\n\
             always_comb begin q = '{default: 1'b1}; n = '{1'b1, 1'b0, '1, 1}; \
             m = '{default: '0}; end endmodule",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let values = assigned_values(&only_body(&module));
        assert_eq!(literal_bits(&values[0]), "11111111");
        assert_eq!(literal_bits(&values[1]), "1011");
        assert_eq!(literal_bits(&values[2]), "0000");
    }

    #[test]
    fn sv_assignment_pattern_unsupported_shapes_are_diagnosed() {
        let (_, diags) = elaborate_top(
            "top.sv",
            "module top; logic [3:0] n; logic [7:0] mem [0:3]; logic v;\n\
             always_comb begin n = '{1, 0}; n = '{0: 1, default: 0}; \
             mem = '{default: 0}; n = '{default: v}; end endmodule",
        );
        assert_eq!(
            codes_of(&diags, errors::E205),
            vec!["'{1, 0}"],
            "item count mismatch: {diags:?}"
        );
        assert_eq!(
            codes_of(&diags, errors::E210),
            vec!["'{0: 1, default: 0}", "'{default: 0}", "'{default: v}"],
            "{diags:?}"
        );
    }

    /// Returns `true` if `expr` contains an IR negation.
    fn contains_neg(expr: &aion_ir::Expr) -> bool {
        match expr {
            aion_ir::Expr::Unary { op, operand, .. } => {
                *op == aion_ir::expr::UnaryOp::Neg || contains_neg(operand)
            }
            aion_ir::Expr::Binary { lhs, rhs, .. } => contains_neg(lhs) || contains_neg(rhs),
            _ => false,
        }
    }

    #[test]
    fn sv_postfix_increment_assigns_old_value_then_increments() {
        let (module, diags) = elaborate_top(
            "top.sv",
            "module top; logic [7:0] a; logic [7:0] b; initial a = b++; endmodule",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let body = only_body(&module);
        let aion_ir::Statement::Block { stmts, .. } = &body else {
            panic!("expected hoisted block, got {body:?}")
        };
        assert_eq!(stmts.len(), 2);
        let values = assigned_values(&body);
        // a = b (old value), then b = b + 1
        assert!(matches!(values[0], aion_ir::Expr::Signal(_)), "{values:?}");
        assert!(matches!(
            values[1],
            aion_ir::Expr::Binary {
                op: aion_ir::expr::BinaryOp::Add,
                ..
            }
        ));
        assert!(!values.iter().any(contains_neg), "{values:?}");
    }

    #[test]
    fn sv_prefix_decrement_runs_before_the_assignment() {
        let (module, diags) = elaborate_top(
            "top.sv",
            "module top; logic [7:0] a; logic [7:0] b; initial a = --b + 1; endmodule",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let values = assigned_values(&only_body(&module));
        assert_eq!(values.len(), 2);
        // b = b - 1 first, then a = b + 1
        assert!(matches!(
            values[0],
            aion_ir::Expr::Binary {
                op: aion_ir::expr::BinaryOp::Sub,
                ..
            }
        ));
        assert!(matches!(
            &values[1],
            aion_ir::Expr::Binary { op: aion_ir::expr::BinaryOp::Add, lhs, .. }
                if matches!(**lhs, aion_ir::Expr::Signal(_))
        ));
        assert!(!values.iter().any(contains_neg), "{values:?}");
    }

    #[test]
    fn sv_conditional_or_non_assignment_incr_is_diagnosed() {
        let (_, diags) = elaborate_top(
            "top.sv",
            "module top; logic [7:0] a; logic [7:0] b; logic c;\n\
             initial begin a = c ? b++ : 0; if (b-- > 1) a = 0; end endmodule",
        );
        assert_eq!(
            codes_of(&diags, errors::E210),
            vec!["b++", "b--"],
            "{diags:?}"
        );
    }

    #[test]
    fn sv_statement_incr_decr_still_works() {
        let (module, diags) = elaborate_top(
            "top.sv",
            "module top; logic [7:0] i; initial begin i++; --i; end endmodule",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let values = assigned_values(&only_body(&module));
        assert_eq!(values.len(), 2);
        assert!(!values.iter().any(contains_neg), "{values:?}");
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
}
