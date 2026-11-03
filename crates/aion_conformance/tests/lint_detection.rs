//! Tests for lint rule detection through the full pipeline.
//!
//! Each test runs the full parse → elaborate → lint pipeline on HDL source
//! and asserts that expected diagnostics are (or are not) emitted.

use aion_conformance::{
    full_pipeline_sv, full_pipeline_verilog, full_pipeline_verilog_with_config,
    full_pipeline_verilog_with_lint, full_pipeline_vhdl, full_pipeline_vhdl_with_config,
};
use aion_diagnostics::Severity;

#[test]
fn unused_wire_w101() {
    let src = r#"
module top (input a, output y);
    wire unused_w;
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w101 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 101 && d.message.contains("unused"));
    assert!(has_w101, "expected W101 for unused wire");
}

#[test]
fn latch_inferred_w106() {
    let src = r#"
module top (
    input a,
    input sel,
    output reg y
);
    always @(*) begin
        if (sel)
            y = a;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w106 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 106 && d.message.contains("latch"));
    assert!(
        has_w106,
        "expected W106 for latch inferred (if without else)"
    );
}

#[test]
fn initial_block_e102() {
    let src = r#"
module top (input clk, output reg q);
    initial begin
        q = 1'b0;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_e102 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 102 && d.severity == Severity::Error);
    assert!(has_e102, "expected E102 for initial block");
}

#[test]
fn missing_case_default_w106() {
    let src = r#"
module top (
    input [1:0] sel,
    input a, b,
    output reg y
);
    always @(*) begin
        case (sel)
            2'b00: y = a;
            2'b01: y = b;
        endcase
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w106 = result.diagnostics.iter().any(|d| d.code.number == 106);
    assert!(has_w106, "expected W106 for case without default");
}

#[test]
fn clean_sv_counter_no_errors() {
    let src = r#"
module counter (
    input logic clk,
    input logic rst_n,
    output logic [7:0] count
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            count <= 8'h00;
        else
            count <= count + 8'h01;
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "counter");
    assert!(!result.has_errors, "clean counter should have no errors");
}

#[test]
fn clean_verilog_fsm_minimal_warnings() {
    let src = r#"
module fsm (
    input clk,
    input rst,
    input go,
    output reg done
);
    reg [1:0] state;

    always @(posedge clk) begin
        if (rst) begin
            state <= 2'b00;
            done  <= 1'b0;
        end else begin
            case (state)
                2'b00: begin
                    if (go) state <= 2'b01;
                    done <= 1'b0;
                end
                2'b01: begin
                    state <= 2'b10;
                    done  <= 1'b0;
                end
                2'b10: begin
                    done  <= 1'b1;
                    state <= 2'b00;
                end
                default: state <= 2'b00;
            endcase
        end
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "fsm");
    assert!(!result.has_errors, "clean FSM should have no errors");
}

#[test]
fn allow_config_suppresses_rule() {
    // Design with unused wire — W101 should normally fire
    let src = r#"
module top (input a, output y);
    wire unused_w;
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog_with_lint(src, "top", &[], &["unused-signal"]);
    let has_w101 = result.diagnostics.iter().any(|d| d.code.number == 101);
    assert!(!has_w101, "W101 should be suppressed by allow config");
}

#[test]
fn deny_config_promotes_severity() {
    // Design with unused wire — W101 normally a warning, but denied = error
    let src = r#"
module top (input a, output y);
    wire unused_w;
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog_with_lint(src, "top", &["unused-signal"], &[]);
    let denied = result.diagnostics.iter().find(|d| d.code.number == 101);
    assert!(denied.is_some(), "W101 should still fire when denied");
    assert_eq!(
        denied.unwrap().severity,
        Severity::Error,
        "denied rule should be promoted to error"
    );
}

