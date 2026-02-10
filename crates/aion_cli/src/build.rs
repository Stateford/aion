//! `aion build` — full synthesis pipeline from source to bitstream.
//!
//! Orchestrates the complete build pipeline:
//! 1. Discover and parse source files
//! 2. Elaborate to IR
//! 3. Synthesize (behavioral lowering, optimization, technology mapping)
//! 4. Place and route
//! 5. Apply pin assignments
//! 6. Static timing analysis
//! 7. Generate bitstream file(s)

use std::path::{Path, PathBuf};

use aion_bitstream::BitstreamFormat;
use aion_config::{ProjectConfig, ResolvedTarget};
use aion_diagnostics::{DiagnosticRenderer, DiagnosticSink, Severity, TerminalRenderer};

use crate::pipeline::{
    apply_pin_assignments, discover_source_files, parse_all_files, resolve_project_root,
};
use crate::{BuildArgs, CliOptLevel, GlobalArgs, ReportFormat};

/// Runs the `aion build` command.
///
/// Chains the full pipeline: parse → elaborate → synthesize → place & route →
/// timing analysis → bitstream generation. Returns exit code 0 on success, 1 on error.
pub fn run(args: &BuildArgs, global: &GlobalArgs) -> Result<i32, Box<dyn std::error::Error>> {
    // Step 1: Find project root and load config
    let project_dir = resolve_project_root(global)?;
    let config = aion_config::load_config(&project_dir)?;

    if !global.quiet {
        eprintln!(
            "   Building {} v{}",
            config.project.name, config.project.version
        );
    }

    // Step 2: Resolve target
    let resolved = resolve_build_target(&config, args.target.as_deref())?;

    if !global.quiet {
        eprintln!("    Target {} ({})", resolved.name, resolved.device);
    }

    // Step 3: Load architecture
    let arch = aion_arch::load_architecture(&resolved.family, &resolved.device)
        .map_err(|e| format!("failed to load architecture: {}", e.message))?;

    // Step 4: Resolve output formats
    let formats = resolve_output_formats(&args.format, &resolved.output_formats, arch.as_ref())?;

    if !global.quiet {
        let fmt_names: Vec<_> = formats.iter().map(|f| f.to_string()).collect();
        eprintln!("   Formats {}", fmt_names.join(", "));
    }

    // Step 5: Discover and parse source files
    let src_dir = project_dir.join("src");
    let source_files = if src_dir.is_dir() {
        discover_source_files(&src_dir)?
    } else {
        Vec::new()
    };

    if source_files.is_empty() {
        eprintln!("error: no HDL source files found in {}", src_dir.display());
        return Ok(1);
    }

    let mut source_db = aion_source::SourceDb::new();
    let interner = aion_common::Interner::new();
    let sink = DiagnosticSink::new();

    let parsed = parse_all_files(&source_files, &mut source_db, &interner, &sink)?;

    // Check for parse errors
    if sink.has_errors() {
        render_and_report(&sink, &source_db, args, global);
        return Ok(1);
    }

    // Step 6: Elaborate
    let design = aion_elaborate::elaborate(&parsed, &config, &source_db, &interner, &sink)?;

    if sink.has_errors() {
        render_and_report(&sink, &source_db, args, global);
        return Ok(1);
    }

    if !global.quiet {
        eprintln!("   Elaborated successfully");
    }

    // Step 7: Synthesize
    let opt_level = match args.optimization {
        Some(cli_opt) => cli_opt_to_config(cli_opt),
        None => resolved.build.optimization.clone(),
    };

    let mapped = aion_synth::synthesize(&design, &interner, arch.as_ref(), &opt_level, &sink);

    if !global.quiet {
        let usage = &mapped.resource_usage;
        eprintln!(
            "   Synthesized: {} LUTs, {} FFs, {} BRAM, {} DSP, {} IO",
            usage.luts, usage.ffs, usage.bram, usage.dsp, usage.io
        );
    }

    // Step 8: Load timing constraints
    let constraints = load_timing_constraints(&project_dir, &resolved, &interner, &sink);

    // Step 9: Place and route
    let mut netlist =
        aion_pnr::place_and_route(&mapped, arch.as_ref(), &constraints, &interner, &sink)
            .map_err(|e| format!("place and route failed: {}", e.message))?;

    if !global.quiet {
        eprintln!("   Placed and routed");
    }

    // Step 10: Apply pin assignments
    apply_pin_assignments(&mut netlist, &resolved.pins);

    // Step 11: Static timing analysis
    let timing_graph = aion_pnr::build_timing_graph(&netlist, arch.as_ref());
    let timing_report = aion_timing::analyze_timing(&timing_graph, &constraints, &interner, &sink)
        .map_err(|e| format!("timing analysis failed: {}", e.message))?;

    if !global.quiet {
        if timing_report.met {
            eprintln!(
                "   Timing met (worst slack: {:.3} ns)",
                timing_report.worst_slack_ns
            );
        } else {
            eprintln!(
                "   Timing VIOLATED (worst slack: {:.3} ns)",
                timing_report.worst_slack_ns
            );
        }
    }

    // Step 12: Generate bitstream(s)
    let build_dir = determine_build_dir(
        &project_dir,
        Some(&resolved.name),
        args.output_dir.as_deref(),
    );
    std::fs::create_dir_all(&build_dir)?;

    let mut generated_files = Vec::new();

    for format in &formats {
        let bitstream = aion_bitstream::generate_bitstream(&netlist, arch.as_ref(), *format, &sink)
            .map_err(|e| format!("bitstream generation failed: {}", e.message))?;

        let filename = format!("{}.{}", config.project.name, format.extension());
        let output_path = build_dir.join(&filename);
        std::fs::write(&output_path, &bitstream.data)?;
        generated_files.push((output_path, bitstream.data.len()));
    }

    // Step 13: Render diagnostics (warnings from synthesis/PnR)
    render_and_report(&sink, &source_db, args, global);

    // Print summary
    if !global.quiet {
        eprintln!();
        for (path, size) in &generated_files {
            eprintln!("   Generated {} ({} bytes)", path.display(), size);
        }
        eprintln!("   Build complete.");
    }

    if sink.has_errors() {
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Resolves which target to build from config and CLI args.
///
/// If `--target` is specified, uses that target. If only one target exists in
/// config, auto-selects it. If no targets are defined or multiple targets exist
/// without `--target`, returns an error.
pub fn resolve_build_target(
    config: &ProjectConfig,
    cli_target: Option<&str>,
) -> Result<ResolvedTarget, Box<dyn std::error::Error>> {
    match cli_target {
        Some(name) => Ok(aion_config::resolve_target(config, name)?),
        None => {
            let target_names: Vec<_> = config.targets.keys().collect();
            match target_names.len() {
                0 => Err("no targets defined in aion.toml; add a [targets.<name>] section or use --target".into()),
                1 => Ok(aion_config::resolve_target(config, target_names[0])?),
                _ => Err(format!(
                    "multiple targets defined ({}); use --target to select one",
                    target_names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ).into()),
            }
        }
    }
}

/// Resolves the output bitstream formats from CLI flags, config, and architecture defaults.
///
/// Priority: CLI `--format` > config `output_formats` > vendor primary format.
/// The special value `"all"` expands to all formats supported by the vendor.
pub fn resolve_output_formats(
    cli_formats: &[String],
    config_formats: &[String],
    arch: &dyn aion_arch::Architecture,
) -> Result<Vec<BitstreamFormat>, Box<dyn std::error::Error>> {
    let generator = aion_bitstream::create_generator(arch)
        .map_err(|e| format!("unsupported architecture: {}", e.message))?;
    let supported = generator.supported_formats();

    let raw_formats = if !cli_formats.is_empty() {
        cli_formats
    } else if !config_formats.is_empty() {
        config_formats
    } else {
        // Default to the vendor's primary format
        return Ok(vec![supported[0]]);
    };

    // Handle "all" expansion
    if raw_formats.len() == 1 && raw_formats[0].to_lowercase() == "all" {
        return Ok(supported.to_vec());
    }

    let mut formats = Vec::new();
    for name in raw_formats {
        match BitstreamFormat::parse(name) {
            Some(fmt) => {
                if !supported.contains(&fmt) {
                    return Err(format!(
                        "format '{}' is not supported for family '{}' (supported: {})",
                        name,
                        arch.family_name(),
                        supported
                            .iter()
                            .map(|f| f.extension())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                    .into());
                }
                formats.push(fmt);
            }
            None => {
                return Err(format!(
                    "unknown bitstream format '{}' (valid: sof, pof, rbf, bit)",
                    name
                )
                .into());
            }
        }
    }

    Ok(formats)
}

/// Determines the output directory for build artifacts.
///
/// CLI `--output-dir` overrides everything. Otherwise uses `build/<target_name>/`
/// relative to the project root.
pub fn determine_build_dir(
    project_dir: &Path,
    target_name: Option<&str>,
    cli_output_dir: Option<&str>,
) -> PathBuf {
    if let Some(dir) = cli_output_dir {
        return PathBuf::from(dir);
    }
    match target_name {
        Some(name) => project_dir.join("build").join(name),
        None => project_dir.join("build"),
    }
}

/// Loads SDC timing constraint files referenced by the resolved target config.
fn load_timing_constraints(
    project_dir: &Path,
    resolved: &ResolvedTarget,
    interner: &aion_common::Interner,
    sink: &DiagnosticSink,
) -> aion_timing::TimingConstraints {
    let mut constraints = aion_timing::TimingConstraints::new();

    for sdc_path in &resolved.constraints.timing {
        let full_path = project_dir.join(sdc_path);
        match std::fs::read_to_string(&full_path) {
            Ok(content) => {
                let parsed = aion_timing::parse_sdc(&content, interner, sink);
                // Merge parsed constraints
                constraints.clocks.extend(parsed.clocks);
                constraints.input_delays.extend(parsed.input_delays);
                constraints.output_delays.extend(parsed.output_delays);
                constraints.false_paths.extend(parsed.false_paths);
                constraints.max_delay_paths.extend(parsed.max_delay_paths);
                constraints.multicycle_paths.extend(parsed.multicycle_paths);
            }
            Err(e) => {
                if !sink.has_errors() {
                    eprintln!(
                        "warning: could not read constraint file {}: {}",
                        full_path.display(),
                        e
                    );
                }
            }
        }
    }

    constraints
}

/// Converts a CLI optimization level to the config `OptLevel`.
fn cli_opt_to_config(cli: CliOptLevel) -> aion_config::OptLevel {
    match cli {
        CliOptLevel::Area => aion_config::OptLevel::Area,
        CliOptLevel::Speed => aion_config::OptLevel::Speed,
        CliOptLevel::Balanced => aion_config::OptLevel::Balanced,
    }
}

/// Renders diagnostics based on the report format setting.
fn render_and_report(
    sink: &DiagnosticSink,
    source_db: &aion_source::SourceDb,
    args: &BuildArgs,
    global: &GlobalArgs,
) {
    let diagnostics = sink.diagnostics();
    if diagnostics.is_empty() {
        return;
    }

    match args.report_format {
        ReportFormat::Text => {
            let renderer = TerminalRenderer::new(global.color, 80);
            for diag in &diagnostics {
                eprintln!("{}", renderer.render(diag, source_db));
            }
        }
        ReportFormat::Json => {
            let json =
                serde_json::to_string_pretty(&diagnostics).unwrap_or_else(|_| "[]".to_string());
            println!("{json}");
        }
    }

    if !global.quiet && args.report_format == ReportFormat::Text {
        let error_count = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warning_count = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count();
        eprintln!(
            "   Result: {} error(s), {} warning(s)",
            error_count, warning_count
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_config::{load_config_from_str, PinAssignment};
    use aion_ir::PortDirection;
    use aion_pnr::{PnrCell, PnrCellId, PnrCellType, PnrNetlist};
    use std::collections::BTreeMap;

    // -- resolve_build_target tests --

    fn make_config_with_targets(targets: &[&str]) -> ProjectConfig {
        let mut toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "top"
"#
        .to_string();
        for target in targets {
            toml.push_str(&format!(
                r#"
[targets.{target}]
device = "EP4CE6E22C8"
family = "cyclone_iv"
"#
            ));
        }
        load_config_from_str(&toml).unwrap()
    }

    #[test]
    fn resolve_target_single_auto() {
        let config = make_config_with_targets(&["board_a"]);
        let resolved = resolve_build_target(&config, None).unwrap();
        assert_eq!(resolved.name, "board_a");
    }

    #[test]
    fn resolve_target_explicit() {
        let config = make_config_with_targets(&["board_a", "board_b"]);
        let resolved = resolve_build_target(&config, Some("board_b")).unwrap();
        assert_eq!(resolved.name, "board_b");
    }

    #[test]
    fn resolve_target_none_defined() {
        let config = make_config_with_targets(&[]);
        let err = resolve_build_target(&config, None).unwrap_err();
        assert!(err.to_string().contains("no targets defined"));
    }

    #[test]
    fn resolve_target_ambiguous() {
        let config = make_config_with_targets(&["board_a", "board_b"]);
        let err = resolve_build_target(&config, None).unwrap_err();
        assert!(err.to_string().contains("multiple targets"));
    }

    // -- resolve_output_formats tests --

    #[test]
    fn resolve_formats_default() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let formats = resolve_output_formats(&[], &[], arch.as_ref()).unwrap();
        assert_eq!(formats.len(), 1);
        // Should be the vendor's primary format (SOF for Intel)
        assert_eq!(formats[0], BitstreamFormat::Sof);
    }

    #[test]
    fn resolve_formats_explicit_cli() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let cli = vec!["pof".to_string()];
        let formats = resolve_output_formats(&cli, &[], arch.as_ref()).unwrap();
        assert_eq!(formats, vec![BitstreamFormat::Pof]);
    }

    #[test]
    fn resolve_formats_all() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let cli = vec!["all".to_string()];
        let formats = resolve_output_formats(&cli, &[], arch.as_ref()).unwrap();
        assert!(formats.len() >= 2);
        assert!(formats.contains(&BitstreamFormat::Sof));
    }

    #[test]
    fn resolve_formats_invalid() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let cli = vec!["xyz".to_string()];
        let err = resolve_output_formats(&cli, &[], arch.as_ref()).unwrap_err();
        assert!(err.to_string().contains("unknown bitstream format"));
    }

    #[test]
    fn resolve_formats_config_fallback() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let config_fmts = vec!["rbf".to_string()];
        let formats = resolve_output_formats(&[], &config_fmts, arch.as_ref()).unwrap();
        assert_eq!(formats, vec![BitstreamFormat::Rbf]);
    }

    #[test]
    fn resolve_formats_cli_overrides_config() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let cli = vec!["pof".to_string()];
        let config_fmts = vec!["sof".to_string()];
        let formats = resolve_output_formats(&cli, &config_fmts, arch.as_ref()).unwrap();
        assert_eq!(formats, vec![BitstreamFormat::Pof]);
    }

    #[test]
    fn resolve_formats_unsupported_for_vendor() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let cli = vec!["bit".to_string()];
        let err = resolve_output_formats(&cli, &[], arch.as_ref()).unwrap_err();
        assert!(err.to_string().contains("not supported"));
    }

    // -- determine_build_dir tests --

    #[test]
    fn build_dir_with_target() {
        let dir = determine_build_dir(Path::new("/proj"), Some("board_a"), None);
        assert_eq!(dir, PathBuf::from("/proj/build/board_a"));
    }

    #[test]
    fn build_dir_without_target() {
        let dir = determine_build_dir(Path::new("/proj"), None, None);
        assert_eq!(dir, PathBuf::from("/proj/build"));
    }

    #[test]
    fn build_dir_cli_override() {
        let dir = determine_build_dir(Path::new("/proj"), Some("board_a"), Some("/custom/out"));
        assert_eq!(dir, PathBuf::from("/custom/out"));
    }

    // -- apply_pin_assignments tests (via pipeline helper) --

    #[test]
    fn apply_pins_matching() {
        let mut netlist = PnrNetlist::new();
        netlist.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "io_clk".to_string(),
            cell_type: PnrCellType::Iobuf {
                direction: PortDirection::Input,
                standard: String::new(),
            },
            placement: None,
            is_fixed: false,
        });

        let mut pins = BTreeMap::new();
        pins.insert(
            "clk".to_string(),
            PinAssignment {
                pin: "PIN_AF14".to_string(),
                io_standard: "3.3-V LVTTL".to_string(),
            },
        );

        apply_pin_assignments(&mut netlist, &pins);

        if let PnrCellType::Iobuf { ref standard, .. } = netlist.cells[0].cell_type {
            assert_eq!(standard, "3.3-V LVTTL");
        } else {
            panic!("expected Iobuf cell type");
        }
    }

    #[test]
    fn apply_pins_no_match() {
        let mut netlist = PnrNetlist::new();
        netlist.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "io_led0".to_string(),
            cell_type: PnrCellType::Iobuf {
                direction: PortDirection::Output,
                standard: "LVCMOS33".to_string(),
            },
            placement: None,
            is_fixed: false,
        });

        let pins = BTreeMap::new(); // empty

        apply_pin_assignments(&mut netlist, &pins);

        if let PnrCellType::Iobuf { ref standard, .. } = netlist.cells[0].cell_type {
            assert_eq!(standard, "LVCMOS33"); // unchanged
        } else {
            panic!("expected Iobuf cell type");
        }
    }
}
