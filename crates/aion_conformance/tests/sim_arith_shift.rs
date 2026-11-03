//! End-to-end simulation tests for the Verilog/SystemVerilog arithmetic
//! shift operators `>>>` and `<<<` (IEEE 1364-2005 5.1.12, IEEE 1800-2017
//! 11.4.10).
//!
//! `>>>` fills the vacated bits with the sign bit when its left operand is
//! signed and with zeros otherwise; `<<<` is identical to `<<`. Each test
//! goes from source through parse → elaborate → simulate.

use aion_common::{Interner, LogicVec};
use aion_config::ProjectConfig;
use aion_diagnostics::{DiagnosticSink, Severity};
use aion_elaborate::ParsedDesign;
use aion_sim::SimKernel;
use aion_source::SourceDb;

/// Parses `src` (SystemVerilog for a `.sv` name, else Verilog), elaborates
/// it with top `top`, simulates it to completion, and returns the kernel.
fn simulate(file_name: &str, src: &str, top: &str) -> SimKernel {
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
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
    let config: ProjectConfig = toml::from_str(&format!(
        "[project]\nname = \"sim_test\"\nversion = \"0.1.0\"\ntop = \"{top}\"\n"
    ))
    .unwrap();
    let design = aion_elaborate::elaborate(&parsed, &config, &source_db, &interner, &sink)
        .expect("elaboration should not return an internal error");
    let errors: Vec<_> = sink
        .diagnostics()
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "errors: {errors:?}");
    let mut kernel = SimKernel::new(&design, &interner).expect("kernel construction");
    kernel.run_to_completion().expect("simulation");
    kernel
}

/// Returns the final value of `top.<name>`.
fn signal(kernel: &SimKernel, name: &str) -> LogicVec {
    let id = kernel
        .find_signal(&format!("top.{name}"))
        .unwrap_or_else(|| panic!("signal top.{name} not found"));
    kernel.signal_value(id).clone()
}

/// Returns the final value of `top.<name>` as an integer (`None` if X/Z).
fn value(kernel: &SimKernel, name: &str) -> Option<u64> {
    signal(kernel, name).to_u64()
}

#[test]
fn verilog_ashr_sign_fills_a_signed_operand() {
    let src = r#"
module top;
  reg signed [7:0] s;
  reg [7:0] u;
  reg [7:0] r1, r2, r3, r4, r5, r6;
  reg signed [15:0] w;
  initial begin
    s = -8;
    u = 8'hF8;
    r1 = s >>> 1;
    r2 = s >> 1;
    r3 = u >>> 1;
    r4 = s >>> 10;
    r5 = s[7:0] >>> 1;
    r6 = s <<< 1;
    w = s >>> 1;
  end
endmodule
"#;
    let k = simulate("top.v", src, "top");
    assert_eq!(value(&k, "r1"), Some(0xFC), "signed >>> sign-fills");
    assert_eq!(value(&k, "r2"), Some(0x7C), ">> is always logical");
    assert_eq!(value(&k, "r3"), Some(0x7C), "unsigned >>> zero-fills");
    assert_eq!(value(&k, "r4"), Some(0xFF), "over-shift fills every bit");
    assert_eq!(value(&k, "r5"), Some(0x7C), "a part-select is unsigned");
    assert_eq!(value(&k, "r6"), Some(0xF0), "<<< is <<");
    // The operand is sign-extended to the 16-bit context before shifting.
    assert_eq!(value(&k, "w"), Some(0xFFFC));
}

#[test]
fn verilog_ashr_by_a_variable_amount_and_of_a_positive_value() {
    let src = r#"
module top;
  reg signed [7:0] s;
  reg [2:0] n;
  reg [7:0] neg, pos;
  initial begin
    n = 3'd3;
    s = -64;
    neg = s >>> n;
    s = 64;
    pos = s >>> n;
  end
endmodule
"#;
    let k = simulate("top.v", src, "top");
    assert_eq!(value(&k, "neg"), Some(0xF8));
    assert_eq!(value(&k, "pos"), Some(0x08));
}

#[test]
fn sv_ashr_and_compound_ashr_assign() {
    let src = r#"
module top;
  logic signed [7:0] s;
  logic signed [7:0] c;
  logic [7:0] u;
  logic [7:0] r1, r2;
  initial begin
    s = -16;
    u = 8'hF0;
    r1 = s >>> 2;
    r2 = u >>> 2;
    c = -16;
    c >>>= 3;
  end
endmodule
"#;
    let k = simulate("top.sv", src, "top");
    assert_eq!(value(&k, "r1"), Some(0xFC));
    assert_eq!(value(&k, "r2"), Some(0x3C));
    assert_eq!(value(&k, "c"), Some(0xFE));
}