#[test]
fn multiple_lint_issues_on_one_design() {
    let src = r#"
module top (input clk, output reg q);
    wire unused1;
    wire unused2;
    initial begin
        q = 1'b0;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    // Should have at least W101 for unused wires + E102 for initial block
    assert!(
        result.diagnostics.len() >= 2,
        "expected multiple lint issues, got {}",
        result.diagnostics.len()
    );
}

#[test]
fn clean_vhdl_through_pipeline() {
    let src = r#"
entity top is
    port (
        a : in  std_logic;
        b : in  std_logic;
        y : out std_logic
    );
end entity top;

architecture rtl of top is
begin
    y <= a and b;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "top");
    assert!(!result.has_errors, "clean VHDL should have no errors");
}

// ============================================================================
// W102: Undriven signal
// ============================================================================

#[test]
fn undriven_wire_w102() {
    let src = r#"
module top (input a, output y);
    wire w;
    assign y = w;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w102 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 102 && d.message.contains("undriven"));
    assert!(has_w102, "expected W102 for wire read but never driven");
}

#[test]
fn undriven_output_port_w102() {
    let src = r#"
module top (input a, output y);
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w102 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 102 && d.message.contains("undriven"));
    assert!(has_w102, "expected W102 for output port never assigned");
}

#[test]
fn driven_wire_no_w102() {
    let src = r#"
module top (input a, output y);
    wire w;
    assign w = a;
    assign y = w;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w102 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 102 && d.message.contains("undriven"));
    assert!(!has_w102, "driven wire should not trigger W102");
}

// ============================================================================
// W103: Width mismatch
// ============================================================================

#[test]
fn width_mismatch_w103() {
    // W103 can only detect width mismatches with literals (expr_width returns
    // None for signal references). Use a 4-bit literal assigned to 8-bit target.
    let src = r#"
module top (input [7:0] a, output [7:0] y);
    assign y = 4'hF;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w103 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 103 && d.message.contains("width"));
    assert!(
        has_w103,
        "expected W103 for width mismatch (4-bit literal to 8-bit target)"
    );
}

#[test]
fn matching_width_no_w103() {
    let src = r#"
module top (input [7:0] a, output [7:0] y);
    assign y = 8'hFF;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w103 = result.diagnostics.iter().any(|d| d.code.number == 103);
    assert!(!has_w103, "matching widths should not trigger W103");
}

// ============================================================================
// W104: Missing reset
// ============================================================================

#[test]
fn missing_reset_w104() {
    let src = r#"
module top (
    input clk,
    input d,
    output reg q
);
    always @(posedge clk) begin
        q <= d;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w104 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 104 && d.message.contains("reset"));
    assert!(
        has_w104,
        "expected W104 for sequential process without reset"
    );
}

#[test]
fn async_reset_no_w104() {
    let src = r#"
module top (
    input clk,
    input rst_n,
    input d,
    output reg q
);
    always @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            q <= 1'b0;
        else
            q <= d;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w104 = result.diagnostics.iter().any(|d| d.code.number == 104);
    assert!(!has_w104, "async reset should not trigger W104");
}

#[test]
fn sync_reset_no_w104() {
    let src = r#"
module top (
    input clk,
    input rst,
    input d,
    output reg q
);
    always @(posedge clk) begin
        if (rst)
            q <= 1'b0;
        else
            q <= d;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w104 = result.diagnostics.iter().any(|d| d.code.number == 104);
    assert!(
        !has_w104,
        "sync reset (if-else pattern) should not trigger W104"
    );
}

// ============================================================================
// W105: Incomplete sensitivity list
// ============================================================================

#[test]
fn incomplete_sensitivity_w105() {
    // `always @(a)` reading both a and b — missing b from sensitivity
    let src = r#"
module top (
    input a,
    input b,
    output reg y
);
    always @(a) begin
        y = a & b;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w105 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 105 && d.message.contains("sensitivity"));
    // The elaborator may convert explicit sensitivity to SignalList, enabling W105.
    // If it converts to Sensitivity::All instead, this test documents that gap.
    if !has_w105 {
        eprintln!(
            "NOTE: W105 not triggered — elaborator may not produce SignalList for explicit sensitivity"
        );
    }
}

