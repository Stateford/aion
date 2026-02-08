//! `aion lint` — static analysis pipeline.
//!
//! Discovers source files, parses them, elaborates to IR, runs lint rules,
//! and renders diagnostics to the terminal. The full pipeline:
//!
//! 1. Find project root (walk up looking for `aion.toml`)
//! 2. Load config via `aion_config`
//! 3. Discover `.v`, `.sv`, `.vhd`, `.vhdl` files in `src/`
//! 4. Parse each file with the appropriate parser
//! 5. Elaborate into unified IR
//! 6. Run lint engine
//! 7. Render diagnostics

use aion_config::{LintConfig, ProjectConfig};
use aion_diagnostics::{DiagnosticRenderer, DiagnosticSink, Severity, TerminalRenderer};
use aion_lint::LintEngine;

use crate::pipeline::{discover_source_files, parse_all_files, resolve_project_root};
use crate::{GlobalArgs, LintArgs, ReportFormat};

/// Runs the `aion lint` command.
///
/// Discovers source files, parses, elaborates, lints, and renders diagnostics.
/// Returns exit code 0 if no errors, 1 if there are errors.
pub fn run(args: &LintArgs, global: &GlobalArgs) -> Result<i32, Box<dyn std::error::Error>> {
    // Step 1: Find project root
    let project_dir = resolve_project_root(global)?;

    // Step 2: Load config
    let config = aion_config::load_config(&project_dir)?;

    if !global.quiet {
        eprintln!(
            "   Checking {} v{}",
            config.project.name, config.project.version
        );
    }

    // Step 3: Discover source files
    let src_dir = project_dir.join("src");
    let source_files = if src_dir.is_dir() {
        discover_source_files(&src_dir)?
    } else {
        Vec::new()
    };

    if source_files.is_empty() {
        if !global.quiet {
            eprintln!(
                "warning: no HDL source files found in {}",
                src_dir.display()
            );
        }
        return Ok(0);
    }

    // Step 4: Load and parse source files
    let mut source_db = aion_source::SourceDb::new();
    let interner = aion_common::Interner::new();
    let sink = DiagnosticSink::new();

    let parsed = parse_all_files(&source_files, &mut source_db, &interner, &sink)?;

    // Step 5: Elaborate
    let design = aion_elaborate::elaborate(&parsed, &config, &source_db, &interner, &sink)?;

    // Step 6: Merge CLI args with config lint section and run lint
    let merged_config = merge_lint_config(&config, args);
    let engine = LintEngine::new(&merged_config);
    engine.run(&design, &sink);

    // Step 7: Render diagnostics
    let diagnostics = sink.diagnostics();

    match args.format {
        ReportFormat::Text => {
            let renderer = TerminalRenderer::new(global.color, 80);
            for diag in &diagnostics {
                eprintln!("{}", renderer.render(diag, &source_db));
            }
        }
        ReportFormat::Json => {
            let json =
                serde_json::to_string_pretty(&diagnostics).unwrap_or_else(|_| "[]".to_string());
            println!("{json}");
        }
    }

    // Summary
    let error_count = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warning_count = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();

    if !global.quiet && args.format == ReportFormat::Text {
        eprintln!(
            "   Result: {} error(s), {} warning(s)",
            error_count, warning_count
        );
    }

    if sink.has_errors() {
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Merges CLI `--allow`/`--deny` flags with the config file's lint section.
///
/// CLI flags take precedence: if a rule appears in both CLI `--allow` and
/// config `deny`, the CLI `--allow` wins.
fn merge_lint_config(config: &ProjectConfig, args: &LintArgs) -> LintConfig {
    let mut deny: Vec<String> = config.lint.deny.clone();
    let mut allow: Vec<String> = config.lint.allow.clone();
    let warn: Vec<String> = config.lint.warn.clone();

    // CLI args override config: remove from opposite list
    for rule in &args.deny {
        allow.retain(|r| r != rule);
        if !deny.contains(rule) {
            deny.push(rule.clone());
        }
    }
    for rule in &args.allow {
        deny.retain(|r| r != rule);
        if !allow.contains(rule) {
            allow.push(rule.clone());
        }
    }

    LintConfig {
        deny,
        allow,
        warn,
        naming: config
            .lint
            .naming
            .as_ref()
            .map(|n| aion_config::NamingConfig {
                module: n.module.as_ref().map(clone_naming_convention),
                signal: n.signal.as_ref().map(clone_naming_convention),
                parameter: n.parameter.as_ref().map(clone_naming_convention),
                constant: n.constant.as_ref().map(clone_naming_convention),
            }),
    }
}

/// Clones a `NamingConvention` value (since it doesn't derive Clone).
fn clone_naming_convention(nc: &aion_config::NamingConvention) -> aion_config::NamingConvention {
    match nc {
        aion_config::NamingConvention::SnakeCase => aion_config::NamingConvention::SnakeCase,
        aion_config::NamingConvention::CamelCase => aion_config::NamingConvention::CamelCase,
        aion_config::NamingConvention::UpperSnakeCase => {
            aion_config::NamingConvention::UpperSnakeCase
        }
        aion_config::NamingConvention::PascalCase => aion_config::NamingConvention::PascalCase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{
        detect_language, discover_source_files, find_project_root, SourceLanguage,
    };
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn find_project_root_in_current_dir() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("aion.toml"),
            "[project]\nname=\"t\"\nversion=\"0.1.0\"\ntop=\"top\"",
        )
        .unwrap();
        let root = find_project_root(tmp.path()).unwrap();
        assert_eq!(root, tmp.path());
    }

    #[test]
    fn find_project_root_in_parent() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("aion.toml"),
            "[project]\nname=\"t\"\nversion=\"0.1.0\"\ntop=\"top\"",
        )
        .unwrap();
        let sub = tmp.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        let root = find_project_root(&sub).unwrap();
        assert_eq!(root, tmp.path());
    }

    #[test]
    fn find_project_root_not_found() {
        let tmp = TempDir::new().unwrap();
        let result = find_project_root(tmp.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("could not find aion.toml"));
    }

    #[test]
    fn detect_language_verilog() {
        assert_eq!(
            detect_language(Path::new("foo.v")),
            Some(SourceLanguage::Verilog)
        );
    }

    #[test]
    fn detect_language_systemverilog() {
        assert_eq!(
            detect_language(Path::new("foo.sv")),
            Some(SourceLanguage::SystemVerilog)
        );
    }

    #[test]
    fn detect_language_vhdl() {
        assert_eq!(
            detect_language(Path::new("foo.vhd")),
            Some(SourceLanguage::Vhdl)
        );
        assert_eq!(
            detect_language(Path::new("foo.vhdl")),
            Some(SourceLanguage::Vhdl)
        );
    }

    #[test]
    fn detect_language_unknown() {
        assert_eq!(detect_language(Path::new("foo.rs")), None);
        assert_eq!(detect_language(Path::new("foo.txt")), None);
        assert_eq!(detect_language(Path::new("foo")), None);
    }

    #[test]
    fn discover_files_finds_hdl() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("top.sv"), "module top; endmodule").unwrap();
        fs::write(src.join("sub.v"), "module sub; endmodule").unwrap();
        fs::write(src.join("readme.txt"), "not hdl").unwrap();

        let files = discover_source_files(&src).unwrap();
        assert_eq!(files.len(), 2);
        // Should be sorted by path
        let langs: Vec<_> = files.iter().map(|(_, l)| *l).collect();
        assert!(langs.contains(&SourceLanguage::Verilog));
        assert!(langs.contains(&SourceLanguage::SystemVerilog));
    }

    #[test]
    fn discover_files_recursive() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path();
        let sub = src.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(src.join("top.sv"), "module top; endmodule").unwrap();
        fs::write(sub.join("child.vhd"), "entity child is end;").unwrap();

        let files = discover_source_files(src).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn discover_files_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let files = discover_source_files(tmp.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn merge_config_cli_deny_overrides() {
        let config = make_test_config(&[], &["unused-signal"]);
        let args = LintArgs {
            allow: vec![],
            deny: vec!["unused-signal".to_string()],
            format: ReportFormat::Text,
            target: None,
        };
        let merged = merge_lint_config(&config, &args);
        assert!(merged.deny.contains(&"unused-signal".to_string()));
        assert!(!merged.allow.contains(&"unused-signal".to_string()));
    }

    #[test]
    fn merge_config_cli_allow_overrides() {
        let config = make_test_config(&["magic-number"], &[]);
        let args = LintArgs {
            allow: vec!["magic-number".to_string()],
            deny: vec![],
            format: ReportFormat::Text,
            target: None,
        };
        let merged = merge_lint_config(&config, &args);
        assert!(merged.allow.contains(&"magic-number".to_string()));
        assert!(!merged.deny.contains(&"magic-number".to_string()));
    }

    #[test]
    fn merge_config_combines_rules() {
        let config = make_test_config(&["rule-a"], &["rule-b"]);
        let args = LintArgs {
            allow: vec![],
            deny: vec!["rule-c".to_string()],
            format: ReportFormat::Text,
            target: None,
        };
        let merged = merge_lint_config(&config, &args);
        assert!(merged.deny.contains(&"rule-a".to_string()));
        assert!(merged.deny.contains(&"rule-c".to_string()));
        assert!(merged.allow.contains(&"rule-b".to_string()));
    }

    #[test]
    fn merge_config_empty() {
        let config = make_test_config(&[], &[]);
        let args = LintArgs {
            allow: vec![],
            deny: vec![],
            format: ReportFormat::Text,
            target: None,
        };
        let merged = merge_lint_config(&config, &args);
        assert!(merged.deny.is_empty());
        assert!(merged.allow.is_empty());
    }

    #[test]
    fn lint_end_to_end_on_init_project() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("e2e_proj");

        // Init a project
        crate::init::run(
            Some(project_dir.to_str().unwrap().to_string()),
            crate::HdlLanguage::SystemVerilog,
            None,
        )
        .unwrap();

        // Now run lint on it
        let prev_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&project_dir).unwrap();

        let args = LintArgs {
            allow: vec![],
            deny: vec![],
            format: ReportFormat::Text,
            target: None,
        };
        let global = GlobalArgs {
            quiet: true,
            verbose: false,
            color: false,
            config: None,
        };

        let result = run(&args, &global);
        // Restore directory before asserting
        std::env::set_current_dir(prev_dir).unwrap();

        // Should succeed (the template design should parse and lint)
        assert!(result.is_ok(), "lint failed: {:?}", result.err());
    }

    /// Helper to build a minimal ProjectConfig with given deny/allow lists.
    fn make_test_config(deny: &[&str], allow: &[&str]) -> ProjectConfig {
        let toml_str = format!(
            r#"
[project]
name = "test"
version = "0.1.0"
top = "top"

[lint]
deny = [{}]
allow = [{}]
"#,
            deny.iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(", "),
            allow
                .iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(", "),
        );
        aion_config::load_config_from_str(&toml_str).unwrap()
    }
}
