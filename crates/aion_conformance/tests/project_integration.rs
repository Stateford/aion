//! Integration tests for multi-file pipelines and on-disk project workflows.
//!
//! These tests exercise the full pipeline (parse → elaborate → lint) across
//! multiple source files, both in-memory and from on-disk project layouts.

use aion_conformance::{
    full_pipeline_sv_multifile, full_pipeline_sv_multifile_with_config, make_config,
    make_config_with_lint,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helper: discover source files in a directory
// ---------------------------------------------------------------------------

/// Recursively discovers HDL source files (`.v`, `.sv`, `.vhd`, `.vhdl`) in `dir`.
fn discover_source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return files;
    }
    collect_hdl_files(dir, &mut files);
    files.sort();
    files
}

fn collect_hdl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_hdl_files(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext, "v" | "sv" | "vhd" | "vhdl"))
        {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: run pipeline on discovered disk files
// ---------------------------------------------------------------------------

/// Parses all SV files from a directory, elaborates, and lints.
fn pipeline_from_disk_sv(
    src_dir: &Path,
    config: &aion_config::ProjectConfig,
) -> aion_conformance::PipelineResult {
    let files = discover_source_files(src_dir);
    let file_contents: Vec<(String, String)> = files
        .iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let content = fs::read_to_string(p).unwrap();
            (name, content)
        })
        .collect();

    let refs: Vec<(&str, &str)> = file_contents
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_str()))
        .collect();

    full_pipeline_sv_multifile_with_config(&refs, config)
}

// ===========================================================================
// Category A: Multi-file pipeline (in-memory)
// ===========================================================================

