//! Parser-level tests for preprocessing (`aion_vpp` integration).

use super::*;
use crate::ast::{BinaryOp, Expr, ModuleItem, SvItem};
use aion_diagnostics::{Diagnostic, DiagnosticRenderer, TerminalRenderer};
use aion_source::Span;

struct Parsed {
    db: SourceDb,
    file: FileId,
    ast: SvSourceFile,
    diags: Vec<Diagnostic>,
}

fn parse_ro(src: &str) -> Parsed {
    let mut db = SourceDb::new();
    let file = db.add_source("top.sv", src.to_string());
    let interner = Interner::new();
    let sink = DiagnosticSink::new();
    let ast = parse_file(file, &db, &interner, &sink);
    Parsed {
        db,
        file,
        ast,
        diags: sink.take_all(),
    }
}

fn parse_with(mut db: SourceDb, file: FileId, options: &PreprocessOptions) -> Parsed {
    let interner = Interner::new();
    let sink = DiagnosticSink::new();
    let ast = parse_file_with_options(file, &mut db, options, &interner, &sink);
    Parsed {
        db,
        file,
        ast,
        diags: sink.take_all(),
    }
}

fn parse_path(path: &std::path::Path, options: &PreprocessOptions) -> Parsed {
    let mut db = SourceDb::new();
    let file = db.load_file(path).unwrap();
    parse_with(db, file, options)
}

fn parse_mut(src: &str) -> Parsed {
    let mut db = SourceDb::new();
    let file = db.add_source("top.sv", src.to_string());
    parse_with(db, file, &PreprocessOptions::default())
}

fn messages(p: &Parsed) -> Vec<String> {
    p.diags.iter().map(|d| d.message.clone()).collect()
}

fn assert_clean(p: &Parsed) {
    assert!(
        p.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        messages(p)
    );
}

/// Renders every diagnostic with the terminal renderer, which slices the
/// source by span, so an out-of-bounds or mid-character span would panic.
fn render_all(p: &Parsed) -> String {
    let r = TerminalRenderer::new(false, 80);
    let mut out = String::new();
    for d in &p.diags {
        out.push_str(&r.render(d, &p.db));
        for l in &d.labels {
            let _ = p.db.snippet(l.span);
        }
    }
    out
}

fn module_items(p: &Parsed) -> &[ModuleItem] {
    match &p.ast.items[0] {
        SvItem::Module(m) => &m.items,
        other => panic!("expected a module, got {other:?}"),
    }
}

fn net_span(item: &ModuleItem) -> Span {
    match item {
        ModuleItem::NetDecl(n) => n.span,
        other => panic!("expected a net declaration, got {other:?}"),
    }
}

#[test]
fn timescale_produces_no_diagnostics() {
    let p = parse_ro("`timescale 1ns/1ps\nmodule m; endmodule\n");
    assert_clean(&p);
    assert_eq!(p.ast.items.len(), 1);
}

#[test]
fn macro_in_range_gives_correct_range() {
    let p = parse_ro("`define W 8\nmodule m;\n  wire [`W-1:0] x;\nendmodule\n");
    assert_clean(&p);
    let items = module_items(&p);
    assert_eq!(items.len(), 1);
    let ModuleItem::NetDecl(net) = &items[0] else {
        panic!("expected net decl, got {:?}", items[0]);
    };
    let range = net.range.as_ref().expect("range");
    let Expr::Binary {
        left, op, right, ..
    } = &range.msb
    else {
        panic!("expected binary msb, got {:?}", range.msb);
    };
    assert_eq!(*op, BinaryOp::Sub);
    let (Expr::Literal { span: l }, Expr::Literal { span: r }) = (&**left, &**right) else {
        panic!("expected literals");
    };
    // The expanded literal points at the macro body text; the rest is exact.
    assert_eq!(p.db.snippet(*l), "8");
    assert_eq!(l.file, p.file);
    assert_eq!(l.start, 10);
    assert_eq!(p.db.snippet(*r), "1");
    let Expr::Literal { span: lsb } = &range.lsb else {
        panic!("expected literal lsb");
    };
    assert_eq!(p.db.snippet(*lsb), "0");
}

#[test]
fn ifdef_branches_with_duplicate_declarations() {
    let p = parse_ro("module m;\n`ifdef FAST\n  wire x;\n`else\n  wire x;\n`endif\nendmodule\n");
    assert_clean(&p);
    assert_eq!(module_items(&p).len(), 1);
}

