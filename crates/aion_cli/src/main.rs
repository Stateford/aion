//! Aion CLI — the command-line interface for the Aion FPGA toolchain.
//!
//! Provides `aion init` for project scaffolding, `aion lint` for static analysis,
//! `aion sim` for running individual testbench simulations, and `aion test` for
//! discovering and running all testbenches in a project.

#![warn(missing_docs)]

mod init;
mod lint;
mod pipeline;
mod sim;
mod test;

use std::process;

use clap::{Parser, Subcommand, ValueEnum};

/// Aion — a fast, unified FPGA toolchain.
#[derive(Parser, Debug)]
#[command(name = "aion", version, about = "Aion FPGA Toolchain")]
pub struct Cli {
    /// Suppress all output except errors.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Enable verbose (debug-level) output.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Control colored output.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    /// Path to a custom `aion.toml` configuration file.
    #[arg(long, global = true)]
    pub config: Option<String>,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Available CLI commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new Aion project.
    Init {
        /// Project name (creates a subdirectory). If omitted, initializes in
        /// the current directory.
        name: Option<String>,

        /// HDL language for the template files.
        #[arg(short, long, value_enum, default_value_t = HdlLanguage::SystemVerilog)]
        lang: HdlLanguage,

        /// Target device part number.
        #[arg(short, long)]
        target: Option<String>,
    },
    /// Run lint checks on the current project.
    Lint(LintArgs),
    /// Run a single testbench simulation.
    Sim(SimArgs),
    /// Discover and run all testbenches.
    Test(TestArgs),
}

/// Arguments for the `aion lint` subcommand.
#[derive(Parser, Debug)]
pub struct LintArgs {
    /// Rule names to suppress (e.g., `--allow unused-signal`).
    #[arg(long, num_args = 1..)]
    pub allow: Vec<String>,

    /// Rule names to promote to errors (e.g., `--deny naming-violation`).
    #[arg(long, num_args = 1..)]
    pub deny: Vec<String>,

    /// Output format for diagnostics.
    #[arg(short, long, value_enum, default_value_t = ReportFormat::Text)]
    pub format: ReportFormat,

    /// Target name to select from `aion.toml`.
    #[arg(short, long)]
    pub target: Option<String>,
}

/// Arguments for the `aion sim` subcommand.
#[derive(Parser, Debug)]
pub struct SimArgs {
    /// Testbench file path or module name.
    pub testbench: String,

    /// Simulation time limit (e.g., "100ns", "1us", "10ms").
    #[arg(long)]
    pub time: Option<String>,

    /// Waveform output format.
    #[arg(long, value_enum)]
    pub waveform: Option<WaveformFormat>,

    /// Output path for waveform file.
    #[arg(short, long)]
    pub output: Option<String>,

    /// Disable waveform recording.
    #[arg(long)]
    pub no_waveform: bool,

    /// Override top module name (default: inferred from file stem).
    #[arg(long)]
    pub top: Option<String>,

    /// Launch interactive REPL debugger instead of running to completion.
    #[arg(short, long)]
    pub interactive: bool,
}

/// Arguments for the `aion test` subcommand.
#[derive(Parser, Debug)]
pub struct TestArgs {
    /// Specific testbench name to run (optional).
    pub name: Option<String>,

    /// Substring filter for testbench names.
    #[arg(long)]
    pub filter: Option<String>,

    /// Waveform output format for all testbenches.
    #[arg(long, value_enum)]
    pub waveform: Option<WaveformFormat>,

    /// Disable waveform recording for all testbenches.
    #[arg(long)]
    pub no_waveform: bool,
}

/// Waveform output format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum WaveformFormat {
    /// Value Change Dump (IEEE 1364).
    Vcd,
    /// Fast Signal Trace (GTKWave).
    Fst,
    /// GHDL Waveform.
    Ghw,
}

/// Controls whether colored output is produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    /// Detect from terminal capabilities.
    Auto,
    /// Always produce colored output.
    Always,
    /// Never produce colored output.
    Never,
}

/// HDL language selection for project scaffolding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum HdlLanguage {
    /// VHDL-2008.
    Vhdl,
    /// Verilog-2005.
    Verilog,
    /// SystemVerilog-2017.
    #[value(name = "systemverilog")]
    SystemVerilog,
}

/// Diagnostic output format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    /// Human-readable terminal output.
    Text,
    /// Machine-readable JSON output.
    Json,
}

/// Global settings derived from CLI flags.
pub struct GlobalArgs {
    /// Whether to suppress non-error output.
    pub quiet: bool,
    /// Whether to print verbose/debug information.
    pub verbose: bool,
    /// Whether to use colored output.
    pub color: bool,
    /// Optional path to a custom config file.
    pub config: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    let color = match cli.color {
        ColorChoice::Auto => atty_is_terminal(),
        ColorChoice::Always => true,
        ColorChoice::Never => false,
    };

    let global = GlobalArgs {
        quiet: cli.quiet,
        verbose: cli.verbose,
        color,
        config: cli.config,
    };

