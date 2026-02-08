//! Extended command parsing for the TUI.
//!
//! Reuses [`aion_sim::interactive::parse_command`] for standard simulation
//! commands and adds TUI-specific commands for zoom, goto, signal management,
//! and display format control.

use aion_sim::interactive::{parse_command as sim_parse_command, parse_sim_duration, SimCommand};

/// A TUI command, either a simulation command or a TUI-specific command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiCommand {
    /// A standard simulation command (run, step, inspect, etc.).
    Sim(SimCommand),
    /// Zoom in the waveform viewport.
    ZoomIn,
    /// Zoom out the waveform viewport.
    ZoomOut,
    /// Fit the viewport to the full simulation time range.
    ZoomFit,
    /// Jump the cursor to a specific time.
    Goto {
        /// Target time in femtoseconds.
        time_fs: u64,
    },
    /// Add a signal to the waveform display by name.
    AddSignal {
        /// Signal name pattern.
        name: String,
    },
    /// Remove a signal from the waveform display by name.
    RemoveSignal {
        /// Signal name pattern.
        name: String,
    },
    /// Cycle the value display format (hex → bin → dec).
    CycleFormat,
    /// Toggle the help popup.
    ToggleHelp,
}

/// Parses a command string into a `TuiCommand`.
///
/// First tries TUI-specific commands, then falls back to the standard
/// simulation command parser.
pub fn parse_tui_command(input: &str) -> Result<TuiCommand, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty command".to_string());
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let cmd = parts[0].to_lowercase();

    match cmd.as_str() {
        "zoomin" | "zi" => Ok(TuiCommand::ZoomIn),
        "zoomout" | "zo" => Ok(TuiCommand::ZoomOut),
        "zoomfit" | "zf" | "fit" => Ok(TuiCommand::ZoomFit),
        "goto" | "g" => {
            if parts.len() < 2 {
                return Err("goto requires a time (e.g., 'goto 100ns')".to_string());
            }
            let time_fs = parse_sim_duration(parts[1]).map_err(|e| format!("invalid time: {e}"))?;
            Ok(TuiCommand::Goto { time_fs })
        }
        "add" => {
            if parts.len() < 2 {
                return Err("add requires a signal name".to_string());
            }
            Ok(TuiCommand::AddSignal {
                name: parts[1].to_string(),
            })
        }
        "remove" | "rm" => {
            if parts.len() < 2 {
                return Err("remove requires a signal name".to_string());
            }
            Ok(TuiCommand::RemoveSignal {
                name: parts[1].to_string(),
            })
        }
        "format" | "fmt" => Ok(TuiCommand::CycleFormat),
        _ => {
            // Try parsing as a simulation command
            let sim_cmd = sim_parse_command(trimmed)?;
            Ok(TuiCommand::Sim(sim_cmd))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_zoom_in() {
        assert_eq!(parse_tui_command("zoomin").unwrap(), TuiCommand::ZoomIn);
        assert_eq!(parse_tui_command("zi").unwrap(), TuiCommand::ZoomIn);
    }

    #[test]
    fn parse_zoom_out() {
        assert_eq!(parse_tui_command("zoomout").unwrap(), TuiCommand::ZoomOut);
        assert_eq!(parse_tui_command("zo").unwrap(), TuiCommand::ZoomOut);
    }

    #[test]
    fn parse_zoom_fit() {
        assert_eq!(parse_tui_command("zoomfit").unwrap(), TuiCommand::ZoomFit);
        assert_eq!(parse_tui_command("fit").unwrap(), TuiCommand::ZoomFit);
    }

    #[test]
    fn parse_goto() {
        let cmd = parse_tui_command("goto 100ns").unwrap();
        assert_eq!(
            cmd,
            TuiCommand::Goto {
                time_fs: 100_000_000
            }
        );
    }

    #[test]
    fn parse_goto_shortcut() {
        let cmd = parse_tui_command("g 50us").unwrap();
        assert_eq!(
            cmd,
            TuiCommand::Goto {
                time_fs: 50_000_000_000
            }
        );
    }

    #[test]
    fn parse_goto_missing_arg() {
        assert!(parse_tui_command("goto").is_err());
    }

    #[test]
    fn parse_add_signal() {
        let cmd = parse_tui_command("add top.clk").unwrap();
        assert_eq!(
            cmd,
            TuiCommand::AddSignal {
                name: "top.clk".into()
            }
        );
    }

    #[test]
    fn parse_remove_signal() {
        let cmd = parse_tui_command("remove top.clk").unwrap();
        assert_eq!(
            cmd,
            TuiCommand::RemoveSignal {
                name: "top.clk".into()
            }
        );
    }

    #[test]
    fn parse_remove_shortcut() {
        let cmd = parse_tui_command("rm top.rst").unwrap();
        assert_eq!(
            cmd,
            TuiCommand::RemoveSignal {
                name: "top.rst".into()
            }
        );
    }

    #[test]
    fn parse_format_cycle() {
        assert_eq!(
            parse_tui_command("format").unwrap(),
            TuiCommand::CycleFormat
        );
        assert_eq!(parse_tui_command("fmt").unwrap(), TuiCommand::CycleFormat);
    }

    #[test]
    fn parse_sim_command_passthrough() {
        let cmd = parse_tui_command("step").unwrap();
        assert_eq!(cmd, TuiCommand::Sim(SimCommand::Step));
    }

    #[test]
    fn parse_sim_run_passthrough() {
        let cmd = parse_tui_command("run 10ns").unwrap();
        match cmd {
            TuiCommand::Sim(SimCommand::Run { duration_fs }) => {
                assert_eq!(duration_fs, 10_000_000);
            }
            _ => panic!("expected Sim(Run)"),
        }
    }

    #[test]
    fn parse_empty_error() {
        assert!(parse_tui_command("").is_err());
    }

    #[test]
    fn parse_unknown_error() {
        assert!(parse_tui_command("foobar").is_err());
    }
}
