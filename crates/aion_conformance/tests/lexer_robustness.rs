//! End-to-end lexer robustness tests (issue #34): non-ASCII input, a trailing
//! backslash in a string at EOF, LRM whitespace in sized literals and escaped
//! identifiers. Every diagnostic is rendered with `TerminalRenderer`, which
//! used to panic on spans inside a UTF-8 character or past EOF.

use aion_common::Interner;
use aion_conformance::{full_pipeline_sv, full_pipeline_verilog};
use aion_diagnostics::{
    Diagnostic, DiagnosticRenderer, DiagnosticSink, Severity, TerminalRenderer,
};
use aion_source::SourceDb;

/// Inputs that used to produce mid-character or out-of-bounds spans.
const REPROS: &[&str] = &[
    "module m; wire a\u{2019}; endmodule",
    "module m; wire caf\u{e9}; endmodule",
    "module m; initial $display(\"abc\\",
    "module m;\u{a0}wire a; endmodule",
    "module m; wire \u{1F600}; endmodule",
    "module m; wire a\u{2019}",
];

/// Lexes and parses `source` with both the Verilog and SV front ends, returning
/// each front end's source database and diagnostics.
fn parse_both(source: &str) -> Vec<(SourceDb, Vec<Diagnostic>)> {
    let mut results = Vec::new();
    for sv in [false, true] {
        let mut db = SourceDb::new();
        let file = db.add_source(if sv { "t.sv" } else { "t.v" }, source.to_string());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        if sv {
            aion_sv_parser::parse_file(file, &db, &interner, &sink);
        } else {
            aion_verilog_parser::parse_file(file, &db, &interner, &sink);
        }
        results.push((db, sink.take_all()));
    }
    results
}

/// Renders every diagnostic with and without color; panics if rendering does.
fn render_all(db: &SourceDb, diags: &[Diagnostic]) -> Vec<String> {
    let mut out = Vec::new();
    for color in [false, true] {
        let renderer = TerminalRenderer::new(color, 80);
        for d in diags {
            out.push(renderer.render(d, db));
        }
    }
    out
}

#[test]
fn repro_inputs_render_without_panic() {
    for src in REPROS {
        for (db, diags) in parse_both(src) {
            assert!(!diags.is_empty(), "expected diagnostics for {src:?}");
            for d in &diags {
                let span = d.primary_span;
                assert!(
                    src.get(span.start as usize..span.end as usize).is_some(),
                    "span {span:?} of {:?} is not valid in {src:?}",
                    d.message
                );
            }
            render_all(&db, &diags);
        }
    }
}

#[test]
fn escaped_multibyte_char_in_string_is_valid() {
    let src = "module m; initial $display(\"\\\u{e9}\"); endmodule";
    for (db, diags) in parse_both(src) {
        assert!(diags.is_empty(), "{diags:?}");
        render_all(&db, &diags);
    }
}

#[test]
fn smart_quote_reported_once_with_real_char() {
    for (db, diags) in parse_both("module m; wire a\u{2019}; endmodule") {
        let lexer_errors: Vec<_> = diags
            .iter()
            .filter(|d| d.message.starts_with("unrecognized character"))
            .collect();
        assert_eq!(lexer_errors.len(), 1, "{diags:?}");
        assert_eq!(
            lexer_errors[0].message,
            "unrecognized character '\u{2019}' (U+2019)"
        );
        let rendered = render_all(&db, &diags).join("\n");
        assert!(rendered.contains("wire a\u{2019};"));
    }
}

#[test]
fn trailing_backslash_string_reports_unterminated() {
    for (db, diags) in parse_both("module m; initial $display(\"abc\\") {
        assert!(diags
            .iter()
            .any(|d| d.message == "unterminated string literal"));
        render_all(&db, &diags);
    }
}

#[test]
fn clean_input_has_no_lexer_errors() {
    let src = "module m(input [7:0] a, output [7:0] y); assign y = a ^ 8 'h FF; endmodule";
    for (_, diags) in parse_both(src) {
        assert!(diags.is_empty(), "{diags:?}");
    }
}

#[test]
fn verilog_escaped_identifier_matches_plain_name() {
    // `\y ` refers to the port `y`, so elaboration finds no unknown signal.
    let src = "module top(input a, output y); assign \\y = \\a ; endmodule";
    let result = full_pipeline_verilog(src, "top");
    assert!(
        !result.has_errors,
        "unexpected errors: {:?}",
        result.diagnostics
    );
    // Escaped top-module name matches the configured plain name too.
    let result = full_pipeline_verilog(
        "module \\top (input a, output y); assign y = a; endmodule",
        "top",
    );
    assert!(!result.has_errors, "{:?}", result.diagnostics);
}

#[test]
fn sv_escaped_identifier_matches_plain_name() {
    let src = "module top(input logic a, output logic y); assign \\y = \\a ; endmodule";
    let result = full_pipeline_sv(src, "top");
    assert!(!result.has_errors, "{:?}", result.diagnostics);
}

#[test]
fn invalid_binary_digit_single_clear_diagnostic() {
    let src = "module top(output [3:0] y); assign y = 4'b102; endmodule";
    let result = full_pipeline_verilog(src, "top");
    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        errors,
        vec!["invalid digit '2' for binary literal"],
        "expected exactly the lexer error"
    );
}
