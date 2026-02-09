//! Intel/Altera SOF (SRAM Object File) format writer.
//!
//! SOF files contain a header with device and design metadata, followed by
//! configuration frame data and a CRC-16 footer. They are the primary
//! programming format for Intel/Altera FPGA SRAM configuration.

use crate::config_bits::ConfigImage;
use crate::crc::crc16;

/// Magic bytes identifying a SOF file.
const SOF_MAGIC: &[u8] = b"AION_SOF";

/// SOF format version.
const SOF_VERSION: u8 = 1;

/// Writes a SOF file from the given configuration image.
///
/// The SOF format contains:
/// 1. Magic identifier (`AION_SOF`)
/// 2. Version byte
/// 3. Device name (length-prefixed string)
/// 4. Design name (length-prefixed string)
/// 5. Frame count (u32 BE)
/// 6. Frame word count (u32 BE)
/// 7. Frame data (each frame: address u32 BE + word data u32 BE)
/// 8. CRC-16 over all preceding data
pub fn write_sof(config: &ConfigImage, device_name: &str, design_name: &str) -> Vec<u8> {
    let mut data = Vec::new();

    // Header
    data.extend_from_slice(SOF_MAGIC);
    data.push(SOF_VERSION);

    // Device name (length-prefixed)
    write_length_prefixed_string(&mut data, device_name);

    // Design name (length-prefixed)
    write_length_prefixed_string(&mut data, design_name);

    // Frame metadata
    let frames = config.finalize();
    data.extend_from_slice(&(frames.len() as u32).to_be_bytes());
    data.extend_from_slice(&config.frame_word_count.to_be_bytes());

    // Frame data
    for frame in &frames {
        data.extend_from_slice(&frame.address.as_raw().to_be_bytes());
        for &word in &frame.data {
            data.extend_from_slice(&word.to_be_bytes());
        }
    }

    // CRC-16 footer
    let checksum = crc16(&data);
    data.extend_from_slice(&checksum.to_be_bytes());

    data
}

/// Writes a length-prefixed string (u16 BE length + UTF-8 bytes).
fn write_length_prefixed_string(data: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(u16::MAX as usize) as u16;
    data.extend_from_slice(&len.to_be_bytes());
    data.extend_from_slice(&bytes[..len as usize]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_bits::{ConfigBit, ConfigImage, FrameAddress};

    #[test]
    fn sof_magic_header() {
        let config = ConfigImage::new(4, 10);
        let data = write_sof(&config, "EP4CE6", "test");
        assert_eq!(&data[..8], SOF_MAGIC);
        assert_eq!(data[8], SOF_VERSION);
    }

    #[test]
    fn sof_device_name() {
        let config = ConfigImage::new(4, 10);
        let data = write_sof(&config, "EP4CE6E22C8", "blinky");
        // After magic(8) + version(1), device name length at offset 9
        let dev_len = u16::from_be_bytes([data[9], data[10]]) as usize;
        let dev_name = std::str::from_utf8(&data[11..11 + dev_len]).unwrap();
        assert_eq!(dev_name, "EP4CE6E22C8");
    }

    #[test]
    fn sof_empty_config() {
        let config = ConfigImage::new(4, 10);
        let data = write_sof(&config, "dev", "design");
        // Should still produce valid output with 0 frames
        assert!(data.len() > SOF_MAGIC.len());
        // Last 2 bytes are CRC
        let crc_offset = data.len() - 2;
        let stored_crc = u16::from_be_bytes([data[crc_offset], data[crc_offset + 1]]);
        let computed_crc = crc16(&data[..crc_offset]);
        assert_eq!(stored_crc, computed_crc);
    }

    #[test]
    fn sof_single_frame() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let data = write_sof(&config, "dev", "test");
        assert!(data.len() > SOF_MAGIC.len() + 20); // header + at least one frame
    }

    #[test]
    fn sof_multiple_frames() {
        let mut config = ConfigImage::new(4, 10);
        for i in 0..5 {
            config.set_bit(ConfigBit {
                frame: FrameAddress::from_raw(i),
                bit_offset: 0,
                value: true,
            });
        }
        let data = write_sof(&config, "dev", "test");
        // Should be larger than single frame
        let single_config = ConfigImage::new(4, 10);
        let single_data = write_sof(&single_config, "dev", "test");
        assert!(data.len() > single_data.len());
    }

    #[test]
    fn sof_crc_validates() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(3),
            bit_offset: 7,
            value: true,
        });
        let data = write_sof(&config, "EP4CE6", "blinky");
        let crc_offset = data.len() - 2;
        let stored_crc = u16::from_be_bytes([data[crc_offset], data[crc_offset + 1]]);
        let computed_crc = crc16(&data[..crc_offset]);
        assert_eq!(stored_crc, computed_crc);
    }

    #[test]
    fn sof_deterministic() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let a = write_sof(&config, "dev", "test");
        let b = write_sof(&config, "dev", "test");
        assert_eq!(a, b);
    }
}
