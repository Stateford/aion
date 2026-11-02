//! End-to-end tests for parameter names used in expressions.
//!
//! A `localparam`-encoded FSM is the most common real-world use of named
//! constants: state encodings in case labels and comparisons, a parameter in
//! a comparison expression, and a parameter as a delay. Each test goes from
//! HDL source through parse → elaborate → simulate (or lint) for Verilog and
//! SystemVerilog.

use aion_common::Interner;
use aion_config::ProjectConfig;
use aion_conformance::{full_pipeline_sv, full_pipeline_verilog, PipelineResult};
use aion_diagnostics::{DiagnosticSink, Severity};
use aion_elaborate::ParsedDesign;
use aion_sim::SimKernel;
use aion_source::SourceDb;

/// A testbench FSM: IDLE → RUN (counts to `STEPS - 1`) → DONE → IDLE, with
/// the clock period set by a parameter delay. `{var}` is the variable
/// keyword and `{ff}` the clocked-process keyword.
const FSM_TEMPLATE: &str = r#"
module top;
  localparam [1:0] IDLE = 2'd0, RUN = 2'd1, DONE = 2'd2;
  localparam HALF = 5, STEPS = 3;
  {var} clk;
  {var} [1:0] state;
  {var} [7:0] count, visits;
  always #HALF clk = ~clk;
  {ff} @(posedge clk) begin
    case (state)
      IDLE: begin count <= 0; state <= RUN; end
      RUN: if (count == STEPS - 1) state <= DONE; else count <= count + 1;
      DONE: begin visits <= visits + 1; state <= IDLE; end
      default: state <= IDLE;
    endcase
  end
  initial begin clk = 0; state = IDLE; count = 0; visits = 0; #200 $finish; end
endmodule
"#;

/// Instantiates [`FSM_TEMPLATE`] for Verilog (`v`) or SystemVerilog (`sv`).
fn fsm_source(sv: bool) -> String {
    let (var, ff) = if sv {
        ("logic", "always_ff")
    } else {
        ("reg", "always")
    };
    FSM_TEMPLATE.replace("{var}", var).replace("{ff}", ff)
}

/// Parses, elaborates and simulates `src` to completion, asserting that
/// elaboration reports no errors, and returns the kernel.
fn simulate(file_name: &str, src: &str) -> SimKernel {
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
    let config: ProjectConfig =
        toml::from_str("[project]\nname = \"param_fsm\"\nversion = \"0.1.0\"\ntop = \"top\"\n")
            .unwrap();
    let design = aion_elaborate::elaborate(&parsed, &config, &source_db, &interner, &sink)
        .expect("elaboration should not return an internal error");
    let errors: Vec<_> = sink
        .diagnostics()
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{file_name}: errors: {errors:?}");
    let mut kernel = SimKernel::new(&design, &interner).expect("kernel construction");
    kernel.run_to_completion().expect("simulation");
    kernel
}

/// Returns the final value of `top.<name>` (`None` if X/Z).
fn value(kernel: &SimKernel, name: &str) -> Option<u64> {
    let id = kernel
        .find_signal(&format!("top.{name}"))
        .unwrap_or_else(|| panic!("signal top.{name} not found"));
    kernel.signal_value(id).to_u64()
}

#[test]
fn localparam_fsm_simulates_through_every_state() {
    // Posedges at 5, 15, ..., 195: 20 edges. One loop is 5 edges (IDLE,
    // RUN x3, DONE), so the FSM completes 4 loops and ends in IDLE.
    for (file, sv) in [("top.v", false), ("top.sv", true)] {
        let kernel = simulate(file, &fsm_source(sv));
        assert_eq!(value(&kernel, "visits"), Some(4), "{file}");
        assert_eq!(value(&kernel, "state"), Some(0), "{file}: IDLE");
        assert_eq!(value(&kernel, "count"), Some(2), "{file}: STEPS - 1");
    }
}

/// Returns the codes of every diagnostic in `result`, as `W103`-style text.
fn codes(result: &PipelineResult) -> Vec<String> {
    result
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn localparam_fsm_lints_without_parameter_false_positives() {
    // A parameter reference is not a magic number (C203), a constant
    // condition (W108), or a 32-bit value truncated into the state register
    // (W103/W107), and it is neither an unknown signal (E204) nor a signal
    // that could be unused or undriven (W101/W102).
    let src = "module m(input clk, input rst, input go, output busy);
          localparam IDLE = 0, RUN = 1, DONE = 2; localparam DEBUG = 0;
          reg [1:0] state;
          always @(posedge clk)
            if (DEBUG) state <= DONE;
            else if (rst) state <= IDLE;
            else case (state)
              IDLE: if (go) state <= RUN;
              RUN: state <= DONE;
              default: state <= IDLE;
            endcase
          assign busy = (state == RUN);
        endmodule";
    for (label, result) in [
        ("verilog", full_pipeline_verilog(src, "m")),
        ("sv", full_pipeline_sv(src, "m")),
    ] {
        let codes = codes(&result);
        for bad in ["E204", "W101", "W102", "W103", "W107", "W108", "C203"] {
            assert!(
                !codes.iter().any(|c| c == bad),
                "{label}: unexpected {bad}: {:#?}",
                result.diagnostics
            );
        }
    }
}

#[test]
fn magic_number_still_reported_for_plain_literal() {
    // The same FSM written with a bare literal still gets C203: only named
    // constants are exempt.
    let src = "module m(input clk, output busy); reg [1:0] state;
          always @(posedge clk) state <= 2'd2;
          assign busy = (state == 2'd2); endmodule";
    let result = full_pipeline_verilog(src, "m");
    assert!(
        codes(&result).iter().any(|c| c == "C203"),
        "{:#?}",
        result.diagnostics
    );
}
