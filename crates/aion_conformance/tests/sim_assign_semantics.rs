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
    value_of(kernel, &format!("top.{name}"))
}

/// Returns the final value of the signal with hierarchical name `full`.
fn value_of(kernel: &SimKernel, full: &str) -> Option<u64> {
    let id = kernel
        .find_signal(full)
        .unwrap_or_else(|| panic!("signal {full} not found"));
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

#[test]
fn vhdl_process_constant_and_variable_defaults_are_honoured() {
    // Process-local declarations keep their declared defaults.
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  signal a : std_logic;
  signal y : std_logic_vector(3 downto 0);
begin
  p : process (a)
    constant K : std_logic_vector(3 downto 0) := "1010";
    variable t : std_logic_vector(3 downto 0) := "0110";
  begin
    y <= K xor t;
  end process;
end architecture;
"#;
    let (kernel, _) = simulate("tb.vhd", src, "tb");
    // VHDL basic identifiers are case-folded, so `K` is the signal `k`.
    assert_eq!(value_of(&kernel, "top.p.k"), Some(0b1010));
    assert_eq!(value_of(&kernel, "top.p.t"), Some(0b0110));
    assert_eq!(value(&kernel, "y"), Some(0b1100));
}

#[test]
fn vhdl_architecture_constant_and_signal_defaults_are_honoured() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  constant K : std_logic_vector(7 downto 0) := x"5A";
  constant Z : std_logic_vector(7 downto 0) := (others => '0');
  signal s : std_logic := '1';
  signal y : std_logic_vector(7 downto 0);
begin
  process (s)
  begin
    y <= K or Z;
  end process;
end architecture;
"#;
    let (kernel, _) = simulate("tb.vhd", src, "tb");
    // VHDL basic identifiers are case-folded, so `K`/`Z` are `k`/`z`.
    assert_eq!(value(&kernel, "k"), Some(0x5A));
    assert_eq!(value(&kernel, "z"), Some(0));
    assert_eq!(value(&kernel, "s"), Some(1));
    assert_eq!(value(&kernel, "y"), Some(0x5A));
}

#[test]
fn vhdl_process_all_does_not_wake_on_its_own_variable() {
    // `process(all)` is sensitive to signals only; writing the variable
    // `v` must not re-trigger the process (it used to loop until the
    // delta-cycle limit at 0 fs).
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  signal a : std_logic;
  signal y : std_logic;
begin
  process (all)
    variable v : std_logic;
  begin
    if v = '1' then
      v := '0';
    else
      v := '1';
    end if;
    y <= v and not a;
  end process;
end architecture;
"#;
    let (kernel, _) = simulate("tb.vhd", src, "tb");
    // Ran exactly once (the time-0 pass): v toggled from X to '1' once; a
    // second run would have flipped it back to '0'.
    assert_eq!(value_of(&kernel, "top._process0.v"), Some(1));
    assert_eq!(value(&kernel, "y"), Some(1));
}

#[test]
fn vhdl_same_named_variables_in_two_processes_stay_distinct() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  signal a : std_logic;
  signal y1 : std_logic;
  signal y2 : std_logic;
begin
  first : process (a)
    variable t : std_logic := '1';
  begin
    y1 <= t;
  end process;
  process (a)
    variable t : std_logic := '0';
  begin
    y2 <= t;
  end process;
end architecture;
"#;
    let (kernel, _) = simulate("tb.vhd", src, "tb");
    let names: Vec<&str> = kernel.all_signals().iter().map(|(_, n, _)| *n).collect();
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "duplicate names: {names:?}");
    assert_eq!(value_of(&kernel, "top.first.t"), Some(1));
    assert_eq!(value(&kernel, "y1"), Some(1));
    assert_eq!(value(&kernel, "y2"), Some(0));
}

#[test]
fn verilog_partial_write_zero_extends_to_slice_width() {
    // The unsized `0` must clear all four bits of the slice, for `=` and
    // for `<=`.
    let src = r#"
module tb;
  reg [7:0] q;
  reg [7:0] r;
  initial begin
    q = 8'hFF;
    q[7:4] = 0;
    r = 8'hFF;
    #1 r[7:4] <= 0;
  end
endmodule
"#;
    let (kernel, _) = simulate("tb.v", src, "tb");
    assert_eq!(value(&kernel, "q"), Some(0x0F));
    assert_eq!(value(&kernel, "r"), Some(0x0F));
}

#[test]
fn vhdl_conditional_and_selected_assignments_simulate() {
    // Concurrent and VHDL-2008 sequential conditional / selected signal
    // assignments, `unaffected`, and an aggregate target (#16).
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity tb is
end entity;

architecture sim of tb is
  signal s   : std_logic := '1';
  signal t   : std_logic := '0';
  signal on1 : std_logic := '1';
  signal off : std_logic := '0';
  signal a   : std_logic_vector(3 downto 0) := x"A";
  signal b   : std_logic_vector(3 downto 0) := x"5";
  signal c   : std_logic_vector(3 downto 0) := x"F";
  signal d   : std_logic_vector(3 downto 0) := "1001";
  signal y1, y2, y3, y4, y5, y6 : std_logic_vector(3 downto 0);
  signal l1 : std_logic_vector(3 downto 0);
  signal l2 : std_logic_vector(3 downto 0) := x"3";
  signal hi, lo : std_logic_vector(1 downto 0);
begin
  y1 <= a when s = '1' else b;
  y2 <= a when t = '1' else b when s = '0' else c;
  with s select y3 <= a when '0', b when others;
  with t select y4 <= transport a when '0', unaffected when others;
  l1 <= a when on1 = '1';
  l2 <= a when off = '1';
  (hi, lo) <= d;
  process (all)
    variable v : std_logic_vector(3 downto 0);
  begin
    v := b when s = '1' else c;
    y5 <= v when t = '1' else a;
    with s select y6 <= v when '1', c when others;
  end process;
end architecture;
"#;
    let (kernel, _) = simulate("tb.vhd", src, "tb");
    assert_eq!(value(&kernel, "y1"), Some(0xA));
    assert_eq!(value(&kernel, "y2"), Some(0xF));
    assert_eq!(value(&kernel, "y3"), Some(0x5));
    assert_eq!(value(&kernel, "y4"), Some(0xA));
    // `l1` follows `a` while enabled; `l2` keeps its initial value (closed latch).
    assert_eq!(value(&kernel, "l1"), Some(0xA));
    assert_eq!(value(&kernel, "l2"), Some(0x3));
    // `(hi, lo) <= "1001"`: leftmost elements go to `hi`.
    assert_eq!(value(&kernel, "hi"), Some(0b10));
    assert_eq!(value(&kernel, "lo"), Some(0b01));
    assert_eq!(value(&kernel, "y5"), Some(0xA));
    assert_eq!(value(&kernel, "y6"), Some(0x5));
}
