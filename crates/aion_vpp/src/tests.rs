//! Unit tests for the preprocessor engine and span mapping.

use super::*;
use aion_diagnostics::{Diagnostic, DiagnosticRenderer, TerminalRenderer};
use aion_source::Span;

struct Run {
    db: SourceDb,
    pp: Preprocessed,
    diags: Vec<Diagnostic>,
}

fn run_with(src: &str, options: &PreprocessOptions) -> Run {
    let mut db = SourceDb::new();
    let file = db.add_source("top.v", src.to_string());
    let sink = DiagnosticSink::new();
    let pp = preprocess(file, &mut db, options, &sink);
    Run {
        db,
        pp,
        diags: sink.take_all(),
    }
}

fn run(src: &str) -> Run {
    run_with(src, &PreprocessOptions::default())
}

/// Collapses whitespace so tests can compare expansions structurally.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn messages(r: &Run) -> Vec<String> {
    r.diags.iter().map(|d| d.message.clone()).collect()
}

fn ok(src: &str) -> String {
    let r = run(src);
    assert!(
        r.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        messages(&r)
    );
    norm(r.pp.text())
}

/// Renders every diagnostic; panics if any span is out of bounds or not on
/// a char boundary.
fn render_all(r: &Run) {
    let renderer = TerminalRenderer::new(false, 80);
    for d in &r.diags {
        let _ = renderer.render(d, &r.db);
        let _ = r.db.snippet(d.primary_span);
    }
}

/// Splits `text` into whitespace-separated words with their byte ranges.
fn words(text: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s as u32, i as u32));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s as u32, text.len() as u32));
    }
    out
}

// ---------------------------------------------------------------------------
// Plain text and directives
// ---------------------------------------------------------------------------

#[test]
fn plain_text_is_identity() {
    let src = "module m; wire x; endmodule\n";
    let r = run(src);
    assert!(r.diags.is_empty());
    assert_eq!(r.pp.text(), src);
    assert_eq!(
        r.pp.exact_span(7, 8),
        Some(Span::new(r.pp.top_file(), 7, 8))
    );
}

#[test]
fn timescale_is_accepted_and_recorded() {
    let r = run("`timescale 1ns/1ps // comment\nmodule m; endmodule\n");
    assert!(r.diags.is_empty(), "{:?}", messages(&r));
    assert_eq!(r.pp.timescales.len(), 1);
    assert_eq!(r.pp.timescales[0].value, "1ns/1ps");
    assert_eq!(r.db.snippet(r.pp.timescales[0].span), "`timescale 1ns/1ps");
    assert_eq!(norm(r.pp.text()), "// comment module m; endmodule");
}

#[test]
fn timescale_without_precision_is_an_error() {
    let r = run("`timescale 1ns\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("timescale"));
    render_all(&r);
}

#[test]
fn default_nettype_recorded_and_validated() {
    let r = run("`default_nettype none\n`default_nettype bogus\n");
    assert_eq!(r.pp.default_nettypes.len(), 1);
    assert_eq!(r.pp.default_nettypes[0].value, "none");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("bogus"));
}

#[test]
fn ignorable_directives_produce_no_diagnostics() {
    let text = ok(
        "`resetall\n`celldefine\nmodule m; endmodule\n`endcelldefine\n\
         `line 3 \"x.v\" 0\n`pragma protect begin\n`begin_keywords \"1800-2017\"\n\
         `end_keywords\n`unconnected_drive pull1\n`nounconnected_drive\n",
    );
    assert_eq!(text, "module m; endmodule");
}

#[test]
fn unconnected_drive_requires_pull_strength() {
    let r = run("`unconnected_drive weak\n");
    assert_eq!(r.diags.len(), 1);
}