    let result = match cli.command {
        Command::Init { name, lang, target } => init::run(name, lang, target),
        Command::Lint(ref args) => lint::run(args, &global),
        Command::Sim(ref args) => sim::run(args, &global),
        Command::Test(ref args) => test::run(args, &global),
    };

    match result {
        Ok(code) => process::exit(code),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

/// Rough terminal detection — checks if stdout is a terminal.
fn atty_is_terminal() -> bool {
    // Use a simple heuristic: check the TERM env var.
    // In a real build we'd use the `is-terminal` crate, but this is
    // sufficient for now.
    std::env::var("TERM").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_init_default() {
        let cli = Cli::parse_from(["aion", "init"]);
        match cli.command {
            Command::Init { name, lang, target } => {
                assert!(name.is_none());
                assert_eq!(lang, HdlLanguage::SystemVerilog);
                assert!(target.is_none());
            }
            _ => panic!("expected Init command"),
        }
    }

    #[test]
    fn parse_init_with_args() {
        let cli = Cli::parse_from([
            "aion",
            "init",
            "my_project",
            "--lang",
            "vhdl",
            "--target",
            "xc7a35t",
        ]);
        match cli.command {
            Command::Init { name, lang, target } => {
                assert_eq!(name.as_deref(), Some("my_project"));
                assert_eq!(lang, HdlLanguage::Vhdl);
                assert_eq!(target.as_deref(), Some("xc7a35t"));
            }
            _ => panic!("expected Init command"),
        }
    }

    #[test]
    fn parse_lint_default() {
        let cli = Cli::parse_from(["aion", "lint"]);
        match cli.command {
            Command::Lint(ref args) => {
                assert!(args.allow.is_empty());
                assert!(args.deny.is_empty());
                assert_eq!(args.format, ReportFormat::Text);
                assert!(args.target.is_none());
            }
            _ => panic!("expected Lint command"),
        }
    }

    #[test]
    fn parse_lint_with_args() {
        let cli = Cli::parse_from([
            "aion",
            "lint",
            "--allow",
            "unused-signal",
            "--deny",
            "naming-violation",
            "--format",
            "json",
            "--target",
            "de10_nano",
        ]);
        match cli.command {
            Command::Lint(ref args) => {
                assert_eq!(args.allow, vec!["unused-signal"]);
                assert_eq!(args.deny, vec!["naming-violation"]);
                assert_eq!(args.format, ReportFormat::Json);
                assert_eq!(args.target.as_deref(), Some("de10_nano"));
            }
            _ => panic!("expected Lint command"),
        }
    }

    #[test]
    fn parse_global_flags() {
        let cli = Cli::parse_from(["aion", "--quiet", "--color", "never", "lint"]);
        assert!(cli.quiet);
        assert!(!cli.verbose);
        assert_eq!(cli.color, ColorChoice::Never);
    }

    #[test]
    fn parse_verbose_flag() {
        let cli = Cli::parse_from(["aion", "--verbose", "init"]);
        assert!(cli.verbose);
        assert!(!cli.quiet);
    }

    #[test]
    fn parse_color_always() {
        let cli = Cli::parse_from(["aion", "--color", "always", "lint"]);
        assert_eq!(cli.color, ColorChoice::Always);
    }

    #[test]
    fn parse_color_auto() {
        let cli = Cli::parse_from(["aion", "--color", "auto", "lint"]);
        assert_eq!(cli.color, ColorChoice::Auto);
    }

    #[test]
    fn parse_config_path() {
        let cli = Cli::parse_from(["aion", "--config", "/path/to/aion.toml", "lint"]);
        assert_eq!(cli.config.as_deref(), Some("/path/to/aion.toml"));
    }

    #[test]
    fn parse_init_verilog() {
        let cli = Cli::parse_from(["aion", "init", "--lang", "verilog"]);
        match cli.command {
            Command::Init { lang, .. } => {
                assert_eq!(lang, HdlLanguage::Verilog);
            }
            _ => panic!("expected Init command"),
        }
    }

    #[test]
    fn parse_init_systemverilog() {
        let cli = Cli::parse_from(["aion", "init", "--lang", "systemverilog"]);
        match cli.command {
            Command::Init { lang, .. } => {
                assert_eq!(lang, HdlLanguage::SystemVerilog);
            }
            _ => panic!("expected Init command"),
        }
    }

    #[test]
    fn parse_lint_multiple_allow() {
        let cli = Cli::parse_from(["aion", "lint", "--allow", "unused-signal", "magic-number"]);
        match cli.command {
            Command::Lint(ref args) => {
                assert_eq!(args.allow, vec!["unused-signal", "magic-number"]);
            }
            _ => panic!("expected Lint command"),
        }
    }

    #[test]
    fn hdl_language_debug_format() {
        assert_eq!(format!("{:?}", HdlLanguage::Vhdl), "Vhdl");
        assert_eq!(format!("{:?}", HdlLanguage::Verilog), "Verilog");
        assert_eq!(format!("{:?}", HdlLanguage::SystemVerilog), "SystemVerilog");
    }

    // -- Sim command parsing tests --

    #[test]
    fn parse_sim_basic() {
        let cli = Cli::parse_from(["aion", "sim", "tests/counter_tb.sv"]);
        match cli.command {
            Command::Sim(ref args) => {
                assert_eq!(args.testbench, "tests/counter_tb.sv");
                assert!(args.time.is_none());
                assert!(args.waveform.is_none());
                assert!(args.output.is_none());
                assert!(!args.no_waveform);
                assert!(args.top.is_none());
            }
            _ => panic!("expected Sim command"),
        }
    }

    #[test]
    fn parse_sim_with_time() {
        let cli = Cli::parse_from(["aion", "sim", "tb.sv", "--time", "100ns"]);
        match cli.command {
            Command::Sim(ref args) => {
                assert_eq!(args.testbench, "tb.sv");
                assert_eq!(args.time.as_deref(), Some("100ns"));
            }
            _ => panic!("expected Sim command"),
        }
    }

    #[test]
    fn parse_sim_with_waveform() {
        let cli = Cli::parse_from(["aion", "sim", "tb.sv", "--waveform", "vcd"]);
        match cli.command {
            Command::Sim(ref args) => {
                assert_eq!(args.waveform, Some(WaveformFormat::Vcd));
            }
            _ => panic!("expected Sim command"),
        }
    }

    #[test]
    fn parse_sim_with_output() {
        let cli = Cli::parse_from(["aion", "sim", "tb.sv", "--output", "out/tb.vcd"]);
        match cli.command {
            Command::Sim(ref args) => {
                assert_eq!(args.output.as_deref(), Some("out/tb.vcd"));
            }
            _ => panic!("expected Sim command"),
        }
    }

    #[test]
    fn parse_sim_no_waveform() {
        let cli = Cli::parse_from(["aion", "sim", "tb.sv", "--no-waveform"]);
        match cli.command {
            Command::Sim(ref args) => {
                assert!(args.no_waveform);
            }
            _ => panic!("expected Sim command"),
        }
    }

    #[test]
    fn parse_sim_with_top() {
        let cli = Cli::parse_from(["aion", "sim", "tb.sv", "--top", "my_tb"]);
        match cli.command {
            Command::Sim(ref args) => {
                assert_eq!(args.top.as_deref(), Some("my_tb"));
            }
            _ => panic!("expected Sim command"),
        }
    }

    // -- Test command parsing tests --

    #[test]
    fn parse_test_default() {
        let cli = Cli::parse_from(["aion", "test"]);
        match cli.command {
            Command::Test(ref args) => {
                assert!(args.name.is_none());
                assert!(args.filter.is_none());
                assert!(args.waveform.is_none());
                assert!(!args.no_waveform);
            }
            _ => panic!("expected Test command"),
        }
    }

    #[test]
    fn parse_test_with_name() {
        let cli = Cli::parse_from(["aion", "test", "counter_tb"]);
        match cli.command {
            Command::Test(ref args) => {
                assert_eq!(args.name.as_deref(), Some("counter_tb"));
            }
            _ => panic!("expected Test command"),
        }
    }

    #[test]
    fn parse_test_with_filter() {
        let cli = Cli::parse_from(["aion", "test", "--filter", "counter"]);
        match cli.command {
            Command::Test(ref args) => {
                assert_eq!(args.filter.as_deref(), Some("counter"));
            }
            _ => panic!("expected Test command"),
        }
    }

    #[test]
    fn parse_test_no_waveform() {
        let cli = Cli::parse_from(["aion", "test", "--no-waveform"]);
        match cli.command {
            Command::Test(ref args) => {
                assert!(args.no_waveform);
            }
            _ => panic!("expected Test command"),
        }
    }

    #[test]
    fn parse_test_with_waveform() {
        let cli = Cli::parse_from(["aion", "test", "--waveform", "vcd"]);
        match cli.command {
            Command::Test(ref args) => {
                assert_eq!(args.waveform, Some(WaveformFormat::Vcd));
            }
            _ => panic!("expected Test command"),
        }
    }

    #[test]
    fn parse_sim_interactive() {
        let cli = Cli::parse_from(["aion", "sim", "tb.sv", "--interactive"]);
        match cli.command {
            Command::Sim(ref args) => {
                assert!(args.interactive);
            }
            _ => panic!("expected Sim command"),
        }
    }

    #[test]
    fn parse_sim_interactive_short() {
        let cli = Cli::parse_from(["aion", "sim", "tb.sv", "-i"]);
        match cli.command {
            Command::Sim(ref args) => {
                assert!(args.interactive);
            }
            _ => panic!("expected Sim command"),
        }
    }

    // -- WaveformFormat tests --

    #[test]
    fn waveform_format_debug() {
        assert_eq!(format!("{:?}", WaveformFormat::Vcd), "Vcd");
        assert_eq!(format!("{:?}", WaveformFormat::Fst), "Fst");
        assert_eq!(format!("{:?}", WaveformFormat::Ghw), "Ghw");
    }
}
