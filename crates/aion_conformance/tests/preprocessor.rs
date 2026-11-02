//! End-to-end preprocessor tests: on-disk files with `include, `define and
//! `ifdef go through parse → elaborate, and the elaborated widths must
//! reflect the macro values (which also checks that macro-expanded literal
//! spans point at text the elaborator can read).

use aion_common::Interner;
use aion_conformance::make_config;
use aion_diagnostics::{Diagnostic, DiagnosticRenderer, DiagnosticSink, TerminalRenderer};
use aion_elaborate::ParsedDesign;
use aion_ir::Design;
use aion_source::SourceDb;
use aion_verilog_parser::PreprocessOptions;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

struct Outcome {
    design: Design,
    interner: Interner,
    diags: Vec<Diagnostic>,
}

fn elaborate_dir(dir: &Path, files: &[&str], top: &str, options: &PreprocessOptions) -> Outcome {
    let mut db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();
    let mut parsed = ParsedDesign {
        verilog_files: vec![],
        sv_files: vec![],
        vhdl_files: vec![],
    };
    for name in files {
        let id = db.load_file(&dir.join(name)).unwrap();
        if name.ends_with(".sv") {
            parsed
                .sv_files
                .push(aion_sv_parser::parse_file_with_options(
                    id, &mut db, options, &interner, &sink,
                ));
        } else {
            parsed
                .verilog_files
                .push(aion_verilog_parser::parse_file_with_options(
                    id, &mut db, options, &interner, &sink,
                ));
        }
    }
    let design = aion_elaborate::elaborate(&parsed, &make_config(top), &db, &interner, &sink)
        .expect("elaboration should not return an internal error");
    let diags = sink.take_all();
    // Every diagnostic span must be renderable (in bounds, on char boundaries).
    let renderer = TerminalRenderer::new(false, 80);
    for d in &diags {
        let _ = renderer.render(d, &db);
    }
    Outcome {
        design,
        interner,
        diags,
    }
}

fn signal_width(o: &Outcome, module: &str, signal: &str) -> Option<u32> {
    let m = o
        .design
        .modules
        .values()
        .find(|m| o.interner.resolve(m.name) == module)?;
    let s = m
        .signals
        .values()
        .find(|s| o.interner.resolve(s.name) == signal)?;
    o.design.types.bit_width(s.ty)
}

fn errors(o: &Outcome) -> Vec<String> {
    o.diags
        .iter()
        .filter(|d| d.severity.is_error())
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn include_define_and_ifdef_drive_elaborated_widths() {
    let tmp = TempDir::new().unwrap();
    let inc = tmp.path().join("inc");
    fs::create_dir(&inc).unwrap();
    fs::write(
        inc.join("cfg.vh"),
        "`ifndef CFG_VH\n`define CFG_VH\n`define W 8\n`define DOUBLE(x) ((x) * 2)\n`endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("top.v"),
        "`timescale 1ns/1ps\n\
         `include \"cfg.vh\"\n\
         `include \"cfg.vh\"\n\
         module top(input clk);\n\
         \x20 wire [`W-1:0] narrow;\n\
         \x20 wire [`DOUBLE(`W)-1:0] wide;\n\
         `ifdef FAST\n\
         \x20 wire [1:0] sel;\n\
         `else\n\
         \x20 wire [`W/2-1:0] sel;\n\
         `endif\n\
         \x20 assign narrow = `W'hA5;\n\
         \x20 sub u(.clk(clk));\n\
         endmodule\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("sub.sv"),
        "`include \"cfg.vh\"\nmodule sub(input logic clk);\n  logic [`W-1:0] q;\n  always_ff @(posedge clk) q <= q + 1;\nendmodule\n",
    )
    .unwrap();

    let opts = PreprocessOptions::default().with_include_dir(&inc);
    let o = elaborate_dir(tmp.path(), &["top.v", "sub.sv"], "top", &opts);
    assert!(errors(&o).is_empty(), "{:?}", errors(&o));
    assert_eq!(signal_width(&o, "top", "narrow"), Some(8));
    assert_eq!(signal_width(&o, "top", "wide"), Some(16));
    assert_eq!(signal_width(&o, "top", "sel"), Some(4));
    assert_eq!(signal_width(&o, "sub", "q"), Some(8));

    // A predefined macro flips the `ifdef branch.
    let opts = opts.with_define("FAST", "");
    let o = elaborate_dir(tmp.path(), &["top.v", "sub.sv"], "top", &opts);
    assert!(errors(&o).is_empty(), "{:?}", errors(&o));
    assert_eq!(signal_width(&o, "top", "sel"), Some(2));
}

#[test]
fn predefined_width_overrides_and_missing_include_is_reported() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("top.v"),
        "module top;\n  wire [`W-1:0] bus;\nendmodule\n",
    )
    .unwrap();
    let opts = PreprocessOptions::default().with_define("W", "12");
    let o = elaborate_dir(tmp.path(), &["top.v"], "top", &opts);
    assert!(errors(&o).is_empty(), "{:?}", errors(&o));
    assert_eq!(signal_width(&o, "top", "bus"), Some(12));

    fs::write(
        tmp.path().join("bad.v"),
        "`include \"nowhere.vh\"\nmodule top;\nendmodule\n",
    )
    .unwrap();
    let o = elaborate_dir(tmp.path(), &["bad.v"], "top", &PreprocessOptions::default());
    let errs = errors(&o);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("nowhere.vh"));
}
