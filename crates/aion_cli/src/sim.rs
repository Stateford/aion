//! `aion sim` — run a single testbench simulation.
//!
//! Resolves a testbench file, parses all project sources plus the testbench,
//! elaborates with the testbench as the top module, and runs the simulation
//! engine. Outputs `$display` messages to stdout, assertion failures to stderr,
//! and optionally records VCD waveforms.

use std::path::{Path, PathBuf};

use aion_common::Interner;
use aion_config::ProjectConfig;
use aion_diagnostics::DiagnosticSink;
use aion_sim::{SimConfig, SimTime, WaveformOutputFormat};
use aion_source::SourceDb;

use crate::pipeline::{
    discover_source_files, parse_all_files, parse_duration, render_diagnostics,
    resolve_project_root,
};
use crate::{GlobalArgs, SimArgs, WaveformFormat};

/// Runs the `aion sim` command.
///
/// Resolves the testbench, parses all project sources, elaborates with the
/// testbench as top module, runs the simulation, and prints results.
/// Returns exit code 0 if no assertion failures, 1 otherwise.
pub fn run(args: &SimArgs, global: &GlobalArgs) -> Result<i32, Box<dyn std::error::Error>> {
    // Step 1: Find project root and load config
    let project_dir = resolve_project_root(global)?;
    let config = aion_config::load_config(&project_dir)?;

    // Step 2: Resolve testbench file
    let tb_path = resolve_testbench(&args.testbench, &project_dir)?;

    // Step 3: Infer top module name
    let top_module = infer_top_module(&tb_path, args.top.as_deref());

    if !global.quiet {
        eprintln!("   Simulating {top_module}");
    }

    // Step 4: Discover source files from src/ + testbench
    let mut source_files = Vec::new();
    let src_dir = project_dir.join("src");
    if src_dir.is_dir() {
        source_files.extend(discover_source_files(&src_dir)?);
    }

    // Add the testbench file itself
    if let Some(lang) = crate::pipeline::detect_language(&tb_path) {
        // Avoid adding duplicates (if testbench is inside src/)
        if !source_files.iter().any(|(p, _)| *p == tb_path) {
            source_files.push((tb_path.clone(), lang));
        }
    } else {
        return Err(format!(
            "unrecognized file extension for testbench: {}",
            tb_path.display()
        )
        .into());
    }

    // Step 5: Parse all files
    let mut source_db = SourceDb::new();
    let interner = Interner::new();
    let sink = DiagnosticSink::new();

    let parsed = parse_all_files(&source_files, &mut source_db, &interner, &sink)?;

    // Step 6: Elaborate with testbench as top
    let elab_config = make_config_with_top(&config, &top_module);
    let design = aion_elaborate::elaborate(&parsed, &elab_config, &source_db, &interner, &sink)?;

    if sink.has_errors() {
        render_diagnostics(&sink, &source_db, global.color);
        return Ok(1);
    }

    // Step 7: Interactive mode — launch TUI
    if args.interactive {
        if !global.quiet {
            eprintln!("   Entering interactive TUI...");
        }
        aion_tui::run_tui(&design, &interner)?;
        return Ok(0);
    }

    // Step 8: Build SimConfig
    let time_limit = match &args.time {
        Some(t) => Some(parse_duration(t)?),
        None => None,
    };

    let record_waveform = !args.no_waveform;
    let waveform_format = match args.waveform {
        Some(WaveformFormat::Fst) => Some(WaveformOutputFormat::Fst),
        Some(WaveformFormat::Ghw) => {
            if !global.quiet {
                eprintln!("warning: GHW format is not yet supported, falling back to VCD");
            }
            Some(WaveformOutputFormat::Vcd)
        }
        _ => Some(WaveformOutputFormat::Vcd),
    };
    let waveform_ext = match waveform_format {
        Some(WaveformOutputFormat::Fst) => "fst",
        _ => "vcd",
    };
    let waveform_path = if record_waveform {
        let out_path = match &args.output {
            Some(p) => PathBuf::from(p),
            None => {
                let out_dir = project_dir.join("out");
                std::fs::create_dir_all(&out_dir)?;
                out_dir.join(format!("{top_module}.{waveform_ext}"))
            }
        };
        Some(out_path)
    } else {
        None
    };

    let sim_config = SimConfig {
        time_limit,
        waveform_path: waveform_path.clone(),
        record_waveform,
        waveform_format,
    };

    // Step 8: Run simulation
    let result = aion_sim::simulate(&design, &sim_config, &interner)?;

    // Step 9: Print $display output to stdout
    for line in &result.display_output {
        println!("{line}");
    }

    // Print assertion failures to stderr
    for failure in &result.assertion_failures {
        eprintln!("ASSERTION FAILED: {failure}");
    }

    // Step 10: Print summary
    if !global.quiet {
        let time_str = format_sim_time(&result.final_time);
        eprintln!(
            "   Simulation finished at {time_str} ({} delta cycles)",
            result.total_deltas
        );
        if let Some(ref path) = waveform_path {
            eprintln!("   Waveform: {}", path.display());
        }
    }

    // Step 11: Exit code
    if result.assertion_failures.is_empty() {
        Ok(0)
    } else {
        if !global.quiet {
            eprintln!(
                "   FAILED: {} assertion(s) failed",
                result.assertion_failures.len()
            );
        }
        Ok(1)
    }
}

