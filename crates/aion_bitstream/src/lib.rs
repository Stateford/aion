//! Bitstream generation for Intel and Xilinx FPGA devices.
//!
//! This crate converts a placed-and-routed `PnrNetlist` into vendor-specific
//! binary files (Intel SOF/POF/RBF, Xilinx BIT). It provides the
//! `BitstreamGenerator` trait with implementations for each vendor family,
//! a `ConfigBitDatabase` abstraction for mapping logical cells to physical
//! configuration bits, and format writers for each output format.
//!
//! The main entry point is `generate_bitstream()`, which dispatches to the
//! appropriate vendor-specific generator based on the target architecture.

#![warn(missing_docs)]

pub mod config_bits;
pub mod crc;
pub mod intel;
pub mod xilinx;

use aion_arch::Architecture;
use aion_common::{AionResult, InternalError};
use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink};
use aion_pnr::PnrNetlist;
use aion_source::Span;
use serde::{Deserialize, Serialize};

/// The output format for a generated bitstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BitstreamFormat {
    /// Intel/Altera SRAM Object File (.sof).
    Sof,
    /// Intel/Altera Programmer Object File (.pof) for flash programming.
    Pof,
    /// Intel/Altera Raw Binary File (.rbf) — headerless raw configuration data.
    Rbf,
    /// Xilinx bitstream file (.bit) with TLV header and command sequence.
    Bit,
}

impl BitstreamFormat {
    /// Returns the conventional file extension for this format.
    pub fn extension(&self) -> &'static str {
        match self {
            BitstreamFormat::Sof => "sof",
            BitstreamFormat::Pof => "pof",
            BitstreamFormat::Rbf => "rbf",
            BitstreamFormat::Bit => "bit",
        }
    }
}

impl std::fmt::Display for BitstreamFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BitstreamFormat::Sof => write!(f, "SOF"),
            BitstreamFormat::Pof => write!(f, "POF"),
            BitstreamFormat::Rbf => write!(f, "RBF"),
            BitstreamFormat::Bit => write!(f, "BIT"),
        }
    }
}

/// A generated bitstream ready for programming or file output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bitstream {
    /// The raw binary data of the bitstream.
    pub data: Vec<u8>,
    /// The format of this bitstream.
    pub format: BitstreamFormat,
    /// The target device name (e.g., "EP4CE6E22C8").
    pub device: String,
    /// A checksum over the bitstream data for integrity verification.
    pub checksum: u32,
}

/// Trait for vendor-specific bitstream generators.
///
/// Implementations convert a placed-and-routed netlist into a binary bitstream
/// for a specific FPGA device family.
pub trait BitstreamGenerator {
    /// Generates a bitstream from the given netlist and architecture.
    ///
    /// Returns the bitstream data in the requested format, or an error if
    /// the format is unsupported or generation fails.
    fn generate(
        &self,
        netlist: &PnrNetlist,
        arch: &dyn Architecture,
        format: BitstreamFormat,
        sink: &DiagnosticSink,
    ) -> AionResult<Bitstream>;

    /// Returns the list of formats supported by this generator.
    fn supported_formats(&self) -> &[BitstreamFormat];
}

/// Creates a bitstream generator for the given architecture.
///
/// Dispatches on the architecture's `family_name()` to select the appropriate
/// vendor-specific generator (Intel or Xilinx).
pub fn create_generator(arch: &dyn Architecture) -> AionResult<Box<dyn BitstreamGenerator>> {
    let family = arch.family_name().to_lowercase();
    match family.as_str() {
        "cyclone_iv" | "cyclone_v" => Ok(Box::new(intel::IntelBitstreamGenerator::new())),
        "artix7" => Ok(Box::new(xilinx::XilinxBitstreamGenerator::new())),
        _ => Err(InternalError::new(format!(
            "unsupported architecture family for bitstream generation: {}",
            arch.family_name()
        ))),
    }
}

/// Convenience function to generate a bitstream in one call.
///
/// Creates the appropriate generator for the architecture, then generates
/// the bitstream in the requested format.
pub fn generate_bitstream(
    netlist: &PnrNetlist,
    arch: &dyn Architecture,
    format: BitstreamFormat,
    sink: &DiagnosticSink,
) -> AionResult<Bitstream> {
    let generator = create_generator(arch)?;

    if !generator.supported_formats().contains(&format) {
        sink.emit(Diagnostic::error(
            DiagnosticCode::new(Category::Vendor, 503),
            format!(
                "format {} is not supported for device family {}",
                format,
                arch.family_name()
            ),
            Span::DUMMY,
        ));
        return Err(InternalError::new(format!(
            "unsupported format {} for family {}",
            format,
            arch.family_name()
        )));
    }

    generator.generate(netlist, arch, format, sink)
}