#[test]
fn nested_ifdef_elsif_else() {
    let src = "`define B\n`define INNER\nmodule m;\n\
               `ifdef A\n  wire a;\n\
               `elsif B\n  `ifndef INNER\n    wire b0;\n  `else\n    wire b1;\n    wire b2;\n  `endif\n\
               `else\n  wire c;\n`endif\nendmodule\n";
    let p = parse_ro(src);
    assert_clean(&p);
    let items = module_items(&p);
    assert_eq!(items.len(), 2);
    assert_eq!(p.db.snippet(net_span(&items[0])), "wire b1;");
    assert_eq!(p.db.snippet(net_span(&items[1])), "wire b2;");
}

#[test]
fn function_macros_with_args_defaults_and_nesting() {
    let src = "`define MAX(a, b = 0) ((a) > (b) ? (a) : (b))\n\
               `define WIDTH `MAX(4, 8)\n\
               module m(input [7:0] a, input [7:0] b, input [7:0] c, output [7:0] y, output [7:0] z);\n\
               assign y = `MAX(`MAX(a, b), c);\n\
               assign z = `MAX(a);\n\
               wire [`WIDTH-1:0] w;\n\
               endmodule\n";
    let p = parse_ro(src);
    assert_clean(&p);
    assert_eq!(module_items(&p).len(), 3);
}

#[test]
fn multi_line_macro_with_continuation() {
    let src =
        "`define DECLS \\\n  wire a; \\\n  wire b; \\\n  wire c;\nmodule m;\n`DECLS\nendmodule\n";
    let p = parse_ro(src);
    assert_clean(&p);
    let items = module_items(&p);
    assert_eq!(items.len(), 3);
    assert_eq!(p.db.snippet(net_span(&items[2])), "wire c;");
}

#[test]
fn recursive_macro_reports_error() {
    let p = parse_ro("`define LOOP (`LOOP + 1)\nmodule m; wire [`LOOP:0] x; endmodule\n");
    assert!(
        messages(&p)
            .iter()
            .any(|m| m.contains("recursive expansion of macro `LOOP`")),
        "{:?}",
        messages(&p)
    );
    render_all(&p);
}

#[test]
fn unbalanced_endif_reports_error() {
    let p = parse_ro("module m;\n`endif\nendmodule\n");
    assert_eq!(messages(&p).len(), 1, "{:?}", messages(&p));
    assert!(p.diags[0].message.contains("`endif without"));
    assert_eq!(p.db.snippet(p.diags[0].primary_span), "`endif");
    let p = parse_ro("`ifdef X\nmodule m; endmodule\n");
    assert_eq!(messages(&p).len(), 1, "{:?}", messages(&p));
    assert!(p.diags[0].message.contains("missing `endif"));
}

#[test]
fn undefined_macro_consumes_only_the_macro_token() {
    let p = parse_ro("module m; wire [`NOPE:0] x; wire y; endmodule\n");
    assert!(p.diags[0].message.contains("NOPE"), "{:?}", messages(&p));
    assert_eq!(p.db.snippet(p.diags[0].primary_span), "`NOPE");
    // The parser may complain about the now-empty msb, but nothing cascades
    // past the declaration: `wire y;` still parses.
    assert!(p.diags.len() <= 3, "{:?}", messages(&p));
    assert!(module_items(&p).len() >= 2);
    render_all(&p);
}

#[test]
fn include_from_temp_dir() {
    let dir = tempfile::tempdir().unwrap();
    let inc = dir.path().join("include");
    std::fs::create_dir(&inc).unwrap();
    std::fs::write(
        inc.join("defs.vh"),
        "`ifndef DEFS_VH\n`define DEFS_VH\n`define W 4\n`endif\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("body.vh"), "wire [`W-1:0] from_inc;\n").unwrap();
    let top = dir.path().join("top.sv");
    std::fs::write(
        &top,
        "`include \"defs.vh\"\n`include \"defs.vh\"\nmodule m;\n`include \"body.vh\"\nwire local;\nendmodule\n",
    )
    .unwrap();
    let opts = PreprocessOptions::default().with_include_dir(&inc);
    let p = parse_path(&top, &opts);
    assert_clean(&p);
    let items = module_items(&p);
    assert_eq!(items.len(), 2);
    let inc_span = net_span(&items[0]);
    assert_ne!(
        inc_span.file, p.file,
        "included declaration keeps its own file"
    );
    assert_eq!(p.db.snippet(inc_span), "wire [`W-1:0] from_inc;");
    let local = net_span(&items[1]);
    assert_eq!(local.file, p.file);
    assert_eq!(p.db.snippet(local), "wire local;");
}

#[test]
fn include_not_found_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().join("top.sv");
    std::fs::write(&top, "`include \"nope.vh\"\nmodule m; endmodule\n").unwrap();
    let p = parse_path(&top, &PreprocessOptions::default());
    assert_eq!(messages(&p).len(), 1, "{:?}", messages(&p));
    assert!(p.diags[0]
        .message
        .contains("cannot find include file `nope.vh`"));
    assert_eq!(p.ast.items.len(), 1);
    render_all(&p);
}

