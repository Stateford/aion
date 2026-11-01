//! End-to-end simulation tests for blocking vs non-blocking assignments.
//!
//! Each test goes from HDL source through parse → elaborate → simulate and
//! checks IEEE 1364/1800 region semantics: blocking (`=`, `:=`) writes are
//! visible to the next statement of the same process, while non-blocking
//! (`<=`) writes sample their right-hand side immediately but update the
//! target only in the NBA region, after every triggered process has run.

use aion_common::Interner;
use aion_config::ProjectConfig;
use aion_diagnostics::{DiagnosticSink, Severity};
use aion_elaborate::ParsedDesign;
use aion_sim::{SimKernel, SimResult};
use aion_source::SourceDb;

/// Parses `src` (language chosen by the extension of `file_name`),
/// elaborates it with top `top`, and simulates it to completion.
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
    } else if file_name.ends_with(".vhd") {
        parsed.vhdl_files.push(aion_vhdl_parser::parse_file(
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
    // The elaborator keeps the format string's source quotes; strip them so
    // assertions read naturally.
    for line in &mut result.display_output {
        *line = line.trim_matches('"').to_string();
    }
    (kernel, result)
}

/// Returns the final value of `top.<name>` as an integer (`None` if X/Z).
fn value(kernel: &SimKernel, name: &str) -> Option<u64> {
    let id = kernel
        .find_signal(&format!("top.{name}"))
        .unwrap_or_else(|| panic!("signal {name} not found"));
    kernel.signal_value(id).to_u64()
}

#[test]
fn verilog_blocking_writes_visible_in_same_initial() {
    // SIM-3: blocking writes used to be deferred until the process ended,
    // so `b = a` read the stale a (X) and `z = tmp` the stale tmp.
    let src = r#"
module tb;
  reg a;
  reg b;
  reg [7:0] x;
  reg [7:0] y;
  reg [7:0] tmp;
  reg [7:0] z;
  initial begin
    a = 1;
    b = a;
    x = 8'd3;
    y = 8'd4;
    tmp = x + y;
    z = tmp;
    $display("b=%d z=%d", b, z);
  end
endmodule
"#;
    let (kernel, result) = simulate("tb.v", src, "tb");
    assert_eq!(result.display_output, vec!["b=1 z=7".to_string()]);
    assert_eq!(value(&kernel, "a"), Some(1));
    assert_eq!(value(&kernel, "b"), Some(1));
    assert_eq!(value(&kernel, "z"), Some(7));
}

#[test]
fn verilog_nonblocking_swap_and_shift_register() {
    // Non-blocking swap swaps; blocking "swap" copies. The shift register
    // stages are written in reverse order and must still shift by one.
    let src = r#"
module tb;
  reg clk;
  reg [7:0] a;
  reg [7:0] b;
  reg [7:0] c;
  reg [7:0] d;
  reg [7:0] din;
  reg [7:0] q1;
  reg [7:0] q2;
  initial begin
    clk = 0;
    a = 1; b = 2;
    c = 1; d = 2;
    din = 8'd5; q1 = 0; q2 = 0;
    #5 clk = 1;
    #5 $display("a=%d b=%d c=%d d=%d q1=%d q2=%d", a, b, c, d, q1, q2);
    $finish;
  end
  always @(posedge clk) begin a <= b; b <= a; end
  always @(posedge clk) begin c = d; d = c; end
  always @(posedge clk) q2 <= q1;
  always @(posedge clk) q1 <= din;
endmodule
"#;
    let (_, result) = simulate("tb.v", src, "tb");
    assert!(result.finished_by_user);
    assert_eq!(
        result.display_output,
        vec!["a=2 b=1 c=2 d=2 q1=5 q2=0".to_string()]
    );
}

#[test]
fn verilog_display_after_nonblocking_shows_old_value() {
    // `<=` is not visible to the next statement of the same process; the
    // value is there after the time step's NBA region.
    let src = r#"
module tb;
  reg [7:0] r;
  initial begin
    r = 8'd1;
    #1;
    r <= 8'd9;
    $display("now=%d", r);
    #1 $display("later=%d", r);
  end
endmodule
"#;
    let (kernel, result) = simulate("tb.v", src, "tb");
    assert_eq!(
        result.display_output,
        vec!["now=1".to_string(), "later=9".to_string()]
    );
    assert_eq!(value(&kernel, "r"), Some(9));
}

#[test]
fn sv_always_comb_temp_feeds_always_ff() {
    // always_comb computes through a blocking temporary (`=` then `+=`);
    // always_ff registers it with `<=`.
    let src = r#"
module tb;
  logic clk;
  logic [7:0] x;
  logic [7:0] y;
  logic [7:0] sum;
  logic [7:0] q;
  always_comb begin
    sum = x;
    sum += y;
  end
  always_ff @(posedge clk) q <= sum;
  initial begin
    clk = 0;
    x = 8'd10;
    y = 8'd20;
    #5 clk = 1;
    #1 $display("sum=%d q=%d", sum, q);
  end
endmodule
"#;
    let (kernel, result) = simulate("tb.sv", src, "tb");
    assert_eq!(result.display_output, vec!["sum=30 q=30".to_string()]);
    assert_eq!(value(&kernel, "q"), Some(30));
}

#[test]
fn vhdl_variable_assignment_is_immediate() {
    // `:=` to a process variable is visible to the next statement, so the
    // chain below drives `q` with the inverted `a` (all ones), not X.
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  signal a : std_logic_vector(7 downto 0);
  signal q : std_logic_vector(7 downto 0);
begin
  process (a)
    variable v : std_logic_vector(7 downto 0);
  begin
    v := a;
    v := not v;
    q <= v;
  end process;
end architecture;
"#;
    let (kernel, _) = simulate("tb.vhd", src, "tb");
    assert_eq!(value(&kernel, "a"), Some(0));
    assert_eq!(value(&kernel, "q"), Some(0xFF));
}
