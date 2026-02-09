//! Intel/Altera POF (Programmer Object File) format writer.
//!
//! POF files are used for flash-based programming of Intel/Altera FPGAs.
//! They contain device metadata, CFI flash addressing information, and
//! configuration data with a CRC-16 integrity check.

use crate::config_bits::ConfigImage;
use crate::crc::crc16;

/// Magic bytes identifying a POF file.
const POF_MAGIC: &[u8] = b"AION_POF";

/// POF format version.
const POF_VERSION: u8 = 1;

/// Default flash base address for configuration data.
const CFI_BASE_ADDRESS: u32 = 0x0002_0000;

/// Writes a POF file from the given configuration image.
///
/// The POF format contains:
/// 1. Magic identifier (`AION_POF`)
/// 2. Version byte
/// 3. Device name (length-prefixed)
/// 4. CFI flash base address (u32 BE)
/// 5. Configuration data length (u32 BE)
/// 6. Frame count (u32 BE)
/// 7. Frame word count (u32 BE)
/// 8. Frame data (address + words, all u32 BE)
/// 9. CRC-16 footer
pub fn write_pof(config: &ConfigImage, device_name: &str) -> Vec<u8> {
    let mut data = Vec::new();

    // Header
    data.extend_from_slice(POF_MAGIC);
    data.push(POF_VERSION);

    // Device name
    let dev_bytes = device_name.as_bytes();
    let dev_len = dev_bytes.len().min(u16::MAX as usize) as u16;
    data.extend_from_slice(&dev_len.to_be_bytes());
    data.extend_from_slice(&dev_bytes[..dev_len as usize]);

    // CFI flash metadata
    data.extend_from_slice(&CFI_BASE_ADDRESS.to_be_bytes());

    // Frame data
    let frames = config.finalize();
    let config_data_len = frames.len() as u32 * (1 + config.frame_word_count) * 4; // (addr + words) * 4 bytes
    data.extend_from_slice(&config_data_len.to_be_bytes());
    data.extend_from_slice(&(frames.len() as u32).to_be_bytes());
    data.extend_from_slice(&config.frame_word_count.to_be_bytes());

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_bits::{ConfigBit, ConfigImage, FrameAddress};

    #[test]
    fn pof_magic_header() {
        let config = ConfigImage::new(4, 10);
        let data = write_pof(&config, "EP4CE6");
        assert_eq!(&data[..8], POF_MAGIC);
        assert_eq!(data[8], POF_VERSION);
    }

    #[test]
    fn pof_cfi_address() {
        let config = ConfigImage::new(4, 10);
        let data = write_pof(&config, "dev");
        // After magic(8) + version(1) + dev_name_len(2) + dev_name(3) = offset 14
        let offset = 8 + 1 + 2 + 3; // 14
        let cfi_addr = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        assert_eq!(cfi_addr, CFI_BASE_ADDRESS);
    }

    #[test]
    fn pof_empty_config() {
        let config = ConfigImage::new(4, 10);
        let data = write_pof(&config, "dev");
        let crc_offset = data.len() - 2;
        let stored_crc = u16::from_be_bytes([data[crc_offset], data[crc_offset + 1]]);
        let computed_crc = crc16(&data[..crc_offset]);
        assert_eq!(stored_crc, computed_crc);
    }

    #[test]
    fn pof_with_frames() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let data = write_pof(&config, "EP4CE6");
        assert!(data.len() > POF_MAGIC.len() + 30);
    }

    #[test]
    fn pof_crc_validates() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(1),
            bit_offset: 3,
            value: true,
        });
        let data = write_pof(&config, "EP4CE6");
        let crc_offset = data.len() - 2;
        let stored_crc = u16::from_be_bytes([data[crc_offset], data[crc_offset + 1]]);
        let computed_crc = crc16(&data[..crc_offset]);
        assert_eq!(stored_crc, computed_crc);
    }

    #[test]
    fn pof_deterministic() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let a = write_pof(&config, "dev");
        let b = write_pof(&config, "dev");
        assert_eq!(a, b);
    }
}
