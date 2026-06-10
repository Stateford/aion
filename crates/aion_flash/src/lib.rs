#![warn(missing_docs)]

//! JTAG programming and device detection for FPGA development boards.
//!
//! This crate provides the [`JtagProgrammer`] trait for programmer backends
//! and the [`OpenFpgaLoader`] backend, which drives the external
//! [openFPGALoader](https://github.com/trabucayre/openFPGALoader) utility as
//! a subprocess. A native `rusb`-based USB-Blaster driver is planned as a
//! future backend behind the same trait (see `docs/de0-nano-plan.md`).
//!
//! Typical flow: detect devices on the JTAG chain, cross-check the connected
//! device's IDCODE against the project's configured part number (see
//! [`idcode_for_part`]), then program the bitstream file.

mod error;
mod idcode;
mod openfpgaloader;

pub use error::FlashError;
pub use idcode::{idcode_for_part, lookup_idcode, KnownDevice, KNOWN_DEVICES};
pub use openfpgaloader::{parse_detect_output, OpenFpgaLoader, DEFAULT_EXECUTABLE};

use std::path::Path;

/// A single FPGA device discovered on a JTAG scan chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JtagDevice {
    /// The 32-bit JTAG IDCODE read from the device.
    pub idcode: u32,
    /// Device model name as reported by the programmer (e.g. `EP4CE22`).
    pub device_name: String,
    /// Device family as reported by the programmer (e.g. `cyclone IV E`).
    pub family: String,
    /// Position of the device in the JTAG chain (0 = closest to TDO).
    pub position: u8,
}

/// Trait for JTAG programmer backends.
///
/// Backends scan the JTAG chain for devices and load bitstream files into a
/// device's configuration SRAM. Implementations report failures through
/// [`FlashError`]; they must not panic on missing hardware or tools.
pub trait JtagProgrammer: Send {
    /// Returns the human-readable backend name for logs and error messages.
    fn name(&self) -> &str;

    /// Scans the JTAG chain and returns all detected devices.
    ///
    /// An empty list means the scan ran but found no devices; a missing or
    /// failing programmer tool is reported as an error instead.
    fn detect_devices(&mut self) -> Result<Vec<JtagDevice>, FlashError>;

    /// Programs the bitstream file at `bitstream` into the connected
    /// device's configuration SRAM (volatile; lost on power cycle).
    ///
    /// Note: this takes a file path rather than raw bytes (deviating from the
    /// technical spec sketch) because the initial backend is subprocess-based
    /// and vendor tools detect the bitstream format from the file extension.
    fn program(&mut self, bitstream: &Path) -> Result<(), FlashError>;

    /// Releases any resources held by the backend. Default: no-op.
    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_device() -> JtagDevice {
        JtagDevice {
            idcode: 0x020F30DD,
            device_name: "EP4CE22".to_string(),
            family: "cyclone IV E".to_string(),
            position: 0,
        }
    }

    #[test]
    fn jtag_device_clone_and_eq() {
        let dev = sample_device();
        let copy = dev.clone();
        assert_eq!(dev, copy);
    }

    #[test]
    fn jtag_device_field_difference_breaks_eq() {
        let dev = sample_device();
        let mut other = dev.clone();
        other.idcode = 0x020F10DD;
        assert_ne!(dev, other);
    }

    /// Mock backend used to verify the trait is object-safe and usable
    /// through `Box<dyn JtagProgrammer>`.
    struct MockProgrammer {
        devices: Vec<JtagDevice>,
        programmed: Vec<PathBuf>,
        closed: bool,
    }

    impl JtagProgrammer for MockProgrammer {
        fn name(&self) -> &str {
            "mock"
        }

        fn detect_devices(&mut self) -> Result<Vec<JtagDevice>, FlashError> {
            Ok(self.devices.clone())
        }

        fn program(&mut self, bitstream: &Path) -> Result<(), FlashError> {
            self.programmed.push(bitstream.to_path_buf());
            Ok(())
        }

        fn close(&mut self) {
            self.closed = true;
        }
    }

    #[test]
    fn trait_is_object_safe() {
        let mut boxed: Box<dyn JtagProgrammer> = Box::new(MockProgrammer {
            devices: vec![sample_device()],
            programmed: Vec::new(),
            closed: false,
        });
        assert_eq!(boxed.name(), "mock");
        let devices = boxed.detect_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].idcode, 0x020F30DD);
    }

    #[test]
    fn trait_program_and_close() {
        let mut mock = MockProgrammer {
            devices: Vec::new(),
            programmed: Vec::new(),
            closed: false,
        };
        mock.program(Path::new("design.rbf")).unwrap();
        mock.close();
        assert_eq!(mock.programmed, vec![PathBuf::from("design.rbf")]);
        assert!(mock.closed);
    }
}