#[test]
fn stray_backtick_is_reported() {
    let r = run("wire ` x;\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("stray"));
    assert_eq!(norm(r.pp.text()), "wire x;");
}

#[test]
fn backticks_in_strings_and_comments_are_ignored() {
    let text = ok("// `bogus\n/* `bogus */ $display(\"`bogus\");\n");
    assert!(text.contains("\"`bogus\""));
}

// ---------------------------------------------------------------------------
// `define / macro usage
// ---------------------------------------------------------------------------

#[test]
fn object_macro_expands_mid_expression() {
    assert_eq!(ok("`define W 8\nwire [`W-1:0] x;\n"), "wire [8-1:0] x;");
}

#[test]
fn macro_body_tokens_map_to_definition() {
    let r = run("`define W 8\nwire [`W-1:0] x;\n");
    let idx = r.pp.text().find('8').unwrap() as u32;
    let span = r.pp.exact_span(idx, idx + 1).unwrap();
    assert_eq!(r.db.snippet(span), "8");
    assert_eq!(span.start, 10);
    // Text after the expansion keeps its own exact span.
    let minus = r.pp.text().find("-1").unwrap() as u32;
    let span = r.pp.exact_span(minus, minus + 2).unwrap();
    assert_eq!(r.db.snippet(span), "-1");
}

#[test]
fn function_macro_with_args_and_defaults() {
    assert_eq!(
        ok("`define ADD(a, b = 2) ((a) + (b))\nx = `ADD(1, 5); y = `ADD(3); z = `ADD(4,);\n"),
        "x = ((1) + (5)); y = ((3) + (2)); z = ((4) + (2));"
    );
}

#[test]
fn nested_invocation_and_macro_to_macro() {
    assert_eq!(
        ok("`define MAX(a,b) ((a)>(b)?(a):(b))\n`define TOP `MAX(1, `MAX(2,3))\nx = `TOP;\n"),
        "x = ((1)>(((2)>(3)?(2):(3)))?(1):(((2)>(3)?(2):(3))));"
    );
}

#[test]
fn multi_line_body_with_continuation() {
    let r = run("`define DECL(n) \\\n  wire n; \\\n  wire n``_b;\n`DECL(foo)\nafter\n");
    assert!(r.diags.is_empty(), "{:?}", messages(&r));
    assert_eq!(norm(r.pp.text()), "wire foo; wire foo_b; after");
}

#[test]
fn arguments_may_span_lines_and_contain_commas_in_parens() {
    assert_eq!(
        ok("`define F(a, b) a | b\nx = `F(f(1, 2),\n       {3, 4});\n"),
        "x = f(1, 2) | {3, 4};"
    );
}

#[test]
fn stringification_and_pasting() {
    let r = run("`define S(x) `\"x`\"\n`define CAT(a,b) a``b\ns = `S(hi); `CAT(foo, bar) = 1;\n");
    assert!(r.diags.is_empty(), "{:?}", messages(&r));
    assert_eq!(norm(r.pp.text()), "s = \"hi\"; foobar = 1;");
    // Pasted identifiers have no exact span.
    let i = r.pp.text().find("foobar").unwrap() as u32;
    assert_eq!(r.pp.exact_span(i, i + 6), None);
}

#[test]
fn parameter_names_are_not_substituted_inside_strings() {
    assert_eq!(
        ok("`define P(x) $display(\"x=%d\", x)\n`P(v);\n"),
        "$display(\"x=%d\", v);"
    );
}

#[test]
fn undef_and_undefineall() {
    let r = run("`define A 1\n`undef A\n`A\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("`A"));
    let r = run("`define A 1\n`define B 2\n`undefineall\n`ifdef A a `elsif B b `else c `endif\n");
    assert!(r.diags.is_empty());
    assert_eq!(norm(r.pp.text()), "c");
}

#[test]
fn undefined_macro_consumes_only_the_name() {
    let r = run("wire [`NOPE-1:0] x;\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("NOPE"));
    assert_eq!(r.db.snippet(r.diags[0].primary_span), "`NOPE");
    assert_eq!(norm(r.pp.text()), "wire [-1:0] x;");
}

#[test]
fn recursive_macro_is_an_error_not_a_hang() {
    let r = run("`define R `R + 1\nx = `R;\n");
    assert_eq!(r.diags.len(), 1, "{:?}", messages(&r));
    assert!(r.diags[0].message.contains("recursive"));
    assert!(r.diags[0].notes[0].contains("in expansion of macro `R`"));
    render_all(&r);

    let r = run("`define A `B\n`define B `A\n`A\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0]
        .message
        .contains("recursive expansion of macro `A`"));
}

#[test]
fn exponential_expansion_hits_the_limit() {
    let mut src = String::from("`define M0 x\n");
    for i in 1..=24 {
        src.push_str(&format!("`define M{i} `M{} `M{}\n", i - 1, i - 1));
    }
    src.push_str("`M24\n");
    let r = run(&src);
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("limit"));
}

#[test]
fn argument_count_errors() {
    let r = run("`define F(a,b) a+b\n`F(1)\n`F(1,2,3)\n`F\n");
    let m = messages(&r);
    assert_eq!(m.len(), 3, "{m:?}");
    assert!(m[0].contains("missing argument for parameter `b`"));
    assert!(m[1].contains("takes 2 argument(s) but 3"));
    assert!(m[2].contains("no argument list"));
    render_all(&r);
}

#[test]
fn unterminated_argument_list_is_reported() {
    let r = run("`define F(a) a\n`F(1, 2\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("unterminated"));
}

#[test]
fn bad_define_lines_are_reported() {
    let r = run("`define\n`define 9x\n`define ifdef 1\n`define F(a,) a\n`define G(a, a) a\n");
    assert_eq!(r.diags.len(), 5, "{:?}", messages(&r));
    render_all(&r);
}

#[test]
fn file_and_line_macros() {
    let r = run("\n\nx = `__LINE__; s = `__FILE__;\n");
    assert!(r.diags.is_empty());
    assert_eq!(norm(r.pp.text()), "x = 3; s = \"top.v\";");
}

#[test]
fn predefined_macros_from_options() {
    let opts = PreprocessOptions::default()
        .with_define("W", "16")
        .with_define("SYNTH", "")
        .with_define("SQ(x)", "((x)*(x))");
    let r = run_with("`ifdef SYNTH a = `W + `SQ(2); `endif\n", &opts);
    assert!(r.diags.is_empty(), "{:?}", messages(&r));
    assert_eq!(norm(r.pp.text()), "a = 16 + ((2)*(2));");
    let i = r.pp.text().find("16").unwrap() as u32;
    let span = r.pp.exact_span(i, i + 2).unwrap();
    assert_eq!(r.db.snippet(span), "16");
}

// ---------------------------------------------------------------------------
// Conditionals
// ---------------------------------------------------------------------------

#[test]
fn nested_conditionals_select_one_branch() {
    let src = "`define A\n`define C\n\
               `ifdef A\n `ifdef B b1 `elsif C c1 `else e1 `endif\n`else\n never\n`endif\n\
               `ifndef A n `elsif C c2 `else e2 `endif\n";
    assert_eq!(ok(src), "c1 c2");
}

#[test]
fn inactive_regions_skip_defines_and_unknown_directives() {
    let src = "`ifdef NOPE\n`define X 1\n`bogus junk\n\"unterminated\n`endif\n`ifdef X bad `else good `endif\n";
    assert_eq!(ok(src), "good");
}

#[test]
fn unbalanced_conditionals_are_reported() {
    let r = run("`endif\n`else\n`elsif A\n");
    assert_eq!(r.diags.len(), 3, "{:?}", messages(&r));
    let r = run("`ifdef A\nx\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("missing `endif"));
    assert_eq!(r.db.snippet(r.diags[0].primary_span), "`ifdef A");
    let r = run("`ifdef A `else `else `endif\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("duplicate `else"));
    let r = run("`ifdef A `else `elsif B `endif\n");
    assert_eq!(r.diags.len(), 1);
}

#[test]
fn ifdef_without_name_is_reported() {
    let r = run("`ifdef\n`endif\n");
    assert_eq!(r.diags.len(), 1);
}

// ---------------------------------------------------------------------------
// `include
// ---------------------------------------------------------------------------

#[test]
fn include_relative_to_including_file_and_include_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let inc = dir.path().join("inc");
    std::fs::create_dir(&inc).unwrap();
    std::fs::write(dir.path().join("local.vh"), "`define LOCAL 1\nlocal_text").unwrap();
    std::fs::write(inc.join("lib.vh"), "lib_text // no newline").unwrap();
    let top = dir.path().join("top.v");
    std::fs::write(
        &top,
        "`include \"local.vh\"\n`include <lib.vh> after\nx = `LOCAL;\n",
    )
    .unwrap();

    let mut db = SourceDb::new();
    let file = db.load_file(&top).unwrap();
    let sink = DiagnosticSink::new();
    let opts = PreprocessOptions::default().with_include_dir(&inc);
    let pp = preprocess(file, &mut db, &opts, &sink);
    let diags = sink.take_all();
    assert!(
        diags.is_empty(),
        "{:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    assert_eq!(
        norm(pp.text()),
        "local_text lib_text // no newline after x = 1;"
    );
    assert_eq!(pp.included_files.len(), 2);

    // Included text keeps its own FileId and exact spans.
    let i = pp.text().find("lib_text").unwrap() as u32;
    let span = pp.exact_span(i, i + 8).unwrap();
    assert_eq!(span.file, pp.included_files[1]);
    assert_eq!(db.snippet(span), "lib_text");
}

#[test]
fn include_not_found_is_reported_with_search_path() {
    let r = run("`include \"missing.vh\"\nwire x;\n");
    assert_eq!(r.diags.len(), 1);
    assert!(r.diags[0].message.contains("missing.vh"));
    assert!(r.diags[0].notes[0].contains("searched"));
    assert_eq!(norm(r.pp.text()), "wire x;");
    render_all(&r);
}

#[test]
fn include_cycle_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.vh"), "a `include \"b.vh\"\n").unwrap();
    std::fs::write(dir.path().join("b.vh"), "b `include \"a.vh\"\n").unwrap();
    let top = dir.path().join("top.v");
    std::fs::write(&top, "`include \"a.vh\"\n").unwrap();
    let mut db = SourceDb::new();
    let file = db.load_file(&top).unwrap();
    let sink = DiagnosticSink::new();
    let pp = preprocess(file, &mut db, &PreprocessOptions::default(), &sink);
    let diags = sink.take_all();
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("include cycle"));
    assert!(diags[0].notes[0].contains("a.vh -> "));
    assert_eq!(norm(pp.text()), "a b");
    for d in &diags {
        let _ = TerminalRenderer::new(false, 80).render(d, &db);
    }
}

#[test]
fn include_depth_limit_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        std::fs::write(
            dir.path().join(format!("i{i}.vh")),
            format!("`include \"i{}.vh\"\n", i + 1),
        )
        .unwrap();
    }
    std::fs::write(dir.path().join("i5.vh"), "leaf\n").unwrap();
    let top = dir.path().join("top.v");
    std::fs::write(&top, "`include \"i0.vh\"\n").unwrap();
    let mut db = SourceDb::new();
    let file = db.load_file(&top).unwrap();
    let sink = DiagnosticSink::new();
    let opts = PreprocessOptions {
        max_include_depth: 3,
        ..Default::default()
    };
    let _ = preprocess(file, &mut db, &opts, &sink);
    let diags = sink.take_all();
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("limit of 3"));
}

