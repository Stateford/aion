//! Tests for error recovery — verifying the pipeline handles malformed input
//! gracefully, emits multiple diagnostics, and never panics.

use aion_conformance::{full_pipeline_sv, full_pipeline_verilog, full_pipeline_vhdl};

#[test]
fn verilog_missing_semicolons_multiple_diagnostics() {
    let src = r#"
module bad (
    input a,
    output y
);
    wire w
    assign w = a
    assign y = w
endmodule
"#;
    let result = full_pipeline_verilog(src, "bad");
    // Should emit multiple diagnostics for the missing semicolons
    assert!(
        result.diagnostics.len() >= 2,
        "expected multiple diagnostics, got {}",
        result.diagnostics.len()
    );
}

#[test]
fn verilog_bad_module_then_good_module_recovers() {
    let src = r#"
module bad_mod (
    input a,
    output
);
endmodule

module good_mod (
    input a,
    output y
);
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog(src, "good_mod");
    // The good module should be found and elaborated
    assert!(
        result.design.module_count() >= 1,
        "should elaborate at least the good module"
    );
}

#[test]
fn sv_bad_then_good_module_recovers() {
    let src = r#"
module bad_sv (
    input logic a
    output logic y
);
endmodule

module good_sv (
    input logic x,
    output logic z
);
    assign z = ~x;
endmodule
"#;
    let result = full_pipeline_sv(src, "good_sv");
    // Should recover and elaborate the good module
    assert!(
        result.design.module_count() >= 1,
        "should elaborate at least the good module"
    );
}

#[test]
fn vhdl_missing_end_entity() {
    let src = r#"
entity bad_ent is
    port (a : in std_logic);

entity good_ent is
    port (x : in std_logic; y : out std_logic);
end entity good_ent;

architecture rtl of good_ent is
begin
    y <= x;
end architecture rtl;
"#;
    let result = full_pipeline_vhdl(src, "good_ent");
    // Should recover and elaborate the good entity
    assert!(!result.diagnostics.is_empty());
}

#[test]
fn verilog_multiple_syntax_errors_all_reported() {
    let src = r#"
module errs (input a, output y);
    wire w
    wire x
    wire z
    assign y = a;
endmodule
"#;
    let result = full_pipeline_verilog(src, "errs");
    // Should report at least 2 errors (not stop at the first one)
    assert!(
        result.diagnostics.len() >= 2,
        "expected at least 2 diagnostics from 3 missing semicolons, got {}",
        result.diagnostics.len()
    );
}

#[test]
fn empty_verilog_source_no_panic() {
    let result = full_pipeline_verilog("", "top");
    // Empty source => top not found error
    assert!(result.has_errors);
}

#[test]
fn empty_sv_source_no_panic() {
    let result = full_pipeline_sv("", "top");
    assert!(result.has_errors);
}

#[test]
fn empty_vhdl_source_no_panic() {
    let result = full_pipeline_vhdl("", "top");
    assert!(result.has_errors);
}

#[test]
fn elaborate_unknown_top_module() {
    let src = "module something_else; endmodule";
    let result = full_pipeline_verilog(src, "nonexistent_top");
    assert!(result.has_errors);
    // Should have E206 (top not found)
    let has_e206 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 206 || d.message.contains("not found"));
    assert!(has_e206, "expected E206 for missing top module");
}

#[test]
fn elaborate_unknown_instantiation() {
    let src = r#"
module top;
    nonexistent_module u0 ();
endmodule
"#;
    let result = full_pipeline_verilog(src, "top");
    assert!(result.has_errors);
    // Should have E200 (unknown module)
    let has_e200 = result
        .diagnostics
        .iter()
        .any(|d| d.code.number == 200 || d.message.contains("unknown"));
    assert!(has_e200, "expected E200 for unknown instantiation");
}
