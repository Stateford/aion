//! `aion flash` — program a generated bitstream onto a connected FPGA.
//!
//! Resolves the bitstream artifact from the project's build directory (or an
//! explicit `--file`), optionally cross-checks the connected device's JTAG
//! IDCODE against the configured target part, and programs the device via
//! the openFPGALoader backend. `--detect` only scans the chain and lists
//! devices without programming.

use std::path::{Path, PathBuf};

use aion_flash::{idcode_for_part, FlashError, JtagProgrammer, OpenFpgaLoader};

use crate::build::{determine_build_dir, resolve_build_target};
use crate::pipeline::resolve_project_root;
use crate::{FlashArgs, GlobalArgs};

/// Runs the `aion flash` command. Returns exit code 0 on success, 1 on error.
pub fn run(args: &FlashArgs, global: &GlobalArgs) -> Result<i32, Box<dyn std::error::Error>> {
    let mut loader = build_loader(args);

    if args.detect {
        let devices = loader.detect_devices()?;
        if devices.is_empty() {
            eprintln!("no FPGA devices detected on the JTAG chain");
            return Ok(1);
        }
        for dev in &devices {
            println!(
                "chain position {}: {} ({}) idcode 0x{:08X}",
                dev.position, dev.device_name, dev.family, dev.idcode
            );
        }
        return Ok(0);
    }

    let (bitstream, expected_part) = resolve_bitstream(args, global)?;

    if !global.quiet {
        eprintln!("  Flashing {}", bitstream.display());
    }

    if let Some(part) = &expected_part {
        verify_connected_device(&mut loader, part, global)?;
    }

    loader.program(&bitstream)?;

    if !global.quiet {
        eprintln!("   Device programmed successfully.");
    }
    Ok(0)
}

/// Constructs the openFPGALoader backend from CLI flags.
///
/// When neither `--board` nor `--cable` is given, defaults to the
/// `usb-blaster` cable (the DE0-Nano's on-board programmer).
fn build_loader(args: &FlashArgs) -> OpenFpgaLoader {
    let mut loader = OpenFpgaLoader::new();
    if let Some(path) = &args.loader_path {
        loader = loader.with_executable(path);
    }
    if args.board.is_none() && args.cable.is_none() {
        return loader.with_cable("usb-blaster");
    }
    if let Some(board) = &args.board {
        loader = loader.with_board(board);
    }
    if let Some(cable) = &args.cable {
        loader = loader.with_cable(cable);
    }
    loader
}

/// Resolves the bitstream file to program and, when it comes from project
/// configuration, the expected device part number for the IDCODE check.
///
/// An explicit `--file` is used as-is (no device check, since the file may
/// target any board). Otherwise the project's `aion.toml` is loaded and the
/// default artifact `build/<target>/<project>.rbf` is expected to exist.
fn resolve_bitstream(
    args: &FlashArgs,
    global: &GlobalArgs,
) -> Result<(PathBuf, Option<String>), Box<dyn std::error::Error>> {
    if let Some(file) = &args.file {
        let path = PathBuf::from(file);
        if !path.is_file() {
            return Err(FlashError::BitstreamNotFound {
                path: path.display().to_string(),
            }
            .into());
        }
        return Ok((path, None));
    }

    let project_dir = resolve_project_root(global)?;
    let config = aion_config::load_config(&project_dir)?;
    let resolved = resolve_build_target(&config, args.target.as_deref())?;
    let path = default_bitstream_path(&project_dir, &resolved.name, &config.project.name);
    if !path.is_file() {
        return Err(FlashError::BitstreamNotFound {
            path: path.display().to_string(),
        }
        .into());
    }
    Ok((path, Some(resolved.device.clone())))
}

/// Computes the default bitstream artifact path: `build/<target>/<project>.rbf`.
fn default_bitstream_path(project_dir: &Path, target_name: &str, project_name: &str) -> PathBuf {
    determine_build_dir(project_dir, Some(target_name), None).join(format!("{project_name}.rbf"))
}

