//! Implementation of the `aion view` command.
//!
//! Loads a previously saved waveform file (VCD) and opens it in the TUI
//! viewer for interactive inspection without re-running the simulation.

use std::path::Path;

use aion_sim::vcd_loader::load_vcd_file;
use aion_tui::app::SignalInfo;
use aion_tui::waveform_data::WaveformData;

use crate::{GlobalArgs, ViewArgs};

/// Runs the `aion view` command.
///
/// Loads a waveform file and launches the TUI viewer. Currently supports
/// VCD files; other formats return an error.
pub fn run(args: &ViewArgs, _global: &GlobalArgs) -> Result<i32, Box<dyn std::error::Error>> {
    let path = Path::new(&args.file);

    if !path.exists() {
        return Err(format!("file not found: {}", args.file).into());
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "vcd" => view_vcd(path),
        _ => Err(format!("unsupported waveform format: .{ext} (supported: .vcd)").into()),
    }
}

/// Loads a VCD file and opens it in the TUI viewer.
fn view_vcd(path: &Path) -> Result<i32, Box<dyn std::error::Error>> {
    let loaded = load_vcd_file(path)?;
    let waveform = WaveformData::from_loaded(&loaded);

    let signal_info: Vec<SignalInfo> = loaded
        .signals
        .iter()
        .enumerate()
        .map(|(i, sig)| SignalInfo {
            id: aion_sim::SimSignalId::from_raw(i as u32),
            name: sig.name.clone(),
            width: sig.width,
        })
        .collect();

    aion_tui::run_tui_viewer(waveform, signal_info)?;

    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn view_file_not_found() {
        let args = ViewArgs {
            file: "/nonexistent/path/foo.vcd".to_string(),
        };
        let global = GlobalArgs {
            quiet: false,
            verbose: false,
            color: false,
            config: None,
        };
        let result = run(&args, &global);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("file not found"));
    }

    #[test]
    fn view_unsupported_extension() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "data").unwrap();
        // Rename to .xyz extension
        let path = tmp.path().to_str().unwrap().to_string();

        let args = ViewArgs { file: path };
        let global = GlobalArgs {
            quiet: false,
            verbose: false,
            color: false,
            config: None,
        };
        let result = run(&args, &global);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported"));
    }

    #[test]
    fn view_vcd_load_succeeds() {
        let mut tmp = NamedTempFile::with_suffix(".vcd").unwrap();
        write!(
            tmp,
            "$timescale 1ns $end\n$scope module top $end\n$var wire 1 ! clk $end\n$upscope $end\n$enddefinitions $end\n#0\n0!\n"
        )
        .unwrap();
        tmp.flush().unwrap();

        // We can't actually run the TUI in a test (no terminal), but we can
        // test that the VCD loads successfully
        let path = tmp.path();
        let loaded = load_vcd_file(path).unwrap();
        assert_eq!(loaded.signals.len(), 1);
        assert_eq!(loaded.signals[0].name, "top.clk");
    }
}