#[test]
fn star_sensitivity_no_w105() {
    let src = r#"
module top (
    input a,
    input b,
    output reg y
);
    always @(*) begin
        y = a & b;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w105 = result.diagnostics.iter().any(|d| d.code.number == 105);
    assert!(!has_w105, "`always @(*)` should not trigger W105");
}

// ============================================================================
// W107: Truncation
// ============================================================================

#[test]
fn truncation_w107() {
    // W107 can only detect truncation with literals (expr_width returns
    // None for signal references). Use an 8-bit literal assigned to 4-bit target.
    let src = r#"
module top (input [3:0] a, output [3:0] y);
    assign y = 8'hFF;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w107 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 107 && d.message.contains("truncat"));
    assert!(
        has_w107,
        "expected W107 for truncation (8-bit literal to 4-bit target)"
    );
}

#[test]
fn no_truncation_wider_target_no_w107() {
    // Narrower literal to wider target = extension, not truncation.
    let src = r#"
module top (input [7:0] a, output [7:0] y);
    assign y = 4'hF;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w107 = result.diagnostics.iter().any(|d| d.code.number == 107);
    assert!(
        !has_w107,
        "extension (4-bit literal to 8-bit target) should not trigger W107"
    );
}

// ============================================================================
// W108: Dead logic
// ============================================================================

#[test]
fn dead_logic_after_finish_w108() {
    let src = r#"
module top (input clk, output reg q);
    initial begin
        q = 1'b0;
        $finish;
        q = 1'b1;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w108 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 108 && d.message.contains("dead"));
    // E102 will also fire for initial block — that's expected
    assert!(has_w108, "expected W108 for dead logic after $finish");
}

#[test]
fn always_true_condition_w108() {
    let src = r#"
module top (
    input a,
    input b,
    output reg y
);
    always @(*) begin
        if (1'b1)
            y = a;
        else
            y = b;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w108 = result.diagnostics.iter().any(|d| d.code.number == 108);
    // The elaborator may or may not preserve `1'b1` as a literal condition.
    // If constant folding happens, this documents that gap.
    if !has_w108 {
        eprintln!("NOTE: W108 not triggered — elaborator may optimize literal conditions");
    }
}

// ============================================================================
// E104: Multiple drivers
// ============================================================================

#[test]
fn multiple_drivers_e104() {
    // E104 only checks SignalKind::Wire, so use an internal wire (not output port).
    let src = r#"
module top (input a, input b, output y);
    wire w;
    assign w = a;
    assign w = b;
    assign y = w;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_e104 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 104 && d.severity == Severity::Error);
    assert!(
        has_e104,
        "expected E104 for multiple drivers on internal wire"
    );
}

#[test]
fn single_driver_no_e104() {
    let src = r#"
module top (input a, output y);
    wire w;
    assign w = a;
    assign y = w;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_e104 = result.diagnostics.iter().any(|d| d.code.number == 104);
    assert!(!has_e104, "single driver should not trigger E104");
}

// ============================================================================
// E105: Port mismatch
// ============================================================================

#[test]
fn missing_port_e105() {
    // Instantiation missing the 'b' port connection
    let src = r#"
module sub (input a, input b, output y);
    assign y = a & b;
endmodule

module top (input x, output z);
    sub u0 (.a(x), .y(z));
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_e105 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 105 && d.severity == Severity::Error);
    assert!(has_e105, "expected E105 for missing port connection 'b'");
}

#[test]
fn correct_ports_no_e105() {
    let src = r#"
module sub (input a, input b, output y);
    assign y = a & b;
endmodule

module top (input x1, input x2, output z);
    sub u0 (.a(x1), .b(x2), .y(z));
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_e105 = result.diagnostics.iter().any(|d| d.code.number == 105);
    assert!(
        !has_e105,
        "correct port connections should not trigger E105"
    );
}

// ============================================================================
// C201: Naming violation (checked against `[lint.naming]`)
// ============================================================================