/// Cross-checks the connected device against the configured target part.
///
/// Skips the check when the part has no known IDCODE, and lets the
/// programming attempt produce the real error when the chain scan comes back
/// empty. A detected-but-different device is a hard error so a bitstream is
/// never sent to the wrong board.
fn verify_connected_device(
    loader: &mut OpenFpgaLoader,
    expected_part: &str,
    global: &GlobalArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(known) = idcode_for_part(expected_part) else {
        if global.verbose {
            eprintln!("note: no known IDCODE for '{expected_part}'; skipping device check");
        }
        return Ok(());
    };

    let devices = loader.detect_devices()?;
    if devices.is_empty() {
        return Ok(());
    }
    if !devices.iter().any(|dev| dev.idcode == known.idcode) {
        let found = devices
            .iter()
            .map(|dev| {
                if dev.device_name.is_empty() {
                    format!("idcode 0x{:08X}", dev.idcode)
                } else {
                    dev.device_name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(FlashError::DeviceMismatch {
            expected: expected_part.to_string(),
            found,
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flash_args() -> FlashArgs {
        FlashArgs {
            target: None,
            file: None,
            board: None,
            cable: None,
            loader_path: None,
            detect: false,
        }
    }

    fn global_args(config: Option<String>) -> GlobalArgs {
        GlobalArgs {
            quiet: true,
            verbose: false,
            color: false,
            config,
        }
    }

    #[test]
    fn build_loader_defaults_to_usb_blaster_cable() {
        let loader = build_loader(&flash_args());
        assert_eq!(loader.detect_args(), vec!["-c", "usb-blaster", "--detect"]);
    }

    #[test]
    fn build_loader_board_suppresses_default_cable() {
        let mut args = flash_args();
        args.board = Some("de0nano".to_string());
        let loader = build_loader(&args);
        assert_eq!(loader.detect_args(), vec!["-b", "de0nano", "--detect"]);
    }

    #[test]
    fn build_loader_explicit_cable_and_executable() {
        let mut args = flash_args();
        args.cable = Some("usb-blaster-ii".to_string());
        args.loader_path = Some("/opt/ofl/openFPGALoader".to_string());
        let loader = build_loader(&args);
        assert_eq!(loader.executable(), Path::new("/opt/ofl/openFPGALoader"));
        assert_eq!(
            loader.detect_args(),
            vec!["-c", "usb-blaster-ii", "--detect"]
        );
    }

    #[test]
    fn default_bitstream_path_layout() {
        let path = default_bitstream_path(Path::new("/proj"), "de0_nano", "blinky");
        assert_eq!(path, Path::new("/proj/build/de0_nano/blinky.rbf"));
    }

    #[test]
    fn resolve_bitstream_explicit_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("design.rbf");
        std::fs::write(&file, b"\x00").unwrap();
        let mut args = flash_args();
        args.file = Some(file.display().to_string());
        let (path, expected) = resolve_bitstream(&args, &global_args(None)).unwrap();
        assert_eq!(path, file);
        assert!(expected.is_none());
    }

    #[test]
    fn resolve_bitstream_explicit_file_missing() {
        let mut args = flash_args();
        args.file = Some("/nonexistent/design.rbf".to_string());
        let err = resolve_bitstream(&args, &global_args(None)).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    /// Creates a temp project with an `aion.toml` targeting the DE0-Nano.
    fn temp_project() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("aion.toml");
        std::fs::write(
            &toml_path,
            r#"
[project]
name = "demo"
version = "0.1.0"
top = "top"

[targets.de0_nano]
device = "EP4CE22F17C6"
family = "cyclone4"
"#,
        )
        .unwrap();
        let config = toml_path.display().to_string();
        (dir, config)
    }

    #[test]
    fn resolve_bitstream_from_config() {
        let (dir, config) = temp_project();
        let build_dir = dir.path().join("build/de0_nano");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::write(build_dir.join("demo.rbf"), b"\x00").unwrap();

        let (path, expected) =
            resolve_bitstream(&flash_args(), &global_args(Some(config))).unwrap();
        assert!(path.ends_with("build/de0_nano/demo.rbf"));
        assert_eq!(expected.as_deref(), Some("EP4CE22F17C6"));
    }

    #[test]
    fn resolve_bitstream_from_config_missing_artifact() {
        let (_dir, config) = temp_project();
        let err = resolve_bitstream(&flash_args(), &global_args(Some(config))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("demo.rbf"));
        assert!(msg.contains("aion build --format rbf"));
    }

    #[test]
    fn verify_skips_unknown_part_without_running_loader() {
        // A nonexistent executable proves detection is never invoked.
        let mut loader = OpenFpgaLoader::new().with_executable("/nonexistent/openFPGALoader");
        verify_connected_device(&mut loader, "XC7A35T-1CSG324C", &global_args(None)).unwrap();
    }

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
    fn verify_accepts_matching_device() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake_loader(
            dir.path(),
            "printf 'index 0:\\n\\tidcode 0x20f30dd\\n\\tmodel  EP4CE22\\n'",
        );
        let mut loader = OpenFpgaLoader::new().with_executable(script);
        verify_connected_device(&mut loader, "EP4CE22F17C6", &global_args(None)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn verify_rejects_mismatched_device() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake_loader(
            dir.path(),
            "printf 'index 0:\\n\\tidcode 0x20f10dd\\n\\tmodel  EP4CE6\\n'",
        );
        let mut loader = OpenFpgaLoader::new().with_executable(script);
        let err =
            verify_connected_device(&mut loader, "EP4CE22F17C6", &global_args(None)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("EP4CE22F17C6"));
        assert!(msg.contains("EP4CE6"));
    }
}
