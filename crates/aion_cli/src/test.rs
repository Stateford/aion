//! `aion test` — discover and run all testbenches.
//!
//! Discovers testbench files in the project's `tests/` directory, optionally
//! filters by name, then runs each testbench through the full pipeline:
//! parse → elaborate → simulate. Reports per-test pass/fail status and
//! a summary line.

use std::path::{Path, PathBuf};

use aion_common::Interner;
use aion_config::ProjectConfig;
use aion_diagnostics::DiagnosticSink;
use aion_sim::{SimConfig, SimTime, WaveformOutputFormat};
use aion_source::SourceDb;

use crate::pipeline::{
    discover_source_files, parse_all_files, render_diagnostics, resolve_project_root,
    SourceLanguage,
};
use crate::{GlobalArgs, TestArgs, WaveformFormat};

/// Result of running a single testbench.
struct TestResult {
    /// Name of the testbench (file stem).
    name: String,
    /// Whether the testbench passed (no assertion failures).
    passed: bool,
    /// Final simulation time.
    final_time: SimTime,
    /// Number of assertion failures.
    assertion_count: usize,
    /// Error message if the testbench failed to run.
    error: Option<String>,
}

/// Runs the `aion test` command.
///
/// Discovers testbenches in `tests/`, filters by name/filter, runs each,
/// and prints per-test status plus a summary. Returns exit code 0 if all
/// pass, 1 if any fail.
pub fn run(args: &TestArgs, global: &GlobalArgs) -> Result<i32, Box<dyn std::error::Error>> {
    // Step 1: Resolve project root and load config
    let project_dir = resolve_project_root(global)?;
    let config = aion_config::load_config(&project_dir)?;

    if !global.quiet {
        eprintln!(
            "   Testing {} v{}",
            config.project.name, config.project.version
        );
    }

    // Step 2: Discover source files from src/
    let src_dir = project_dir.join("src");
    let src_files = if src_dir.is_dir() {
        discover_source_files(&src_dir)?
    } else {
        Vec::new()
    };

    // Step 3: Discover testbenches from tests/
    let tests_dir = project_dir.join("tests");
    let test_files = if tests_dir.is_dir() {
        discover_source_files(&tests_dir)?
    } else {
        if !global.quiet {
            eprintln!("warning: no tests/ directory found");
        }
        return Ok(0);
    };

    if test_files.is_empty() {
        if !global.quiet {
            eprintln!("warning: no testbench files found in tests/");
        }
        return Ok(0);
    }

    // Step 4: Filter testbenches by name/filter
    let testbenches = filter_testbenches(&test_files, args.name.as_deref(), args.filter.as_deref());

    if testbenches.is_empty() {
        if !global.quiet {
            eprintln!("warning: no testbenches match the given filter");
        }
        return Ok(0);
    }

    if !global.quiet {
        eprintln!("   Found {} testbench(es)", testbenches.len());
    }

    // Step 5: Parse all source files (src + tests) once
    let mut all_files = src_files;
    for (path, lang) in &test_files {
        if !all_files.iter().any(|(p, _)| p == path) {
            all_files.push((path.clone(), *lang));
        }
    }

    let mut source_db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();

    let parsed = parse_all_files(&all_files, &mut source_db, &interner, &sink)?;

    // Check for parse errors
    if sink.has_errors() {
        render_diagnostics(&sink, &source_db, global.color);
        return Ok(1);
    }

    // Step 6: Run each testbench
    let mut results = Vec::new();
    let record_waveform = !args.no_waveform;

    for (tb_path, _lang) in &testbenches {
        let tb_name = tb_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let result = run_single_testbench(
            &tb_name,
            &parsed,
            &config,
            &source_db,
            &interner,
            record_waveform,
            args.waveform,
            &project_dir,
        );

        if !global.quiet {
            print_test_result(&result);
        }

        results.push(result);
    }

    // Step 7: Print summary
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;

    if !global.quiet {
        eprintln!();
        eprintln!(
            "   Result: {passed} passed, {failed} failed out of {} testbench(es)",
            results.len()
        );
    }

    if failed > 0 {
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Filters testbench files by name and/or substring filter.
///
/// If `name` is provided, only the testbench with that exact stem is returned.
/// If `filter` is provided, only testbenches whose stem contains the substring
/// are returned. If both are `None`, all testbenches are returned.
fn filter_testbenches(
    files: &[(PathBuf, SourceLanguage)],
    name: Option<&str>,
    filter: Option<&str>,
) -> Vec<(PathBuf, SourceLanguage)> {
    files
        .iter()
        .filter(|(path, _)| {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Some(n) = name {
                return stem == n;
            }
            if let Some(f) = filter {
                return stem.contains(f);
            }
            true
        })
        .cloned()
        .collect()
}

/// Runs a single testbench and returns the result.
#[allow(clippy::too_many_arguments)]
fn run_single_testbench(
    tb_name: &str,
    parsed: &aion_elaborate::ParsedDesign,
    config: &ProjectConfig,
    source_db: &SourceDb,
    interner: &Interner,
    record_waveform: bool,
    waveform_format: Option<WaveformFormat>,
    project_dir: &Path,
) -> TestResult {
    // Create a fresh sink for this testbench
    let elab_sink = DiagnosticSink::new();

    // Elaborate with this testbench as top
    let elab_config = crate::sim::make_config_with_top(config, tb_name);
    let design =
        match aion_elaborate::elaborate(parsed, &elab_config, source_db, interner, &elab_sink) {
            Ok(d) => d,
            Err(e) => {
                return TestResult {
                    name: tb_name.to_string(),
                    passed: false,
                    final_time: SimTime::default(),
                    assertion_count: 0,
                    error: Some(format!("elaboration error: {e}")),
                };
            }
        };

    if elab_sink.has_errors() {
        return TestResult {
            name: tb_name.to_string(),
            passed: false,
            final_time: SimTime::default(),
            assertion_count: 0,
            error: Some("elaboration produced errors".to_string()),
        };
    }

    // Build SimConfig
    let resolved_format = match waveform_format {
        Some(WaveformFormat::Fst) => Some(WaveformOutputFormat::Fst),
        Some(WaveformFormat::Ghw) => Some(WaveformOutputFormat::Vcd), // Silently fall back
        _ => Some(WaveformOutputFormat::Vcd),
    };
    let waveform_ext = match resolved_format {
        Some(WaveformOutputFormat::Fst) => "fst",
        _ => "vcd",
    };
    let waveform_path = if record_waveform {
        let out_dir = project_dir.join("out");
        let _ = std::fs::create_dir_all(&out_dir);
        Some(out_dir.join(format!("{tb_name}.{waveform_ext}")))
    } else {
        None
    };

    let sim_config = SimConfig {
        time_limit: None,
        waveform_path,
        record_waveform,
        waveform_format: resolved_format,
    };

    // Run simulation
    match aion_sim::simulate(&design, &sim_config) {
        Ok(result) => TestResult {
            name: tb_name.to_string(),
            passed: result.assertion_failures.is_empty(),
            final_time: result.final_time,
            assertion_count: result.assertion_failures.len(),
            error: None,
        },
        Err(e) => TestResult {
            name: tb_name.to_string(),
            passed: false,
            final_time: SimTime::default(),
            assertion_count: 0,
            error: Some(format!("simulation error: {e}")),
        },
    }
}

/// Prints the result of a single testbench run.
fn print_test_result(result: &TestResult) {
    if result.passed {
        eprintln!(
            "   PASS  {name} ({time})",
            name = result.name,
            time = result.final_time,
        );
    } else if let Some(ref err) = result.error {
        eprintln!("   FAIL  {name}: {err}", name = result.name);
    } else {
        eprintln!(
            "   FAIL  {name}: {count} assertion(s) failed",
            name = result.name,
            count = result.assertion_count,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn filter_testbenches_by_name() {
        let files = vec![
            (
                PathBuf::from("tests/a_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
            (
                PathBuf::from("tests/b_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
            (
                PathBuf::from("tests/c_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
        ];
        let result = filter_testbenches(&files, Some("b_tb"), None);
        assert_eq!(result.len(), 1);
        assert!(result[0].0.to_str().unwrap().contains("b_tb"));
    }

    #[test]
    fn filter_testbenches_by_substring() {
        let files = vec![
            (
                PathBuf::from("tests/counter_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
            (
                PathBuf::from("tests/fsm_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
            (
                PathBuf::from("tests/counter_fast_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
        ];
        let result = filter_testbenches(&files, None, Some("counter"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_testbenches_no_match() {
        let files = vec![
            (
                PathBuf::from("tests/a_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
            (
                PathBuf::from("tests/b_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
        ];
        let result = filter_testbenches(&files, Some("nonexistent"), None);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_testbenches_all() {
        let files = vec![
            (
                PathBuf::from("tests/a_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
            (
                PathBuf::from("tests/b_tb.sv"),
                SourceLanguage::SystemVerilog,
            ),
        ];
        let result = filter_testbenches(&files, None, None);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_end_to_end_on_init_project() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("test_proj");

        // Init a project
        crate::init::run(
            Some(project_dir.to_str().unwrap().to_string()),
            crate::HdlLanguage::SystemVerilog,
            None,
        )
        .unwrap();

        // Run test on it
        let args = TestArgs {
            name: None,
            filter: None,
            waveform: None,
            no_waveform: true,
        };
        let global = GlobalArgs {
            quiet: true,
            verbose: false,
            color: false,
            config: Some(project_dir.join("aion.toml").to_str().unwrap().to_string()),
        };

        let result = run(&args, &global);
        assert!(result.is_ok(), "test failed: {:?}", result.err());
    }
}