#[test]
fn include_cycle_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.vh"), "`include \"b.vh\"\n").unwrap();
    std::fs::write(dir.path().join("b.vh"), "`include \"a.vh\"\n").unwrap();
    let top = dir.path().join("top.sv");
    std::fs::write(&top, "`include \"a.vh\"\nmodule m; endmodule\n").unwrap();
    let p = parse_path(&top, &PreprocessOptions::default());
    assert_eq!(messages(&p).len(), 1, "{:?}", messages(&p));
    assert!(p.diags[0].message.contains("include cycle"));
    assert_eq!(p.ast.items.len(), 1);
    render_all(&p);
}

#[test]
fn readonly_parse_file_rejects_include() {
    let p = parse_ro("`include \"x.vh\"\nmodule m; endmodule\n");
    assert_eq!(messages(&p).len(), 1);
    assert_eq!(p.ast.items.len(), 1);
}

#[test]
fn predefined_macros_from_options() {
    let mut db = SourceDb::new();
    let file = db.add_source(
        "top.sv",
        "module m;\n`ifdef SYNTH\nwire [`W-1:0] x;\n`endif\nendmodule\n".to_string(),
    );
    let opts = PreprocessOptions::default()
        .with_define("SYNTH", "")
        .with_define("W", "16");
    let p = parse_with(db, file, &opts);
    assert_clean(&p);
    let ModuleItem::NetDecl(n) = &module_items(&p)[0] else {
        panic!("expected net decl");
    };
    let Expr::Binary { left, .. } = &n.range.as_ref().unwrap().msb else {
        panic!("expected binary");
    };
    assert_eq!(p.db.snippet(left.span()), "16");
}

#[test]
fn literal_built_across_macro_boundary_uses_view_file() {
    let src = "`define W 8\nmodule m; wire [7:0] x; assign x = `W'b0; endmodule\n";
    let p = parse_mut(src);
    assert_clean(&p);
    let ModuleItem::ContinuousAssign(a) = &module_items(&p)[1] else {
        panic!("expected assign, got {:?}", module_items(&p)[1]);
    };
    let Expr::Literal { span } = &a.value else {
        panic!("expected literal, got {:?}", a.value);
    };
    assert_ne!(span.file, p.file);
    assert_eq!(p.db.snippet(*span), "8'b0");
    // The assign statement itself still starts at its real location.
    assert_eq!(a.span.file, p.file);

    // Read-only parsing of the same text reports, rather than miscompiles,
    // the token whose text exists in no file.
    let p = parse_ro(src);
    assert_eq!(messages(&p).len(), 1, "{:?}", messages(&p));
    assert!(p.diags[0].message.contains("8'b0"));
    render_all(&p);
}

#[test]
fn spans_in_macro_expanded_code_are_in_bounds() {
    let src = "`define BAD(x) x +\n`define PASTE(a, b) a``b\nmodule m;\n\
               assign y = `BAD(\"é\");\n wire `PASTE(q, 1) = ;\n wire [`UNDEF:0] z;\nendmodule\n";
    for p in [parse_ro(src), parse_mut(src)] {
        assert!(p.diags.len() >= 3, "{:?}", messages(&p));
        let rendered = render_all(&p);
        assert!(rendered.contains("error["));
    }
}

#[test]
fn previously_cascading_define_line_is_clean() {
    // Before the preprocessor this produced 9 cascading diagnostics.
    let p = parse_ro("`define W 8\nmodule m;\nwire [`W-1:0] x;\nendmodule\n");
    assert_clean(&p);
}
