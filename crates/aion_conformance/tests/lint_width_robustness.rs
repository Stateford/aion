//! Regression tests for issue #38: width computation in the W103/W107 lint
//! rules must never panic on user input, and a rule that does panic must not
//! abort the whole lint run.
//!
//! Each test drives the full parse → elaborate → lint pipeline on source that
//! previously crashed the linter (ascending slices, VHDL `to` ranges, indexed
//! part-selects, huge replication counts) or produced port-order-dependent
//! results.

use aion_conformance::{full_pipeline_verilog, full_pipeline_vhdl, PipelineResult};
use aion_diagnostics::{Category, Diagnostic};

/// Returns the diagnostics carrying the given warning number (`W<number>`).
fn warnings(result: &PipelineResult, number: u16) -> Vec<&Diagnostic> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.code.category == Category::Warning && d.code.number == number)
        .collect()
}

/// Asserts that the lint engine did not report an internal (panic) error.
fn assert_no_internal_error(result: &PipelineResult) {
    let internal: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code == aion_lint::LINT_INTERNAL_ERROR)
        .collect();
    assert!(
        internal.is_empty(),
        "lint rules must not panic, got: {internal:#?}"
    );
}

#[test]
fn indexed_part_select_does_not_panic() {
    // `a[0 +: 8]` is lowered as Slice { high: 0, low: 8 }, which used to
    // underflow `high - low + 1`. Until the elaborator lowers part-selects to
    // real bounds (tracked separately as LINT-4), the computed width is a
    // meaningless 9 bits. W103 may still report it (pre-existing LINT-4
    // behavior), but W107 must not raise a false truncation.
    let src = r#"
module t(input [15:0] a, output [7:0] y);
    assign y = a[0 +: 8];
endmodule
"#;
    let result = full_pipeline_verilog(src, "t");
    assert_no_internal_error(&result);
    assert!(
        warnings(&result, 107).is_empty(),
        "part-select must not give a false W107: {:#?}",
        result.diagnostics
    );
}

#[test]
fn part_select_patterns_no_false_w107() {
    // Byte-lane concatenation, high part-selects and constant-index memory
    // writes all produced false W107 truncations when RHS slice bounds were
    // trusted.
    let src = r#"
module t(input clk, input [31:0] a, output [31:0] y, output [7:0] z);
    reg [7:0] mem [0:3];
    assign y = {a[0 +: 8], a[8 +: 8], a[16 +: 8], a[24 +: 8]};
    assign z = a[24 +: 8];
    always @(posedge clk) mem[1] <= a[7:0];
endmodule
"#;
    let result = full_pipeline_verilog(src, "t");
    assert_no_internal_error(&result);
    assert!(
        warnings(&result, 107).is_empty(),
        "no W107 expected: {:#?}",
        result.diagnostics
    );
}

#[test]
fn ascending_lhs_slice_no_panic_and_no_false_w103() {
    // `y[0:3]` on an ascending `[0:7]` vector is a 4-bit target.
    let src = r#"
module t(input [3:0] a, output [0:7] y);
    assign y[0:3] = a;
    assign y[4:7] = a;
endmodule
"#;
    let result = full_pipeline_verilog(src, "t");
    assert_no_internal_error(&result);
    assert!(
        warnings(&result, 103).is_empty(),
        "4-bit ascending slice <- 4-bit signal is not a mismatch: {:#?}",
        result.diagnostics
    );
    assert!(warnings(&result, 107).is_empty());
}

#[test]
fn ascending_lhs_slice_real_mismatch_still_detected() {
    // 8-bit literal into a 4-bit ascending slice: W103 and W107 must fire,
    // proving the ascending width is computed (4), not skipped or wrapped.
    let src = r#"
module t(output [0:7] y);
    assign y[0:3] = 8'hFF;
    assign y[4:7] = 4'h0;
endmodule
"#;
    let result = full_pipeline_verilog(src, "t");
    assert_no_internal_error(&result);
    let w103 = warnings(&result, 103);
    assert_eq!(w103.len(), 1, "{:#?}", result.diagnostics);
    assert!(w103[0].message.contains("LHS is 4 bits, RHS is 8 bits"));
    let w107 = warnings(&result, 107);
    assert_eq!(w107.len(), 1, "{:#?}", result.diagnostics);
    assert!(w107[0].message.contains("losing 4 bits"));
}

#[test]
fn vhdl_ascending_range_does_not_panic() {
    let src = r#"
library ieee;
use ieee.std_logic_1164.all;

entity t is
    port (
        a : in  std_logic_vector(0 to 7);
        y : out std_logic_vector(0 to 3)
    );
end entity t;

architecture rtl of t is
begin
    y <= a(0 to 3);
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "t");
    assert_no_internal_error(&result);
    assert!(
        warnings(&result, 103).is_empty(),
        "4-bit slice into 4-bit port is not a mismatch: {:#?}",
        result.diagnostics
    );
    assert!(warnings(&result, 107).is_empty());
}

#[test]
fn huge_replication_count_does_not_panic() {
    // 1e9 * 8 overflows u32; the width must be treated as unknown.
    let src = r#"
module t(output [7:0] y);
    assign y = {1000000000{8'hFF}};
endmodule
"#;
    let result = full_pipeline_verilog(src, "t");
    assert_no_internal_error(&result);
    assert!(warnings(&result, 103).is_empty());
    assert!(warnings(&result, 107).is_empty());
}

#[test]
fn w107_comparison_no_false_truncation() {
    // `a == b` is stamped with the TypeId(0) placeholder by the elaborator.
    // With `a` as the first port, TypeId(0) aliases the 8-bit port type and
    // W107 used to report a bogus 8 -> 1 truncation.
    let src = r#"
module t(input [7:0] a, input [7:0] b, output y);
    assign y = a == b;
endmodule
"#;
    let result = full_pipeline_verilog(src, "t");
    assert_no_internal_error(&result);
    assert!(
        warnings(&result, 107).is_empty(),
        "comparison result must not be reported as truncation: {:#?}",
        result.diagnostics
    );
}

#[test]
fn w107_result_independent_of_port_order() {
    // The same truncating assignment with different port orders must give the
    // same W107 verdict (the placeholder type must never be trusted).
    let a_first = r#"
module t(input [7:0] a, input [7:0] b, input c, output [7:0] y);
    assign y[3:0] = a + b;
    assign y[7:4] = {3'b0, c};
endmodule
"#;
    let c_first = r#"
module t(input c, input [7:0] a, input [7:0] b, output [7:0] y);
    assign y[3:0] = a + b;
    assign y[7:4] = {3'b0, c};
endmodule
"#;
    let r1 = full_pipeline_verilog(a_first, "t");
    let r2 = full_pipeline_verilog(c_first, "t");
    assert_no_internal_error(&r1);
    assert_no_internal_error(&r2);
    assert_eq!(
        warnings(&r1, 107).len(),
        warnings(&r2, 107).len(),
        "W107 must not depend on port order:\n{:#?}\n---\n{:#?}",
        r1.diagnostics,
        r2.diagnostics
    );
}