/// Resolves a testbench argument to a file path.
///
/// Tries: (1) exact file path, (2) relative to project dir,
/// (3) search `tests/` directory by stem name.
fn resolve_testbench(arg: &str, project_dir: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // Try as an absolute/relative file path
    let path = PathBuf::from(arg);
    if path.is_file() {
        return Ok(std::fs::canonicalize(&path)?);
    }

    // Try relative to project dir
    let rel_path = project_dir.join(arg);
    if rel_path.is_file() {
        return Ok(std::fs::canonicalize(&rel_path)?);
    }

    // Search tests/ directory by stem name
    let tests_dir = project_dir.join("tests");
    if tests_dir.is_dir() {
        if let Ok(files) = discover_source_files(&tests_dir) {
            for (file_path, _lang) in &files {
                if let Some(stem) = file_path.file_stem().and_then(|s| s.to_str()) {
                    if stem == arg {
                        return Ok(file_path.clone());
                    }
                }
            }
        }
    }

    Err(format!("testbench not found: '{arg}'").into())
}

/// Infers the top module name from the testbench file path.
///
/// Uses the `--top` override if provided, otherwise uses the file stem
/// (e.g., `counter_tb.sv` → `counter_tb`).
fn infer_top_module(tb_path: &Path, top_override: Option<&str>) -> String {
    if let Some(top) = top_override {
        return top.to_string();
    }
    tb_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("top")
        .to_string()
}

/// Creates a `ProjectConfig` with the given top module name.
///
/// Copies project metadata from the original config but overrides the top module.
pub(crate) fn make_config_with_top(config: &ProjectConfig, top: &str) -> ProjectConfig {
    let toml_str = format!(
        r#"
[project]
name = "{}"
version = "{}"
top = "{}"
"#,
        config.project.name, config.project.version, top,
    );
    // This should always succeed since we're building a minimal valid config
    aion_config::load_config_from_str(&toml_str).unwrap_or_else(|_| {
        // Fallback: parse a truly minimal config
        aion_config::load_config_from_str(&format!(
            "[project]\nname=\"sim\"\nversion=\"0.1.0\"\ntop=\"{top}\""
        ))
        .expect("fallback config must parse")
    })
}

/// Formats a SimTime for human-readable output.
fn format_sim_time(time: &SimTime) -> String {
    format!("{time}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn infer_top_module_from_path() {
        let path = Path::new("/project/tests/counter_tb.sv");
        assert_eq!(infer_top_module(path, None), "counter_tb");
    }

    #[test]
    fn infer_top_module_explicit() {
        let path = Path::new("/project/tests/counter_tb.sv");
        assert_eq!(infer_top_module(path, Some("my_top")), "my_top");
    }

    #[test]
    fn resolve_testbench_file_path() {
        let tmp = TempDir::new().unwrap();
        let tb_file = tmp.path().join("tb.sv");
        fs::write(&tb_file, "module tb; endmodule").unwrap();

        let result = resolve_testbench(tb_file.to_str().unwrap(), tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_testbench_by_name() {
        let tmp = TempDir::new().unwrap();
        let tests_dir = tmp.path().join("tests");
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(
            tests_dir.join("counter_tb.sv"),
            "module counter_tb; endmodule",
        )
        .unwrap();

        let result = resolve_testbench("counter_tb", tmp.path());
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_str().unwrap().contains("counter_tb.sv"));
    }

    #[test]
    fn resolve_testbench_not_found() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_testbench("nonexistent", tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn make_config_with_top_overrides_module() {
        let base = aion_config::load_config_from_str(
            "[project]\nname=\"test\"\nversion=\"1.0.0\"\ntop=\"original\"",
        )
        .unwrap();
        let config = make_config_with_top(&base, "my_tb");
        assert_eq!(config.project.top, "my_tb");
        assert_eq!(config.project.name, "test");
    }

    #[test]
    fn sim_end_to_end_on_init_project() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("sim_proj");

        // Init a project
        crate::init::run(
            Some(project_dir.to_str().unwrap().to_string()),
            crate::HdlLanguage::SystemVerilog,
            None,
        )
        .unwrap();

        // Run sim on the generated testbench
        let args = SimArgs {
            testbench: project_dir
                .join("tests")
                .join("top_tb.sv")
                .to_str()
                .unwrap()
                .to_string(),
            time: Some("100ns".to_string()),
            waveform: None,
            output: None,
            no_waveform: true,
            top: None,
            interactive: false,
        };
        let global = GlobalArgs {
            quiet: true,
            verbose: false,
            color: false,
            config: Some(project_dir.join("aion.toml").to_str().unwrap().to_string()),
        };

        let result = run(&args, &global);
        assert!(result.is_ok(), "sim failed: {:?}", result.err());
    }
}
