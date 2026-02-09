//! Conformance test helpers for the Aion FPGA toolchain.
//!
//! Provides shared pipeline functions that parse HDL source text through the
//! full pipeline (parse → elaborate → lint) and return structured results
//! for assertion in integration tests.

#![warn(missing_docs)]

use aion_common::Interner;
use aion_config::ProjectConfig;
use aion_diagnostics::{Diagnostic, DiagnosticSink, Severity};
use aion_elaborate::ParsedDesign;
use aion_ir::Design;
use aion_lint::LintEngine;
use aion_source::SourceDb;

/// Result of running the full parse → elaborate → lint pipeline.
pub struct PipelineResult {
    /// The elaborated IR design.
    pub design: Design,
    /// All diagnostics emitted during the pipeline.
    pub diagnostics: Vec<Diagnostic>,
    /// Whether any errors were emitted.
    pub has_errors: bool,
    /// Number of error-severity diagnostics.
    pub error_count: usize,
    /// Number of warning-severity diagnostics.
    pub warning_count: usize,
}

/// Creates a minimal `ProjectConfig` with the given top module name.
pub fn make_config(top: &str) -> ProjectConfig {
    let toml_str = format!(
        r#"
[project]
name = "conformance_test"
version = "0.1.0"
top = "{top}"
"#
    );
    toml::from_str(&toml_str).unwrap()
}

/// Creates a `ProjectConfig` with lint deny/allow overrides.
pub fn make_config_with_lint(top: &str, deny: &[&str], allow: &[&str]) -> ProjectConfig {
    let deny_list: Vec<String> = deny.iter().map(|s| format!("\"{}\"", s)).collect();
    let allow_list: Vec<String> = allow.iter().map(|s| format!("\"{}\"", s)).collect();
    let toml_str = format!(
        r#"
[project]
name = "conformance_test"
version = "0.1.0"
top = "{top}"

[lint]
deny = [{deny}]
allow = [{allow}]
"#,
        deny = deny_list.join(", "),
        allow = allow_list.join(", "),
    );
    toml::from_str(&toml_str).unwrap()
}

/// Runs the full pipeline on Verilog source text.
///
/// Parses the source as Verilog-2005, elaborates with the given top module,
/// and runs the default lint rules.
pub fn full_pipeline_verilog(source: &str, top: &str) -> PipelineResult {
    let config = make_config(top);
    run_pipeline_verilog(source, &config)
}

/// Runs the full pipeline on Verilog source with custom lint config.
pub fn full_pipeline_verilog_with_lint(
    source: &str,
    top: &str,
    deny: &[&str],
    allow: &[&str],
) -> PipelineResult {
    let config = make_config_with_lint(top, deny, allow);
    run_pipeline_verilog(source, &config)
}

/// Runs the full pipeline on SystemVerilog source text.
///
/// Parses the source as SystemVerilog-2017, elaborates with the given top module,
/// and runs the default lint rules.
pub fn full_pipeline_sv(source: &str, top: &str) -> PipelineResult {
    let config = make_config(top);
    run_pipeline_sv(source, &config)
}

/// Runs the full pipeline on SystemVerilog source with custom lint config.
pub fn full_pipeline_sv_with_lint(
    source: &str,
    top: &str,
    deny: &[&str],
    allow: &[&str],
) -> PipelineResult {
    let config = make_config_with_lint(top, deny, allow);
    run_pipeline_sv(source, &config)
}

/// Runs the full pipeline on VHDL source text.
///
/// Parses the source as VHDL-2008, elaborates with the given top module,
/// and runs the default lint rules.
pub fn full_pipeline_vhdl(source: &str, top: &str) -> PipelineResult {
    let config = make_config(top);
    run_pipeline_vhdl(source, &config)
}

/// Runs the full pipeline on VHDL source with custom lint config.
pub fn full_pipeline_vhdl_with_lint(
    source: &str,
    top: &str,
    deny: &[&str],
    allow: &[&str],
) -> PipelineResult {
    let config = make_config_with_lint(top, deny, allow);
    run_pipeline_vhdl(source, &config)
}

fn run_pipeline_verilog(source: &str, config: &ProjectConfig) -> PipelineResult {
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();

    let file_id = source_db.add_source("test.v", source.to_string());
    let ast = aion_verilog_parser::parse_file(file_id, &source_db, &interner, &sink);

    let parsed = ParsedDesign {
        verilog_files: vec![ast],
        sv_files: vec![],
        vhdl_files: vec![],
    };

    finish_pipeline(parsed, config, &source_db, &interner, &sink)
}