#[test]
fn multifile_hierarchy_resolves() {
    let files = &[
        (
            "clk_divider.sv",
            r#"
module clk_divider (
    input  logic        clk,
    input  logic        rst_n,
    output logic        tick
);
    logic [23:0] cnt;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            cnt  <= 24'h000000;
            tick <= 1'b0;
        end else begin
            if (cnt == 24'hFFFFFF) begin
                cnt  <= 24'h000000;
                tick <= 1'b1;
            end else begin
                cnt  <= cnt + 24'h000001;
                tick <= 1'b0;
            end
        end
    end
endmodule
"#,
        ),
        (
            "counter.sv",
            r#"
module counter (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    output logic [7:0] count
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            count <= 8'h00;
        else if (en)
            count <= count + 8'h01;
    end
endmodule
"#,
        ),
        (
            "led_ctrl.sv",
            r#"
module led_ctrl (
    input  logic [7:0] count,
    output logic [7:0] leds
);
    always_comb begin
        leds[0] = count[0];
        leds[1] = count[1];
        leds[2] = count[2];
        leds[3] = count[3];
        leds[4] = count[4];
        leds[5] = count[5];
        leds[6] = count[6];
        leds[7] = count[7];
    end
endmodule
"#,
        ),
        (
            "top.sv",
            r#"
module blinky_top (
    input  logic       clk,
    input  logic       rst_n,
    output logic [7:0] leds
);
    logic       tick;
    logic [7:0] count;

    clk_divider u_clk_div (
        .clk   (clk),
        .rst_n (rst_n),
        .tick  (tick)
    );

    counter u_counter (
        .clk   (clk),
        .rst_n (rst_n),
        .en    (tick),
        .count (count)
    );

    led_ctrl u_led_ctrl (
        .count (count),
        .leds  (leds)
    );
endmodule
"#,
        ),
    ];

    let result = full_pipeline_sv_multifile(files, "blinky_top");
    assert!(
        !result.has_errors,
        "unexpected errors: {:?}",
        result.diagnostics
    );
    // 4 modules: blinky_top + 3 submodules
    assert_eq!(result.design.module_count(), 4);
    // Top module should have 3 cell instances
    let top = result.design.top_module();
    assert_eq!(top.cells.len(), 3);
}

#[test]
fn multifile_clean_lint() {
    let files = &[
        (
            "sub.sv",
            r#"
module sub (
    input  logic [7:0] a,
    output logic [7:0] b
);
    always_comb begin
        b = a;
    end
endmodule
"#,
        ),
        (
            "top.sv",
            r#"
module top (
    input  logic [7:0] x,
    output logic [7:0] y
);
    sub u_sub (
        .a (x),
        .b (y)
    );
endmodule
"#,
        ),
    ];

    let result = full_pipeline_sv_multifile(files, "top");
    assert!(
        !result.has_errors,
        "unexpected errors: {:?}",
        result.diagnostics
    );
    assert_eq!(result.error_count, 0);
}

#[test]
fn multifile_missing_submodule() {
    // Only provide the top module — submodule "missing_mod" is not defined
    let files = &[(
        "top.sv",
        r#"
module top (
    input logic clk
);
    missing_mod u_inst (
        .clk (clk)
    );
endmodule
"#,
    )];

    let result = full_pipeline_sv_multifile(files, "top");
    // Should produce an elaboration error for unknown module
    assert!(result.has_errors, "expected errors for missing submodule");
    assert!(result.error_count > 0);
}

// ===========================================================================
// Category B: On-disk project (using tempfile)
// ===========================================================================

/// Creates a temp directory with aion.toml and source files, returns (TempDir, src_path).
fn create_temp_project(toml_content: &str, files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Write aion.toml
    fs::write(root.join("aion.toml"), toml_content).unwrap();

    // Write source files
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    for (name, content) in files {
        let path = src_dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }

    (tmp, src_dir)
}

#[test]
fn disk_project_discovery_and_pipeline() {
    let toml = r#"
[project]
name = "test_proj"
version = "0.1.0"
top = "top"
"#;
    let files = &[
        (
            "mod_a.sv",
            "module mod_a (input logic a, output logic b); always_comb b = a; endmodule",
        ),
        (
            "mod_b.sv",
            "module mod_b (input logic a, output logic b); always_comb b = a; endmodule",
        ),
        (
            "top.sv",
            r#"module top (input logic x, output logic y);
    logic mid;
    mod_a u_a (.a(x), .b(mid));
    mod_b u_b (.a(mid), .b(y));
endmodule"#,
        ),
    ];

    let (tmp, src_dir) = create_temp_project(toml, files);

    // Verify file discovery
    let discovered = discover_source_files(&src_dir);
    assert_eq!(discovered.len(), 3);

    // Load config
    let config = aion_config::load_config(tmp.path()).unwrap();
    assert_eq!(config.project.top, "top");

    // Run pipeline
    let result = pipeline_from_disk_sv(&src_dir, &config);
    assert!(
        !result.has_errors,
        "unexpected errors: {:?}",
        result.diagnostics
    );
    assert_eq!(result.design.module_count(), 3);
}

#[test]
fn disk_project_subdirectory_discovery() {
    let toml = r#"
[project]
name = "nested"
version = "0.1.0"
top = "top"
"#;
    let files = &[
        (
            "sub/inner.sv",
            "module inner (input logic a, output logic b); always_comb b = a; endmodule",
        ),
        (
            "top.sv",
            "module top (input logic x, output logic y); inner u_i (.a(x), .b(y)); endmodule",
        ),
    ];

    let (_tmp, src_dir) = create_temp_project(toml, files);

    let discovered = discover_source_files(&src_dir);
    assert_eq!(discovered.len(), 2, "should find files in subdirectories");

    // Verify pipeline works with discovered files
    let config = make_config("top");
    let result = pipeline_from_disk_sv(&src_dir, &config);
    assert!(
        !result.has_errors,
        "unexpected errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn disk_project_lint_issues() {
    let toml = r#"
[project]
name = "lint_proj"
version = "0.1.0"
top = "top"
"#;
    // top module has an unused wire → should trigger W101
    let files = &[(
        "top.sv",
        r#"module top (input logic clk);
    wire unused_w;
endmodule"#,
    )];

    let (_tmp, src_dir) = create_temp_project(toml, files);
    let config = make_config("top");
    let result = pipeline_from_disk_sv(&src_dir, &config);

    // W101 should fire for unused wire
    let w101_count = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 101)
        .count();
    assert!(w101_count > 0, "expected W101 for unused wire");
}

#[test]
fn disk_project_lint_config_allow() {
    let toml = r#"
[project]
name = "allow_proj"
version = "0.1.0"
top = "top"

[lint]
allow = ["unused-signal"]
"#;
    let files = &[(
        "top.sv",
        r#"module top (input logic clk);
    wire unused_w;
endmodule"#,
    )];

    let (tmp, src_dir) = create_temp_project(toml, files);
    let config = aion_config::load_config(tmp.path()).unwrap();
    let result = pipeline_from_disk_sv(&src_dir, &config);

    // W101 should be suppressed
    let w101_count = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 101)
        .count();
    assert_eq!(w101_count, 0, "W101 should be suppressed by allow config");
}

#[test]
fn disk_project_empty_src() {
    let toml = r#"
[project]
name = "empty_proj"
version = "0.1.0"
top = "top"
"#;
    // No source files at all
    let files: &[(&str, &str)] = &[];
    let (_tmp, src_dir) = create_temp_project(toml, files);

    let discovered = discover_source_files(&src_dir);
    assert_eq!(discovered.len(), 0, "no files should be discovered");

    // Pipeline with zero files should not panic
    let config = make_config("top");
    let result = full_pipeline_sv_multifile_with_config(&[], &config);
    // Should have an error because top module doesn't exist
    assert!(result.has_errors);
}

#[test]
fn disk_project_top_not_found() {
    let toml = r#"
[project]
name = "bad_top"
version = "0.1.0"
top = "nonexistent"
"#;
    let files = &[("mod.sv", "module some_mod (input logic a); endmodule")];

    let (_tmp, src_dir) = create_temp_project(toml, files);
    let config = make_config("nonexistent");
    let result = pipeline_from_disk_sv(&src_dir, &config);

    // Should error: top module not found
    assert!(result.has_errors, "expected error for missing top module");
    let has_top_error = result.diagnostics.iter().any(|d| d.code.number == 206);
    assert!(
        has_top_error,
        "expected E206 (top not found), got: {:?}",
        result.diagnostics
    );
}

// ===========================================================================
// Category C: Committed example verification
// ===========================================================================

#[test]
fn example_blinky_soc_lints_clean() {
    // Find the examples/blinky_soc directory relative to the workspace root
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    let example_dir = workspace_root.join("examples/blinky_soc");

    assert!(
        example_dir.join("aion.toml").exists(),
        "examples/blinky_soc/aion.toml not found at {:?}",
        example_dir
    );

    // Load config from the example project
    let config = aion_config::load_config(&example_dir).unwrap();
    assert_eq!(config.project.name, "blinky_soc");
    assert_eq!(config.project.top, "blinky_top");

    // Discover and parse source files
    let src_dir = example_dir.join("src");
    let result = pipeline_from_disk_sv(&src_dir, &config);

    assert!(
        !result.has_errors,
        "blinky_soc example should have no errors, got: {:?}",
        result.diagnostics
    );
    assert_eq!(
        result.design.module_count(),
        4,
        "expected 4 modules: blinky_top, clk_divider, counter, led_ctrl"
    );

    let top = result.design.top_module();
    assert_eq!(top.cells.len(), 3, "top should have 3 instantiations");
}

// ===========================================================================
// Additional multi-file edge cases
// ===========================================================================

#[test]
fn multifile_duplicate_module_across_files() {
    // Two files defining the same module name
    let files = &[
        ("a.sv", "module dup (input logic x); endmodule"),
        ("b.sv", "module dup (input logic x); endmodule"),
        ("top.sv", "module top; dup u_d (.x(1'b0)); endmodule"),
    ];

    let result = full_pipeline_sv_multifile(files, "top");
    // Should detect duplicate module definition (E202)
    let has_dup = result.diagnostics.iter().any(|d| d.code.number == 202);
    assert!(
        has_dup,
        "expected E202 (duplicate module), got: {:?}",
        result.diagnostics
    );
}

#[test]
fn multifile_single_file_still_works() {
    // Ensure the multifile API works with just one file
    let files = &[("only.sv", "module only_mod; endmodule")];
    let result = full_pipeline_sv_multifile(files, "only_mod");
    assert!(
        !result.has_errors,
        "single file via multifile API should work: {:?}",
        result.diagnostics
    );
    assert_eq!(result.design.module_count(), 1);
}

#[test]
fn multifile_with_lint_config() {
    // Multi-file with custom lint config
    let files = &[(
        "top.sv",
        r#"module top (input logic clk);
    wire unused_w;
endmodule"#,
    )];

    let config = make_config_with_lint("top", &[], &["unused-signal"]);
    let result = full_pipeline_sv_multifile_with_config(files, &config);

    let w101_count = result
        .diagnostics
        .iter()
        .filter(|d| d.code.number == 101)
        .count();
    assert_eq!(w101_count, 0, "W101 should be suppressed");
}