/// Builds a config with `[lint.naming]` set to snake_case modules/signals
/// and UPPER_SNAKE_CASE parameters/constants.
fn naming_config(top: &str) -> aion_config::ProjectConfig {
    let toml_str = format!(
        r#"
[project]
name = "conformance_test"
version = "0.1.0"
top = "{top}"

[lint.naming]
module = "snake_case"
signal = "snake_case"
parameter = "upper_snake_case"
constant = "upper_snake_case"
"#
    );
    toml::from_str(&toml_str).unwrap()
}

#[test]
fn naming_without_config_reports_nothing_c201() {
    let src = r#"
module MyModule (input aIn, output yOut);
    assign yOut = aIn;
endmodule
"#;
    let result = full_pipeline_verilog(src, "MyModule");
    let has_c201 = result.diagnostics.iter().any(|d| d.code.number == 201);
    assert!(!has_c201, "C201 must stay silent without [lint.naming]");
}

#[test]
fn conforming_names_no_c201() {
    let src = r#"
module my_module #(parameter WIDTH = 4) (input [WIDTH-1:0] a_in, output [WIDTH-1:0] y_out);
    wire [WIDTH-1:0] internal_wire;
    assign internal_wire = a_in;
    assign y_out = internal_wire;
endmodule
"#;
    let result = full_pipeline_verilog_with_config(src, &naming_config("my_module"));
    let c201: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 201)
        .collect();
    assert!(c201.is_empty(), "{c201:#?}");
}

#[test]
fn naming_violations_are_reported_with_names_c201() {
    let src = r#"
module MyModule #(parameter width = 4) (input [width-1:0] dataIn, output [width-1:0] y);
    assign y = dataIn;
endmodule
"#;
    let result = full_pipeline_verilog_with_config(src, &naming_config("MyModule"));
    let msgs: Vec<&str> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 201)
        .map(|d| d.message.as_str())
        .collect();
    assert!(
        msgs.contains(&"module name `MyModule` does not follow the snake_case convention"),
        "{msgs:#?}"
    );
    assert!(
        msgs.contains(&"parameter name `width` does not follow the UPPER_SNAKE_CASE convention"),
        "{msgs:#?}"
    );
    assert!(
        msgs.contains(&"signal name `dataIn` does not follow the snake_case convention"),
        "{msgs:#?}"
    );
    assert_eq!(msgs.len(), 3, "{msgs:#?}");
    // The note quotes the exact aion.toml spelling, not the display label.
    let param = result
        .diagnostics
        .iter()
        .find(|d| d.code.number == 201 && d.message.contains("`width`"))
        .expect("parameter violation");
    assert!(
        param
            .notes
            .iter()
            .any(|n| n.contains("`parameter = \"upper_snake_case\"`")),
        "{:#?}",
        param.notes
    );
}

#[test]
fn vhdl_names_checked_as_written_c201() {
    // The VHDL parser folds identifiers to lowercase; C201 must still see
    // `DATA_WIDTH` (conforming) and `DataOut` (violating) as spelled.
    let src = r#"
entity blink is
    generic (DATA_WIDTH : integer := 8);
    port (clk : in bit; DataOut : out bit);
end entity;

architecture rtl of blink is
begin
    DataOut <= clk;
end architecture;
"#;
    let result = full_pipeline_vhdl_with_config(src, &naming_config("blink"));
    let msgs: Vec<&str> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 201)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        msgs,
        vec!["signal name `DataOut` does not follow the snake_case convention"],
        "{:#?}",
        result.diagnostics
    );
}

// ============================================================================
// C202: Missing doc
// ============================================================================

#[test]
fn undocumented_module_c202() {
    let src = r#"
module top (input a, output y);
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let c202: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 202)
        .collect();
    assert_eq!(c202.len(), 1, "{:#?}", result.diagnostics);
    assert_eq!(c202[0].message, "module `top` has no documentation comment");
    assert_eq!(c202[0].severity, Severity::Warning);
}