#[test]
fn include_via_macro_and_malformed_include() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("x.vh"), "xx\n").unwrap();
    let top = dir.path().join("top.v");
    std::fs::write(
        &top,
        "`define F \"x.vh\"\n`include `F\n`include x.vh\n`include \"x.vh\n",
    )
    .unwrap();
    let mut db = SourceDb::new();
    let file = db.load_file(&top).unwrap();
    let sink = DiagnosticSink::new();
    let pp = preprocess(file, &mut db, &PreprocessOptions::default(), &sink);
    let diags = sink.take_all();
    assert_eq!(
        diags.len(),
        2,
        "{:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    assert!(norm(pp.text()).starts_with("xx"));
}

#[test]
fn readonly_mode_rejects_include_but_expands_macros() {
    let mut db = SourceDb::new();
    let file = db.add_source("top.v", "`define W 8\n`include \"a.vh\"\nx = `W;\n".into());
    let sink = DiagnosticSink::new();
    let pp = preprocess_readonly(file, &db, &PreprocessOptions::default(), &sink);
    let diags = sink.take_all();
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("read-only"));
    assert_eq!(norm(pp.text()), "x = 8;");
}

// ---------------------------------------------------------------------------
// Span mapping through lex_mapped
// ---------------------------------------------------------------------------

