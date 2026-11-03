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
pub mod ranges;
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
        // `.Q(5)` targets a local parameter: reported (E214), value kept.
        let e214 = spans_with(&diags, errors::E214);
        assert_eq!(e214.len(), 1, "{diags:?}");
        assert!(e214[0].contains('Q'), "{e214:?}");
        assert_eq!(errors_of(&diags).len(), 1, "{diags:?}");
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
    fn overriding_local_parameters_reports_e214() {
        // Header `localparam` and body `localparam` (Verilog and SV), and an
        // SV body `parameter` in a module with a `#(...)` list, are all local.
        for (file, src) in [
            (
                "top.v",
                "module c #(parameter P = 1, localparam L = 2) (); localparam M = 3; endmodule
                 module top; c #(.P(4), .L(9), .M(9)) u(); endmodule",
            ),
            (
                "top.sv",
                "module c #(parameter int P = 1, localparam int L = 2) (); localparam int M = 3; endmodule
                 module top; c #(.P(4), .L(9), .M(9)) u(); endmodule",
            ),
        ] {
            let (design, interner, diags) = elaborate_src(file, src, "top");
            assert_eq!(spans_with(&diags, errors::E214).len(), 2, "{file}: {diags:?}");
            let c = module_named(&design, &interner, "c");
            assert_eq!(
                params_of(c, &interner),
                vec![
                    ("P".to_string(), ConstValue::Int(4)),
                    ("L".to_string(), ConstValue::Int(2)),
                    ("M".to_string(), ConstValue::Int(3)),
                ],
                "{file}"
            );
        }
        let (_, _, diags) = elaborate_src(
            "top.sv",
            "module c #(parameter int P = 1) (); parameter int Q = 2; endmodule
             module top; c #(.Q(5)) u(); endmodule",
            "top",
        );
        assert_eq!(spans_with(&diags, errors::E214).len(), 1, "{diags:?}");
    }

    #[test]
    fn legal_overrides_do_not_report_e214() {
        let (_, _, diags) = elaborate_src(
            "top.v",
            "module c #(parameter P = 1) (); endmodule
             module d; parameter W = 2; endmodule
             module top; c #(.P(4)) u1(); d #(.W(8)) u2(); endmodule",
            "top",
        );
        assert!(spans_with(&diags, errors::E214).is_empty(), "{diags:?}");
        assert!(errors_of(&diags).is_empty(), "{diags:?}");
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
    fn w205_with_fill_literal_and_postfix_incr() {
        // #35 (timing warning) and #30 (target-sized `'1`, hoisted `++`)
        // share the SV assignment lowering: both must still apply.
        use aion_ir::stmt::AssignKind;
        let src = "module tb; logic [7:0] q, a, b; \
                   initial begin q <= #1 '1; a = #2 b++; end endmodule";
        let (procs, w205) = elaborate_tb("tb.sv", src, errors::W205);
        assert_eq!(w205, vec!["#1", "#2"]);
        let (_, e210) = elaborate_tb("tb.sv", src, errors::E210);
        assert!(
            e210.is_empty(),
            "'1 and b++ must not be unsupported: {e210:?}"
        );
        let mut kinds = Vec::new();
        assign_kinds(&procs[0].body, &mut kinds);
        // q <= '1; a = b; b = b + 1 (hoisted after the assignment).
        assert_eq!(
            kinds,
            vec![
                AssignKind::NonBlocking,
                AssignKind::Blocking,
                AssignKind::Blocking
            ]
        );
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
        // VHDL basic identifiers are case-folded at intern time (#15), so the
        // generic declared as `N` is stored as `n`.
        assert_eq!(
            params_of(child, &interner),
            vec![("n".to_string(), ConstValue::Int(7))]
        );
    }

    #[test]
    fn vhdl_generic_type_range_attribute_reports_e212() {
        // A generic is the first declaration of an entity, so a range
        // attribute in its own type has nothing in scope to name (#20/#21
        // merge): it is reported as unresolvable rather than panicking or
        // silently picking a width.
        let src = "library ieee; use ieee.std_logic_1164.all;
            entity m is generic (G : std_logic_vector(x'range) := \"0\"); port (y : out integer); end entity;
            architecture rtl of m is begin end architecture;";
        let (_design, _interner, diags) = elaborate_src("m.vhd", src, "m");
        assert!(
            diags.iter().any(|(code, _)| *code == crate::errors::E212),
            "{diags:?}"
        );
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