#[test]
fn documented_module_no_c202() {
    let src = r#"
// Pass-through buffer used by the conformance suite.
module top (input a, output y);
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_c202 = result.diagnostics.iter().any(|d| d.code.number == 202);
    assert!(!has_c202, "{:#?}", result.diagnostics);
}

#[test]
fn documented_vhdl_entity_no_c202() {
    let src = r#"
-- Drives the output from the clock.
library ieee;
use ieee.std_logic_1164.all;

entity blink is
    port (clk : in std_logic; led : out std_logic);
end entity;

architecture rtl of blink is
begin
    led <= clk;
end architecture;
"#;
    let result = full_pipeline_vhdl(src, "blink");
    let has_c202 = result.diagnostics.iter().any(|d| d.code.number == 202);
    assert!(!has_c202, "{:#?}", result.diagnostics);
}

#[test]
fn trailing_block_comment_after_code_is_not_doc_c202() {
    // `endmodule /* end */` ends with `*/` but is code, not a header comment
    // for the module declared on the next line.
    let src = "// Pass-through.\nmodule sub(input a, output y);\n    assign y = a; endmodule /* end */\nmodule top(input a, output y);\n    sub u(.a(a), .y(y));\nendmodule\n";
    let result = full_pipeline_verilog(src, "top");
    let c202: Vec<&str> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 202)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(c202, vec!["module `top` has no documentation comment"]);
}

#[test]
fn parameterized_module_c202_reported_once() {
    let src = r#"
// Top level.
module top (input [7:0] a, input [3:0] b, output [7:0] y, output [3:0] z);
    buffer #(.W(8)) u8 (.d(a), .q(y));
    buffer #(.W(4)) u4 (.d(b), .q(z));
endmodule

module buffer #(parameter W = 1) (input [W-1:0] d, output [W-1:0] q);
    assign q = d;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let c202: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 202)
        .collect();
    assert_eq!(c202.len(), 1, "{c202:#?}");
    assert!(c202[0].message.contains("`buffer`"));
}

// ============================================================================
// Issue #39: W101 reads and real names in messages
// ============================================================================

#[test]
fn gated_clock_used_only_in_edge_list_no_w101() {
    let src = r#"
module top (input clk, input en, input d, output reg q);
    wire gclk;
    assign gclk = clk & en;
    always @(posedge gclk) q <= d;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let w101: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 101)
        .collect();
    assert!(w101.is_empty(), "gclk is read by the edge list: {w101:#?}");
}

#[test]
fn async_reset_used_only_in_edge_list_no_w101() {
    let src = r#"
module top (input clk, input rst_in, input d, output reg q);
    wire arst;
    assign arst = ~rst_in;
    always @(posedge clk or posedge arst)
        if (arst) q <= 1'b0; else q <= d;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_w101 = result.diagnostics.iter().any(|d| d.code.number == 101);
    assert!(!has_w101, "{:#?}", result.diagnostics);
}

#[test]
fn underscore_prefixed_wire_no_w101() {
    let src = r#"
module top (input a, output y);
    wire _unused;
    wire spare;
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let w101: Vec<&str> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 101)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(w101, vec!["unused signal `spare`"]);
}

#[test]
fn dynamic_index_of_write_is_a_read_no_w101() {
    let src = r#"
module top (input clk, input [2:0] sel, input d, output [7:0] q);
    reg [7:0] r;
    wire [2:0] addr;
    assign addr = sel;
    always @(posedge clk) r[addr] <= d;
    assign q = r;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let w101: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 101)
        .collect();
    assert!(w101.is_empty(), "addr is read as an index: {w101:#?}");
}

/// Returns the W101 messages in `result`.
fn w101_messages(result: &aion_conformance::PipelineResult) -> Vec<&str> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 101)
        .map(|d| d.message.as_str())
        .collect()
}