/// Computes a simple checksum over bitstream data using XOR folding.
///
/// Processes the data in 4-byte big-endian words, XORing them together.
pub fn compute_checksum(data: &[u8]) -> u32 {
    let mut checksum: u32 = 0;
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        checksum ^= u32::from_be_bytes(word);
    }
    checksum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_extension() {
        assert_eq!(BitstreamFormat::Sof.extension(), "sof");
        assert_eq!(BitstreamFormat::Pof.extension(), "pof");
        assert_eq!(BitstreamFormat::Rbf.extension(), "rbf");
        assert_eq!(BitstreamFormat::Bit.extension(), "bit");
    }

    #[test]
    fn format_display() {
        assert_eq!(format!("{}", BitstreamFormat::Sof), "SOF");
        assert_eq!(format!("{}", BitstreamFormat::Pof), "POF");
        assert_eq!(format!("{}", BitstreamFormat::Rbf), "RBF");
        assert_eq!(format!("{}", BitstreamFormat::Bit), "BIT");
    }

    #[test]
    fn format_equality() {
        assert_eq!(BitstreamFormat::Sof, BitstreamFormat::Sof);
        assert_ne!(BitstreamFormat::Sof, BitstreamFormat::Bit);
    }

    #[test]
    fn format_serde_roundtrip() {
        let fmt = BitstreamFormat::Sof;
        let json = serde_json::to_string(&fmt).unwrap();
        let back: BitstreamFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(fmt, back);
    }

    #[test]
    fn bitstream_serde_roundtrip() {
        let bs = Bitstream {
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
            format: BitstreamFormat::Sof,
            device: "EP4CE6E22C8".into(),
            checksum: 0xDEADBEEF,
        };
        let json = serde_json::to_string(&bs).unwrap();
        let back: Bitstream = serde_json::from_str(&json).unwrap();
        assert_eq!(back.format, BitstreamFormat::Sof);
        assert_eq!(back.device, "EP4CE6E22C8");
        assert_eq!(back.checksum, 0xDEADBEEF);
        assert_eq!(back.data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn create_generator_intel() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let gen = create_generator(arch.as_ref()).unwrap();
        assert!(gen.supported_formats().contains(&BitstreamFormat::Sof));
        assert!(gen.supported_formats().contains(&BitstreamFormat::Rbf));
        assert!(gen.supported_formats().contains(&BitstreamFormat::Pof));
    }

    #[test]
    fn create_generator_xilinx() {
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let gen = create_generator(arch.as_ref()).unwrap();
        assert!(gen.supported_formats().contains(&BitstreamFormat::Bit));
    }

    #[test]
    fn create_generator_unknown_family() {
        // Use a mock — but since we can't easily create one, test the error path
        let result = create_generator(&UnknownArch);
        assert!(result.is_err());
    }

    #[test]
    fn compute_checksum_empty() {
        assert_eq!(compute_checksum(&[]), 0);
    }

    #[test]
    fn compute_checksum_known() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
        let expected = 0xDEADBEEF ^ 0x01020304;
        assert_eq!(compute_checksum(&data), expected);
    }

    #[test]
    fn compute_checksum_partial_word() {
        let data = [0xAA, 0xBB];
        let expected = u32::from_be_bytes([0xAA, 0xBB, 0x00, 0x00]);
        assert_eq!(compute_checksum(&data), expected);
    }

    #[test]
    fn generate_bitstream_unsupported_format() {
        let arch = aion_arch::load_architecture("cyclone_iv", "EP4CE6E22C8").unwrap();
        let netlist = PnrNetlist::new();
        let sink = DiagnosticSink::new();
        let result = generate_bitstream(&netlist, arch.as_ref(), BitstreamFormat::Bit, &sink);
        assert!(result.is_err());
        assert!(sink.has_errors());
    }

    /// Minimal arch impl for testing unknown family error path.
    #[derive(Debug)]
    struct UnknownArch;

    impl Architecture for UnknownArch {
        fn family_name(&self) -> &str {
            "unknown_vendor"
        }
        fn device_name(&self) -> &str {
            "test"
        }
        fn total_luts(&self) -> u32 {
            0
        }
        fn total_ffs(&self) -> u32 {
            0
        }
        fn total_bram(&self) -> u32 {
            0
        }
        fn total_dsp(&self) -> u32 {
            0
        }
        fn total_io(&self) -> u32 {
            0
        }
        fn total_pll(&self) -> u32 {
            0
        }
        fn lut_input_count(&self) -> u32 {
            4
        }
        fn resource_summary(&self) -> aion_arch::ResourceUsage {
            aion_arch::ResourceUsage::default()
        }
        fn tech_mapper(&self) -> Box<dyn aion_arch::TechMapper> {
            unreachable!()
        }
    }
}
