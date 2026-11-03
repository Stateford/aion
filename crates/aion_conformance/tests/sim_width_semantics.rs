//! End-to-end simulation tests for width-correct value semantics (issue #6).
//!
//! Each test goes from Verilog/SystemVerilog source through parse →
//! elaborate → simulate and checks IEEE 1364 sizing and 4-state rules:
//! every write is resized to its target, expressions are evaluated at the
//! context width, `x / 0` is X, bit-selects keep X/Z, arithmetic works above
//! 64 bits, and `x -> 1` counts as a positive edge.

use aion_common::{Interner, Logic, LogicVec};
use aion_config::ProjectConfig;
use aion_diagnostics::{DiagnosticSink, Severity};
use aion_elaborate::ParsedDesign;
use aion_sim::{SimKernel, SimResult};
use aion_source::SourceDb;

/// Parses `src` (SystemVerilog for a `.sv` name, else Verilog), elaborates
/// it with top `top`, and simulates it to completion.
///
/// Returns the kernel (for reading final signal values) and the run result.
fn simulate(file_name: &str, src: &str, top: &str) -> (SimKernel, SimResult) {
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
    let mut result = kernel.run_to_completion().expect("simulation");
    for line in &mut result.display_output {
        *line = line.trim_matches('"').to_string();
    }
    (kernel, result)
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
fn sim5_narrow_assign_keeps_width_then_bit_write() {
    // Repro E: used to panic with "index 7 out of bounds for width 4".
    let src = r#"
module top;
  reg [7:0] y;
  reg [3:0] a;
  initial begin
    a = 0;
    y = a;
    y[7] = 1;
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    let y = signal(&kernel, "y");
    assert_eq!(y.width(), 8);
    assert_eq!(y.to_u64(), Some(0x80));
}

#[test]
fn sim5_narrow_assign_in_clocked_process_keeps_width() {
    // The same pattern through the event queue (blocking writes in an
    // always block become events), plus a VCD-safe width.
    let src = r#"
module top;
  reg clk;
  reg [7:0] y;
  reg [3:0] a;
  initial begin
    clk = 0;
    a = 4'h5;
    #1 clk = 1;
    #1 clk = 0;
    #1 clk = 1;
  end
  always @(posedge clk) begin
    y = a;
    y[7] = 1;
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    let y = signal(&kernel, "y");
    assert_eq!(y.width(), 8);
    assert_eq!(y.to_u64(), Some(0x85));
}

#[test]
fn sim6_pow_wraps_instead_of_panicking() {
    // Repro F: 2 ** 200 overflowed u128 and panicked.
    let src = r#"
module top;
  reg [31:0] p;
  reg [31:0] q;
  initial begin
    p = 32'd2 ** 32'd200;
    q = 32'd3 ** 32'd5;
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    assert_eq!(value(&kernel, "p"), Some(0));
    assert_eq!(value(&kernel, "q"), Some(243));
}

#[test]
fn sim7_division_by_zero_is_x_not_an_abort() {
    // Repro G: wires start at 0, so `a / b` used to abort the run at t=0.
    let src = r#"
module top;
  reg [7:0] a;
  reg [7:0] b;
  wire [7:0] q;
  wire [7:0] r;
  reg [7:0] q0;
  assign q = a / b;
  assign r = a % b;
  initial begin
    a = 0;
    b = 0;
    #1 q0 = q;
    #1 a = 8'd20;
    b = 8'd3;
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    let q0 = signal(&kernel, "q0");
    assert_eq!(q0.width(), 8);
    assert!((0..8).all(|i| q0.get(i) == Logic::X), "q0 = {q0}");
    assert_eq!(value(&kernel, "q"), Some(6));
    assert_eq!(value(&kernel, "r"), Some(2));
}

#[test]
fn sim8_bit_select_keeps_x() {
    // Repro H: r[0] of an X register used to read as 0.
    let src = r#"
module top;
  reg [3:0] r;
  reg [3:0] k;
  reg y;
  reg z;
  initial begin
    k = 4'b0100;
    y = r[0];
    z = k[2];
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    assert_eq!(signal(&kernel, "y").get(0), Logic::X);
    assert_eq!(value(&kernel, "z"), Some(1));
}

#[test]
fn sim9_wide_arithmetic_comparison_and_case() {
    // Repro I: 72'd1 + 72'd1 was all-X and 72'd1 == 72'd1 was X.
    let src = r#"
module top;
  reg [71:0] a;
  reg [71:0] s;
  reg [71:0] big;
  reg [71:0] m;
  reg eq;
  reg gt;
  reg hit;
  initial begin
    a = 72'd1;
    s = a + a;
    eq = (a == 72'd1);
    // Built with a shift: the Verilog elaborator still lowers literals
    // through a 64-bit integer, so a wider literal would lose its top bits.
    big = 72'd255 << 64;
    gt = big > a;
    m = big >> 64;
    hit = 0;
    case (a)
      72'd2: hit = 0;
      72'd1: hit = 1;
    endcase
    if (a == 72'd1) $display("wide eq ok");
    $display("%d %h", big, big);
  end
endmodule
"#;
    let (kernel, result) = simulate("top.v", src, "top");
    assert_eq!(value(&kernel, "s"), None, "72-bit value does not fit u64");
    let s = signal(&kernel, "s");
    assert_eq!(s.width(), 72);
    assert_eq!(format!("{s}"), format!("{:072b}", 2));
    assert_eq!(value(&kernel, "eq"), Some(1));
    assert_eq!(value(&kernel, "gt"), Some(1));
    assert_eq!(value(&kernel, "hit"), Some(1));
    let m = signal(&kernel, "m");
    assert_eq!(format!("{m}"), format!("{:072b}", 0xFF));
    assert_eq!(
        result.display_output,
        vec![
            "wide eq ok".to_string(),
            "4703919738795935662080 ff0000000000000000".to_string(),
        ]
    );
}

#[test]
fn sim10_x_to_one_is_a_posedge() {
    // Repro N: rst starts at X; `#1 rst = 1` must trigger posedge rst.
    let src = r#"
module top;
  reg rst;
  reg hit;
  reg nhit;
  initial begin
    hit = 0;
    nhit = 0;
    #1 rst = 1;
    #1 rst = 1'bx;
    #1 rst = 0;
  end
  always @(posedge rst) hit = 1;
  always @(negedge rst) nhit = 1;
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    assert_eq!(value(&kernel, "hit"), Some(1));
    assert_eq!(value(&kernel, "nhit"), Some(1));
}

#[test]
fn sim12_narrow_rhs_zero_extends() {
    // Repro O: y kept its stale upper nibble (11110000).
    let src = r#"
module top;
  reg [7:0] y;
  reg [3:0] a;
  reg [7:0] y0;
  initial begin
    y = 8'hFF;
    a = 4'hF;
    #1 y0 = y;
    a = 4'h0;
  end
  always @* y = a;
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    assert_eq!(value(&kernel, "y0"), Some(0x0F));
    assert_eq!(value(&kernel, "y"), Some(0));
}

#[test]
fn sim12_context_width_keeps_the_carry() {
    let src = r#"
module top;
  reg [7:0] a;
  reg [7:0] b;
  reg [7:0] sum;
  reg cout;
  reg [8:0] wide;
  reg [7:0] narrow;
  initial begin
    a = 8'hFF;
    b = 8'h01;
    {cout, sum} = a + b;
    wide = a + b;
    narrow = a + b;
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    assert_eq!(value(&kernel, "cout"), Some(1));
    assert_eq!(value(&kernel, "sum"), Some(0));
    assert_eq!(value(&kernel, "wide"), Some(0x100));
    assert_eq!(value(&kernel, "narrow"), Some(0));
}

#[test]
fn signed_regs_sign_extend_and_compare_signed() {
    let src = r#"
module top;
  reg signed [7:0] a;
  reg signed [7:0] b;
  reg signed [15:0] w;
  reg [15:0] u;
  reg lt;
  reg ult;
  initial begin
    a = -8'd3;
    b = 8'd1;
    w = a;
    u = a;
    lt = a < b;
    ult = a[7:0] < b[7:0];
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    assert_eq!(value(&kernel, "a"), Some(0xFD));
    assert_eq!(value(&kernel, "w"), Some(0xFFFD));
    // The RHS is signed, so it is sign-extended even into an unsigned target.
    assert_eq!(value(&kernel, "u"), Some(0xFFFD));
    assert_eq!(value(&kernel, "lt"), Some(1));
    // Part-selects are unsigned: 253 < 1 is false.
    assert_eq!(value(&kernel, "ult"), Some(0));
}

#[test]
fn signed_literals_keep_signed_semantics() {
    // An unsized decimal is a signed 32-bit integer and `'s` makes a sized
    // literal signed (IEEE 1364-2005 3.5.1, 5.5), so mixing them with a
    // signed operand keeps the operation signed.
    let src = r#"
module top;
  integer i;
  reg signed [7:0] s;
  reg lt0, slt, sgt, big;
  reg [15:0] w16;
  reg [7:0] c;
  initial begin
    i = -1;
    s = -8'sd2;
    c = 8'hFF;
    lt0 = (i < 0);
    slt = (s < 0);
    sgt = (s > -8'sd3);
    w16 = s + 1;
    // An unsized literal is 32 bits wide, so `c + 1` does not wrap at 8.
    big = (c + 1) > c;
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    assert_eq!(value(&kernel, "lt0"), Some(1), "integer -1 < 0");
    assert_eq!(value(&kernel, "slt"), Some(1), "signed -2 < 0");
    assert_eq!(value(&kernel, "sgt"), Some(1), "-2 > -3 signed");
    assert_eq!(value(&kernel, "w16"), Some(0xFFFF), "s + 1 sign-extends");
    assert_eq!(value(&kernel, "big"), Some(1), "c + 1 is 32 bits wide");
}

#[test]
fn sized_unsigned_literal_keeps_operation_unsigned() {
    // Without `s` a sized (or based) literal is unsigned, so the signed
    // operand is treated as unsigned too: -2 (0xFE) < 8'd0 is false.
    let src = r#"
module top;
  reg signed [7:0] s;
  reg lt, gt, hlt;
  reg [15:0] w16;
  initial begin
    s = -8'sd2;
    lt = (s < 8'd0);
    gt = (s > 8'd1);
    hlt = (s < 'h0);
    w16 = s + 8'd1;
  end
endmodule
"#;
    let (kernel, _) = simulate("top.v", src, "top");
    assert_eq!(value(&kernel, "lt"), Some(0));
    assert_eq!(value(&kernel, "gt"), Some(1));
    assert_eq!(value(&kernel, "hlt"), Some(0));
    assert_eq!(value(&kernel, "w16"), Some(0x00FF));
}

#[test]
fn display_width_specifiers() {
    // `%0d` is one specification (it used to print `%0d` and swallow the
    // argument). An explicit width pads decimal with spaces and the other
    // radices with zeros; `-` left-justifies.
    let src = r#"
module top;
  reg [7:0] u;
  reg [3:0] x;
  initial begin
    u = 8'd5;
    // `x` is never assigned, so it stays X (the elaborator cannot lower
    // X/Z literals yet).
    $display("v=%0d h=%h", u, u);
    $display("[%4d] [%-4d] [%0h] [%3d] [%6b] [%0b] [%b]", u, u, u, x, x, u, u);
  end
endmodule
"#;
    let (_, result) = simulate("top.v", src, "top");
    assert_eq!(result.display_output[0], "v=5 h=5");
    assert_eq!(
        result.display_output[1],
        "[   5] [5   ] [5] [  x] [00xxxx] [101] [00000101]"
    );
}