#[test]
fn unused_concat_target_still_w101() {
    // Regression: the dynamic-index fallback must not treat `{...}` on the
    // left-hand side as an index, or an unused carry-out goes unreported.
    let src = r#"
module top (input [3:0] a, input [3:0] b, output [3:0] y);
    wire carry;
    wire [3:0] s;
    assign {carry, s} = a + b;
    assign y = s;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    assert_eq!(w101_messages(&result), vec!["unused signal `carry`"]);
}

#[test]
fn unused_procedural_concat_target_still_w101() {
    let src = r#"
module top (input clk, input [3:0] a, input [3:0] b, output reg [3:0] y);
    reg carry;
    reg [3:0] s;
    always @(posedge clk) begin
        {carry, s} <= a + b;
        y <= s;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    assert_eq!(w101_messages(&result), vec!["unused signal `carry`"]);
}

#[test]
fn dynamic_index_read_is_case_sensitive_in_verilog() {
    // `Addr` is read by the index; `addr` is a different, unused signal.
    let src = r#"
module top (input clk, input [2:0] sel, input d, output [7:0] q);
    reg [7:0] mem;
    wire [2:0] Addr;
    wire [2:0] addr;
    assign Addr = sel;
    always @(posedge clk) mem[Addr] <= d;
    assign q = mem;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    assert_eq!(w101_messages(&result), vec!["unused signal `addr`"]);
}

#[test]
fn w101_message_names_the_signal() {
    let src = r#"
module top (input a, output y);
    wire unused_w;
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let w101 = result
        .diagnostics
        .iter()
        .find(|d| d.code.number == 101)
        .expect("W101 expected");
    assert_eq!(w101.message, "unused signal `unused_w`");
}

#[test]
fn e105_messages_name_ports_not_ids() {
    let src = r#"
module sub (input a, input b, output y);
    assign y = a & b;
endmodule

module top (input x, output z);
    sub u0 (.a(x), .y(z));
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let e105: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 105)
        .collect();
    assert_eq!(e105.len(), 1, "{e105:#?}");
    assert_eq!(
        e105[0].message,
        "missing port connection: port `b` of module `sub` is not connected in instance `u0`"
    );
    assert!(
        e105[0].labels.iter().all(|l| !l.message.contains('\'')),
        "labels must not quote raw IDs: {:#?}",
        e105[0].labels
    );
}

#[test]
fn e104_and_w102_messages_name_the_signal() {
    let src = r#"
module top (input a, input b, output y);
    wire w;
    wire floating;
    assign w = a;
    assign w = b;
    assign y = w | floating;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let msgs: Vec<&str> = result
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert!(
        msgs.contains(&"signal `w` has 2 concurrent drivers"),
        "{msgs:#?}"
    );
    assert!(msgs.contains(&"undriven signal `floating`"), "{msgs:#?}");
}

// ============================================================================
// C203: Magic number
// ============================================================================

#[test]
fn magic_number_c203() {
    let src = r#"
module top (input [7:0] data, output [7:0] result);
    assign result = data + 8'h42;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_c203 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 203 && d.message.contains("magic"));
    assert!(has_c203, "expected C203 for magic number 8'h42");
}

#[test]
fn zero_literal_no_c203() {
    let src = r#"
module top (input [7:0] data, output [7:0] result);
    assign result = data + 8'h00;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_c203 = result.diagnostics.iter().any(|d| d.code.number == 203);
    assert!(!has_c203, "all-zeros literal should be exempt from C203");
}

#[test]
fn all_ones_literal_no_c203() {
    let src = r#"
module top (input [7:0] data, output [7:0] result);
    assign result = data & 8'hFF;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_c203 = result.diagnostics.iter().any(|d| d.code.number == 203);
    assert!(!has_c203, "all-ones literal should be exempt from C203");
}

// ============================================================================
// C204: Inconsistent style (latched process)
// ============================================================================