/// A whitespace-splitting stand-in for a front-end lexer.
fn word_lexer(text: &str, file: aion_source::FileId) -> Vec<Span> {
    words(text)
        .into_iter()
        .map(|(s, e)| Span::new(file, s, e))
        .collect()
}

#[test]
fn lex_mapped_gives_exact_or_view_spans() {
    let mut db = SourceDb::new();
    let file = db.add_source(
        "top.v",
        "`define W 8\n`define CAT(a,b) a``b\nx = `W'b0 + `CAT(p, q) + y\n".into(),
    );
    let sink = DiagnosticSink::new();
    let pp = preprocess(file, &mut db, &PreprocessOptions::default(), &sink);
    let (spans, ranges) = lex_mapped(&pp, Some(&mut db), &sink, |t, _| word_lexer(t, file), |s| s);
    assert!(sink.take_all().is_empty());
    for (span, (s, e)) in spans.iter().zip(&ranges) {
        // Every span's SourceDb text equals the token text.
        assert_eq!(db.snippet(*span), &pp.text()[*s as usize..*e as usize]);
    }
    let texts: Vec<&str> = spans.iter().map(|s| db.snippet(*s)).collect();
    assert_eq!(texts, vec!["x", "=", "8'b0", "+", "pq", "+", "y"]);
    // Straddling / pasted tokens live in the view file; others in top.v.
    assert_ne!(spans[2].file, file);
    assert_eq!(spans[0].file, file);
    assert_eq!(spans[6].file, file);
}