/// Runs the full pipeline on multiple SystemVerilog source files.
///
/// Each entry in `files` is `(filename, source_text)`. All files are parsed
/// as SystemVerilog-2017, then elaborated together with the given top module
/// and run through the default lint rules. This enables testing multi-file
/// designs with module hierarchies split across files.
pub fn full_pipeline_sv_multifile(files: &[(&str, &str)], top: &str) -> PipelineResult {
    let config = make_config(top);
    full_pipeline_sv_multifile_with_config(files, &config)
}

/// Runs the full pipeline on multiple SystemVerilog source files with custom config.
///
/// Like [`full_pipeline_sv_multifile`] but accepts an explicit [`ProjectConfig`]
/// for lint allow/deny overrides.
pub fn full_pipeline_sv_multifile_with_config(
    files: &[(&str, &str)],
    config: &ProjectConfig,
) -> PipelineResult {
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();

    let mut sv_files = Vec::with_capacity(files.len());
    for (name, source) in files {
        let file_id = source_db.add_source(name, source.to_string());
        let ast = aion_sv_parser::parse_file(file_id, &source_db, &interner, &sink);
        sv_files.push(ast);
    }

    let parsed = ParsedDesign {
        verilog_files: vec![],
        sv_files,
        vhdl_files: vec![],
    };

    finish_pipeline(parsed, config, &source_db, &interner, &sink)
}

fn run_pipeline_sv(source: &str, config: &ProjectConfig) -> PipelineResult {
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();

    let file_id = source_db.add_source("test.sv", source.to_string());
    let ast = aion_sv_parser::parse_file(file_id, &source_db, &interner, &sink);

    let parsed = ParsedDesign {
        verilog_files: vec![],
        sv_files: vec![ast],
        vhdl_files: vec![],
    };

    finish_pipeline(parsed, config, &source_db, &interner, &sink)
}

fn run_pipeline_vhdl(source: &str, config: &ProjectConfig) -> PipelineResult {
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();

    let file_id = source_db.add_source("test.vhd", source.to_string());
    let ast = aion_vhdl_parser::parse_file(file_id, &source_db, &interner, &sink);

    let parsed = ParsedDesign {
        verilog_files: vec![],
        sv_files: vec![],
        vhdl_files: vec![ast],
    };

    finish_pipeline(parsed, config, &source_db, &interner, &sink)
}

fn finish_pipeline(
    parsed: ParsedDesign,
    config: &ProjectConfig,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> PipelineResult {
    let design = aion_elaborate::elaborate(&parsed, config, source_db, interner, sink)
        .expect("elaboration should not return internal error");

    let engine = LintEngine::new(&config.lint);
    engine.run(&design, sink);

    let diagnostics = sink.diagnostics();
    let has_errors = sink.has_errors();
    let error_count = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warning_count = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();

    PipelineResult {
        design,
        diagnostics,
        has_errors,
        error_count,
        warning_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_config_creates_valid_config() {
        let config = make_config("top");
        assert_eq!(config.project.top, "top");
        assert_eq!(config.project.name, "conformance_test");
    }

    #[test]
    fn make_config_with_lint_creates_valid_config() {
        let config = make_config_with_lint("top", &["W101"], &["C201"]);
        assert_eq!(config.lint.deny, vec!["W101"]);
        assert_eq!(config.lint.allow, vec!["C201"]);
    }

    #[test]
    fn pipeline_verilog_empty_module() {
        let result = full_pipeline_verilog("module top; endmodule", "top");
        assert!(!result.has_errors);
        assert_eq!(result.design.module_count(), 1);
    }

    #[test]
    fn pipeline_sv_empty_module() {
        let result = full_pipeline_sv("module top; endmodule", "top");
        assert!(!result.has_errors);
        assert_eq!(result.design.module_count(), 1);
    }

    #[test]
    fn pipeline_sv_multifile_two_modules() {
        let files = &[
            ("a.sv", "module a; endmodule"),
            ("b.sv", "module b; a u_a(); endmodule"),
        ];
        let result = full_pipeline_sv_multifile(files, "b");
        assert!(!result.has_errors, "errors: {:?}", result.diagnostics);
        assert_eq!(result.design.module_count(), 2);
    }

    #[test]
    fn pipeline_vhdl_empty_entity() {
        let result = full_pipeline_vhdl(
            "entity top is end entity top; architecture rtl of top is begin end architecture rtl;",
            "top",
        );
        assert!(!result.has_errors);
        assert_eq!(result.design.module_count(), 1);
    }
}