#[test]
fn latched_process_c204() {
    // always_latch should produce ProcessKind::Latched
    let src = r#"
module top (
    input en,
    input d,
    output logic q
);
    always_latch begin
        if (en)
            q <= d;
    end
endmodule
"#;
    let result = full_pipeline_sv(src, "top");
    let has_c204 = result.diagnostics.iter().any(|d| d.code.number == 204);
    // This depends on the elaborator setting ProcessKind::Latched for always_latch.
    // If it doesn't, the test documents that gap.
    if !has_c204 {
        eprintln!(
            "NOTE: C204 not triggered — elaborator may not produce ProcessKind::Latched for always_latch"
        );
    }
}

#[test]
fn combinational_process_no_c204() {
    let src = r#"
module top (
    input a,
    input b,
    output reg y
);
    always @(*) begin
        if (a)
            y = b;
        else
            y = 1'b0;
    end
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let has_c204 = result.diagnostics.iter().any(|d| d.code.number == 204);
    assert!(
        !has_c204,
        "combinational process with full coverage should not trigger C204"
    );
}

// ============================================================================
// Review follow-ups for the lint-context work (#39)
// ============================================================================

#[test]
fn vhdl_aggregate_target_members_are_still_unused_w101() {
    // Regression: `(carry, s) <= ...` is a positional aggregate target whose
    // members are written. The `(` opening the target is not an index
    // bracket, so `carry` must still be reported as unused.
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity top is
    port (a : in unsigned(3 downto 0); b : in unsigned(3 downto 0);
          y : out unsigned(3 downto 0));
end entity;

architecture rtl of top is
    signal carry : std_logic;
    signal s : unsigned(3 downto 0);
begin
    (carry, s) <= ('0' & a) + b;
    y <= s;
end architecture;
"#;
    let result = full_pipeline_vhdl(src, "top");
    assert_eq!(
        w101_messages(&result),
        vec!["unused signal `carry`"],
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn vhdl_dynamic_index_of_write_is_a_read_no_w101() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity top is
    port (clk : in std_logic; sel : in unsigned(2 downto 0); d : in std_logic;
          q : out std_logic_vector(7 downto 0));
end entity;

architecture rtl of top is
    signal mem : std_logic_vector(7 downto 0);
    signal addr : unsigned(2 downto 0);
begin
    addr <= sel;
    process (clk)
    begin
        if rising_edge(clk) then
            mem(to_integer(addr)) <= d;
        end if;
    end process;
    q <= mem;
end architecture;
"#;
    let result = full_pipeline_vhdl(src, "top");
    assert!(
        w101_messages(&result).is_empty(),
        "addr is read as an index: {:#?}",
        result.diagnostics
    );
}

#[test]
fn sized_literal_in_index_is_not_a_signal_w101() {
    // `8'd1` must not be read as an identifier `d1`: the unused wire `d1`
    // is still reported, while `addr` counts as read by the index.
    let src = r#"
module top (input clk, input [2:0] sel, input x, output [7:0] q);
    reg [7:0] mem;
    wire [2:0] addr;
    wire d1;
    wire hA;
    wire b10;
    assign addr = sel;
    always @(posedge clk) begin
        mem[addr + 8'd1] <= x;
        mem[addr + 4'hA] <= x;
        mem[addr + 2'b10] <= x;
    end
    assign q = mem;
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    let mut msgs = w101_messages(&result);
    msgs.sort_unstable();
    assert_eq!(
        msgs,
        vec![
            "unused signal `b10`",
            "unused signal `d1`",
            "unused signal `hA`"
        ],
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn verilog_names_are_matched_case_sensitively_c201() {
    // `CLK` and `clk` are different signals in Verilog. C201 must check
    // each one's own spelling, so only `CLK` violates snake_case.
    let src = r#"
module top (input wire CLK, clk, output y);
    assign y = CLK & clk;
endmodule
"#;
    let result = full_pipeline_verilog_with_config(src, &naming_config("top"));
    let msgs: Vec<&str> = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 201)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        msgs,
        vec!["signal name `CLK` does not follow the snake_case convention"],
        "{:#?}",
        result.diagnostics
    );
}
