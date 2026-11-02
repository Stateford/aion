//! End-to-end simulation tests for named port binding across the
//! Verilog/VHDL boundary.
//!
//! VHDL basic identifiers are case-insensitive and stored folded to
//! lowercase, while Verilog identifiers are case-sensitive. A mixed-case
//! named association (`.Clk(x)` on a VHDL entity, `Clk => x` on a Verilog
//! module) must still bind to the child's port so values actually flow
//! through the hierarchy in the simulator.

use aion_common::Interner;
use aion_config::ProjectConfig;
use aion_diagnostics::{DiagnosticSink, Severity};
use aion_elaborate::ParsedDesign;
use aion_sim::SimKernel;
use aion_source::SourceDb;

/// Parses each `(file_name, source)` pair (language chosen by extension),
/// elaborates with top `top`, asserts there are no errors, simulates to
/// completion, and returns the kernel for reading final signal values.
fn simulate(files: &[(&str, &str)], top: &str) -> SimKernel {
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
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
        } else {
            parsed.verilog_files.push(aion_verilog_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        }
    }
    let config: ProjectConfig = toml::from_str(&format!(
        "[project]\nname = \"bind_test\"\nversion = \"0.1.0\"\ntop = \"{top}\"\n"
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

/// Returns the final value of the signal with hierarchical name `full`.
fn value_of(kernel: &SimKernel, full: &str) -> Option<u64> {
    let id = kernel
        .find_signal(full)
        .unwrap_or_else(|| panic!("signal {full} not found"));
    kernel.signal_value(id).to_u64()
}

#[test]
fn verilog_tb_drives_vhdl_child_through_mixed_case_ports() {
    let child = r#"
library ieee;
use ieee.std_logic_1164.all;
entity Inv is
  port (A : in std_logic; Y : out std_logic);
end entity;
architecture rtl of Inv is
begin
  process (a)
  begin
    y <= not a;
  end process;
end architecture;
"#;
    // Two instances with opposite inputs, so an unbound port (which would
    // leave the child input or the parent wire at its default) is visible.
    let tb = r#"
module tb;
  reg x0;
  reg x1;
  wire z0;
  wire z1;
  Inv u0(.A(x0), .Y(z0));
  Inv u1(.A(x1), .Y(z1));
  initial begin
    x0 = 1'b1;
    x1 = 1'b0;
  end
endmodule
"#;
    let kernel = simulate(&[("inv.vhd", child), ("tb.v", tb)], "tb");
    // A bound port aliases the parent signal, so the child has no separate
    // `top.u0.a` signal.
    assert!(kernel.find_signal("top.u0.a").is_none());
    // z0 = not x0 needs `.A(x0)` bound; z1 = 1 needs `.Y(z1)` bound.
    assert_eq!(value_of(&kernel, "top.z0"), Some(0));
    assert_eq!(value_of(&kernel, "top.z1"), Some(1));
}

#[test]
fn vhdl_tb_drives_verilog_child_through_mixed_case_ports() {
    let child = "module Inv2(input A, output Y); assign Y = ~A; endmodule\n";
    let tb = r#"
library ieee;
use ieee.std_logic_1164.all;
entity tb is
end entity;
architecture sim of tb is
  signal x0 : std_logic := '1';
  signal x1 : std_logic := '0';
  signal z0, z1 : std_logic;
begin
  u0 : entity work.Inv2 port map (A => x0, Y => z0);
  u1 : entity work.Inv2 port map (A => x1, Y => z1);
end architecture;
"#;
    let kernel = simulate(&[("inv2.v", child), ("tb.vhd", tb)], "tb");
    // The folded formals `a`/`y` bound to the Verilog ports `A`/`Y`.
    assert!(kernel.find_signal("top.u0.A").is_none());
    assert_eq!(value_of(&kernel, "top.z0"), Some(0));
    assert_eq!(value_of(&kernel, "top.z1"), Some(1));
}