#[test]
fn lex_mapped_readonly_reports_inexact_once() {
    let mut db = SourceDb::new();
    let file = db.add_source(
        "top.v",
        "`define CAT(a,b) a``b\n`CAT(p, q) `CAT(r, s)\n".into(),
    );
    let sink = DiagnosticSink::new();
    let pp = preprocess_readonly(file, &db, &PreprocessOptions::default(), &sink);
    let (spans, _) = lex_mapped(&pp, None, &sink, |t, _| word_lexer(t, file), |s| s);
    let diags = sink.take_all();
    assert_eq!(diags.len(), 1);
    // Falls back to the invocation site.
    assert_eq!(db.snippet(spans[0]), "`CAT(p, q)");
}

#[test]
fn lex_mapped_remaps_lexer_diagnostics_and_eof() {
    let mut db = SourceDb::new();
    let file = db.add_source("top.v", "`define W 8\nab `W\n".into());
    let sink = DiagnosticSink::new();
    let pp = preprocess(file, &mut db, &PreprocessOptions::default(), &sink);
    let text_len = pp.text().len() as u32;
    let (spans, _) = lex_mapped(
        &pp,
        Some(&mut db),
        &sink,
        |t, lex_sink| {
            let i = t.find('8').unwrap() as u32;
            lex_sink.emit(Diagnostic::error(
                diagnostic_code(),
                "fake lexer error",
                Span::new(file, i, i + 1),
            ));
            vec![Span::new(file, text_len, text_len)]
        },
        |s| s,
    );
    let diags = sink.take_all();
    assert_eq!(diags.len(), 1);
    assert_eq!(db.snippet(diags[0].primary_span), "8");
    assert_eq!(diags[0].primary_span.start, 10);
    // EOF maps to the end of the top-level file.
    let len = db.get_file(file).content.len() as u32;
    assert_eq!(spans[0], Span::new(file, len, len));
}

#[test]
fn map_span_handles_foreign_and_out_of_range_spans() {
    let r = run("abc\n");
    let other = Span::new(aion_source::FileId::from_raw(7), 1, 2);
    assert_eq!(r.pp.map_span(other), other);
    let top = r.pp.top_file();
    assert_eq!(
        r.pp.map_span(Span::new(top, 100, 200)),
        Span::new(top, 4, 4)
    );
    assert_eq!(r.pp.map_span(Span::new(top, 0, 3)), Span::new(top, 0, 3));
}

#[test]
fn diagnostics_inside_macros_render_in_bounds() {
    let r = run("`define BAD(x) x + `UNDEF + \\\n  `\n`BAD(é)\n`define U é`NOPE\n`U\n");
    assert!(r.diags.len() >= 3, "{:?}", messages(&r));
    render_all(&r);
}

#[test]
fn non_ascii_text_survives_unchanged() {
    let text = ok("// héllo `x\n\"ünï`code\" \\esc`aped wire\n");
    assert!(text.contains("\"ünï`code\""));
    assert!(text.contains("\\esc`aped"));
}

#[test]
fn diagnostic_code_is_e106() {
    assert_eq!(diagnostic_code().to_string(), "E106");
}
