//! Programmer backend that drives the external `openFPGALoader` utility.
//!
//! [openFPGALoader](https://github.com/trabucayre/openFPGALoader) supports
//! the DE0-Nano's on-board USB-Blaster (and many other cables/boards) and
//! handles the vendor-specific JTAG configuration sequences. This backend
//! shells out to it, keeping Aion free of USB dependencies until the native
//! `rusb` driver lands.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{FlashError, JtagDevice, JtagProgrammer};

/// Default executable name, resolved through `PATH`.
pub const DEFAULT_EXECUTABLE: &str = "openFPGALoader";

/// JTAG programmer backend using the `openFPGALoader` command-line tool.
///
/// Construct with [`OpenFpgaLoader::new`] and refine with the builder
/// methods, e.g. `OpenFpgaLoader::new().with_board("de0nano")`.
pub struct OpenFpgaLoader {
    executable: PathBuf,
    board: Option<String>,
    cable: Option<String>,
}

impl OpenFpgaLoader {
    /// Creates a backend that resolves `openFPGALoader` from `PATH`.
    pub fn new() -> Self {
        Self {
            executable: PathBuf::from(DEFAULT_EXECUTABLE),
            board: None,
            cable: None,
        }
    }

    /// Overrides the path to the `openFPGALoader` executable.
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = path.into();
        self
    }

    /// Selects a board profile (e.g. `de0nano`), passed as `-b <board>`.
    pub fn with_board(mut self, board: impl Into<String>) -> Self {
        self.board = Some(board.into());
        self
    }

    /// Selects a JTAG cable (e.g. `usb-blaster`), passed as `-c <cable>`.
    pub fn with_cable(mut self, cable: impl Into<String>) -> Self {
        self.cable = Some(cable.into());
        self
    }

    /// Returns the executable path this backend will invoke.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Builds the command-line arguments for a `--detect` chain scan.
    pub fn detect_args(&self) -> Vec<String> {
        let mut args = self.common_args();
        args.push("--detect".to_string());
        args
    }

    /// Builds the command-line arguments to program `bitstream`.
    pub fn program_args(&self, bitstream: &Path) -> Vec<String> {
        let mut args = self.common_args();
        args.push(bitstream.display().to_string());
        args
    }

    /// Board/cable selection flags shared by all invocations.
    fn common_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(board) = &self.board {
            args.push("-b".to_string());
            args.push(board.clone());
        }
        if let Some(cable) = &self.cable {
            args.push("-c".to_string());
            args.push(cable.clone());
        }
        args
    }

    /// Runs the executable with `args`, returning combined stdout+stderr on
    /// success. A missing executable maps to [`FlashError::LoaderNotFound`];
    /// a non-zero exit maps to [`FlashError::LoaderFailed`].
    fn run(&self, args: &[String]) -> Result<String, FlashError> {
        let output = Command::new(&self.executable)
            .args(args)
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    FlashError::LoaderNotFound {
                        path: self.executable.display().to_string(),
                    }
                } else {
                    FlashError::Io(e)
                }
            })?;

        if !output.status.success() {
            return Err(FlashError::LoaderFailed {
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        // openFPGALoader splits informational output across both streams.
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

impl Default for OpenFpgaLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl JtagProgrammer for OpenFpgaLoader {
    fn name(&self) -> &str {
        "openFPGALoader"
    }

    fn detect_devices(&mut self) -> Result<Vec<JtagDevice>, FlashError> {
        let output = self.run(&self.detect_args())?;
        Ok(parse_detect_output(&output))
    }

    fn program(&mut self, bitstream: &Path) -> Result<(), FlashError> {
        if !bitstream.is_file() {
            return Err(FlashError::BitstreamNotFound {
                path: bitstream.display().to_string(),
            });
        }
        self.run(&self.program_args(bitstream))?;
        Ok(())
    }
}

/// Parses `openFPGALoader --detect` output into a list of devices.
///
/// The expected shape is one `index N:` header per device followed by
/// indented `key value` lines (`idcode`, `family`, `model`, ...). Unknown
/// lines are ignored and entries without a parseable IDCODE are dropped, so
/// banner text and JTAG frequency messages are tolerated.
pub fn parse_detect_output(output: &str) -> Vec<JtagDevice> {
    let mut devices = Vec::new();
    let mut current: Option<JtagDevice> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("index ") {
            if let Some(dev) = current.take() {
                devices.push(dev);
            }
            let position = rest.trim_end_matches(':').trim().parse().unwrap_or(0);
            current = Some(JtagDevice {
                idcode: 0,
                device_name: String::new(),
                family: String::new(),
                position,
            });
        } else if let Some(dev) = current.as_mut() {
            if let Some(value) = trimmed.strip_prefix("idcode ") {
                dev.idcode = parse_hex_u32(value.trim());
            } else if let Some(value) = trimmed.strip_prefix("family ") {
                dev.family = value.trim().to_string();
            } else if let Some(value) = trimmed.strip_prefix("model ") {
                dev.device_name = value.trim().to_string();
            }
        }
    }
    if let Some(dev) = current.take() {
        devices.push(dev);
    }

    devices.retain(|dev| dev.idcode != 0);
    devices
}

/// Parses a hex literal with optional `0x` prefix; returns 0 on failure.
fn parse_hex_u32(text: &str) -> u32 {
    let stripped = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    u32::from_str_radix(stripped, 16).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DE0_NANO_DETECT: &str = "\
Jtag frequency : requested 6000000Hz -> real 6000000Hz
index 0:
\tidcode 0x20f30dd
\tmanufacturer altera
\tfamily cyclone IV E
\tmodel  EP4CE22
\tirlength 10
";

    #[test]
    fn detect_args_default_has_only_detect_flag() {
        let loader = OpenFpgaLoader::new();
        assert_eq!(loader.detect_args(), vec!["--detect".to_string()]);
    }

    #[test]
    fn detect_args_with_board_and_cable() {
        let loader = OpenFpgaLoader::new()
            .with_board("de0nano")
            .with_cable("usb-blaster");
        assert_eq!(
            loader.detect_args(),
            vec!["-b", "de0nano", "-c", "usb-blaster", "--detect"]
        );
    }

    #[test]
    fn program_args_end_with_bitstream_path() {
        let loader = OpenFpgaLoader::new().with_board("de0nano");
        let args = loader.program_args(Path::new("build/de0_nano/blinky.rbf"));
        assert_eq!(args, vec!["-b", "de0nano", "build/de0_nano/blinky.rbf"]);
    }

    #[test]
    fn executable_default_and_override() {
        assert_eq!(
            OpenFpgaLoader::new().executable(),
            Path::new(DEFAULT_EXECUTABLE)
        );
        let custom = OpenFpgaLoader::new().with_executable("/opt/ofl/openFPGALoader");
        assert_eq!(custom.executable(), Path::new("/opt/ofl/openFPGALoader"));
    }

    #[test]
    fn parse_detect_single_device() {
        let devices = parse_detect_output(DE0_NANO_DETECT);
        assert_eq!(devices.len(), 1);
        let dev = &devices[0];
        assert_eq!(dev.idcode, 0x020F30DD);
        assert_eq!(dev.device_name, "EP4CE22");
        assert_eq!(dev.family, "cyclone IV E");
        assert_eq!(dev.position, 0);
    }

    #[test]
    fn parse_detect_multiple_devices() {
        let output = "index 0:\n\tidcode 0x20f30dd\n\tmodel EP4CE22\nindex 1:\n\tidcode 0x20f10dd\n\tmodel EP4CE6\n";
        let devices = parse_detect_output(output);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].position, 0);
        assert_eq!(devices[1].position, 1);
        assert_eq!(devices[1].idcode, 0x020F10DD);
    }

    #[test]
    fn parse_detect_empty_output() {
        assert!(parse_detect_output("").is_empty());
        assert!(parse_detect_output("no device found\n").is_empty());
    }

    #[test]
    fn parse_detect_drops_entries_without_idcode() {
        let output = "index 0:\n\tmodel mystery\n";
        assert!(parse_detect_output(output).is_empty());
    }

    #[test]
    fn parse_hex_handles_prefix_and_garbage() {
        assert_eq!(parse_hex_u32("0x20f30dd"), 0x020F30DD);
        assert_eq!(parse_hex_u32("20F30DD"), 0x020F30DD);
        assert_eq!(parse_hex_u32("not-hex"), 0);
    }

    #[test]
    fn program_missing_bitstream_is_error() {
        let mut loader = OpenFpgaLoader::new();
        let err = loader
            .program(Path::new("/nonexistent/design.rbf"))
            .unwrap_err();
        assert!(matches!(err, FlashError::BitstreamNotFound { .. }));
    }

    #[test]
    fn detect_with_missing_executable_is_loader_not_found() {
        let mut loader = OpenFpgaLoader::new().with_executable("/nonexistent/openFPGALoader");
        let err = loader.detect_devices().unwrap_err();
        assert!(matches!(err, FlashError::LoaderNotFound { .. }));
    }

    /// Writes a fake `openFPGALoader` shell script for subprocess tests.
    #[cfg(unix)]
    fn fake_loader(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("openFPGALoader");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn detect_devices_via_fake_executable() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake_loader(
            dir.path(),
            "printf 'index 0:\\n\\tidcode 0x20f30dd\\n\\tfamily cyclone IV E\\n\\tmodel  EP4CE22\\n'",
        );
        let mut loader = OpenFpgaLoader::new().with_executable(script);
        let devices = loader.detect_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_name, "EP4CE22");
    }

    #[cfg(unix)]
    #[test]
    fn failing_executable_surfaces_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake_loader(dir.path(), "echo 'JTAG init failed' >&2; exit 1");
        let mut loader = OpenFpgaLoader::new().with_executable(script);
        let err = loader.detect_devices().unwrap_err();
        match err {
            FlashError::LoaderFailed { stderr, .. } => assert!(stderr.contains("JTAG init failed")),
            other => panic!("expected LoaderFailed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn program_via_fake_executable_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake_loader(dir.path(), "exit 0");
        let bitstream = dir.path().join("design.rbf");
        std::fs::write(&bitstream, b"\x00\x01").unwrap();
        let mut loader = OpenFpgaLoader::new().with_executable(script);
        loader.program(&bitstream).unwrap();
    }
}
