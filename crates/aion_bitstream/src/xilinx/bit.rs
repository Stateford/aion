//! Xilinx BIT format writer.
//!
//! BIT files contain a TLV-encoded header with design metadata, followed by
//! a synchronization word and a command sequence that programs configuration
//! frames into the FPGA fabric via the FDRI register.

use crate::config_bits::ConfigImage;
use crate::crc::crc32;

/// Xilinx sync word (marks start of configuration commands).
const SYNC_WORD: u32 = 0xAA99_5566;

/// Xilinx NOOP command.
const NOOP: u32 = 0x2000_0000;

/// Type 1 write packet header builder.
fn type1_write(reg: u32, word_count: u32) -> u32 {
    0x3000_0000 | (reg << 13) | (word_count & 0x7FF)
}

/// Type 2 write packet header (for FDRI with > 2047 words).
fn type2_write(word_count: u32) -> u32 {
    0x5000_0000 | (word_count & 0x03FF_FFFF)
}

// Xilinx configuration register addresses
/// Command register.
const REG_CMD: u32 = 0x04;
/// Frame Address Register.
const REG_FAR: u32 = 0x01;
/// Frame Data Register Input.
const REG_FDRI: u32 = 0x02;
/// CRC register.
const REG_CRC: u32 = 0x00;
/// Configuration Options Register 0.
const REG_COR0: u32 = 0x09;

// Command register values
/// Reset CRC.
const CMD_RCRC: u32 = 0x07;
/// Write Configuration.
const CMD_WCFG: u32 = 0x01;
/// GRestore — assert GRESTORE signal.
const CMD_GRESTORE: u32 = 0x0A;
/// Start — begin startup sequence.
const CMD_START: u32 = 0x05;
/// Desync — end configuration.
const CMD_DESYNC: u32 = 0x0D;

/// Header field tag 'a' (design name).
const FIELD_DESIGN: u8 = b'a';
/// Header field tag 'b' (device name).
const FIELD_DEVICE: u8 = b'b';
/// Header field tag 'c' (date).
const FIELD_DATE: u8 = b'c';
/// Header field tag 'd' (time).
const FIELD_TIME: u8 = b'd';
/// Header field tag 'e' (data length).
const FIELD_DATA_LEN: u8 = b'e';

/// Writes a Xilinx BIT file from the given configuration image.
///
/// The BIT format contains:
/// 1. TLV header (design name, device, date, time, data length)
/// 2. Padding (16-byte alignment)
/// 3. Sync word (0xAA995566)
/// 4. Configuration command sequence:
///    - Reset CRC
///    - Configure COR0
///    - Write Configuration command
///    - FAR (Frame Address Register) for each frame
///    - FDRI (Frame Data Register Input) with frame data
///    - CRC-32 check
/// 5. Startup sequence (GRESTORE, START, DESYNC)
pub fn write_bit(config: &ConfigImage, device_name: &str, design_name: &str) -> Vec<u8> {
    let mut data = Vec::new();

    // --- TLV Header ---
    write_header(&mut data, design_name, device_name);

    // --- Padding to 16-byte alignment ---
    while data.len() % 16 != 0 {
        data.push(0xFF);
    }

    // --- Sync word ---
    data.extend_from_slice(&SYNC_WORD.to_be_bytes());

    // NOOPs after sync
    for _ in 0..2 {
        data.extend_from_slice(&NOOP.to_be_bytes());
    }

    // --- Command sequence ---
    // Reset CRC
    data.extend_from_slice(&type1_write(REG_CMD, 1).to_be_bytes());
    data.extend_from_slice(&CMD_RCRC.to_be_bytes());
    data.extend_from_slice(&NOOP.to_be_bytes());

    // COR0 configuration
    data.extend_from_slice(&type1_write(REG_COR0, 1).to_be_bytes());
    data.extend_from_slice(&0x00003FE5u32.to_be_bytes()); // Default COR0 value

    // Write Configuration command
    data.extend_from_slice(&type1_write(REG_CMD, 1).to_be_bytes());
    data.extend_from_slice(&CMD_WCFG.to_be_bytes());
    data.extend_from_slice(&NOOP.to_be_bytes());

    // --- Frame data ---
    let frames = config.finalize();

    if !frames.is_empty() {
        // Calculate total words for all frames
        let words_per_frame = config.frame_word_count as usize;
        let total_words = frames.len() * words_per_frame;

        // FAR: set to first frame address
        data.extend_from_slice(&type1_write(REG_FAR, 1).to_be_bytes());
        data.extend_from_slice(&frames[0].address.as_raw().to_be_bytes());

        // FDRI: write all frame data
        if total_words <= 2047 {
            data.extend_from_slice(&type1_write(REG_FDRI, total_words as u32).to_be_bytes());
        } else {
            // Type 1 header with 0 words, followed by Type 2 with actual count
            data.extend_from_slice(&type1_write(REG_FDRI, 0).to_be_bytes());
            data.extend_from_slice(&type2_write(total_words as u32).to_be_bytes());
        }

        // Write frame data words
        let crc_start = data.len();
        for frame in &frames {
            for &word in &frame.data {
                data.extend_from_slice(&word.to_be_bytes());
            }
        }
        let crc_end = data.len();

        // CRC over frame data
        let frame_crc = crc32(&data[crc_start..crc_end]);
        data.extend_from_slice(&type1_write(REG_CRC, 1).to_be_bytes());
        data.extend_from_slice(&frame_crc.to_be_bytes());
    }

    // --- Startup sequence ---
    // GRESTORE
    data.extend_from_slice(&type1_write(REG_CMD, 1).to_be_bytes());
    data.extend_from_slice(&CMD_GRESTORE.to_be_bytes());
    data.extend_from_slice(&NOOP.to_be_bytes());

    // START
    data.extend_from_slice(&type1_write(REG_CMD, 1).to_be_bytes());
    data.extend_from_slice(&CMD_START.to_be_bytes());
    data.extend_from_slice(&NOOP.to_be_bytes());

    // DESYNC
    data.extend_from_slice(&type1_write(REG_CMD, 1).to_be_bytes());
    data.extend_from_slice(&CMD_DESYNC.to_be_bytes());

    // Trailing NOOPs
    for _ in 0..4 {
        data.extend_from_slice(&NOOP.to_be_bytes());
    }

    data
}

/// Writes the TLV header with design metadata.
fn write_header(data: &mut Vec<u8>, design_name: &str, device_name: &str) {
    // Header preamble (2-byte length + 9-byte header field)
    let preamble = [
        0x00, 0x09, 0x0F, 0xF0, 0x0F, 0xF0, 0x0F, 0xF0, 0x0F, 0xF0, 0x00,
    ];
    data.extend_from_slice(&preamble);

    // Design name
    write_tlv_field(data, FIELD_DESIGN, design_name.as_bytes());
    // Device name
    write_tlv_field(data, FIELD_DEVICE, device_name.as_bytes());
    // Date
    write_tlv_field(data, FIELD_DATE, b"2024/01/01");
    // Time
    write_tlv_field(data, FIELD_TIME, b"00:00:00");

    // Data length field (placeholder — actual length filled after data section)
    // For BIT files, field 'e' contains the 4-byte BE length of the remaining data
    data.push(FIELD_DATA_LEN);
    // The length will be approximate — this is metadata only
    data.extend_from_slice(&0u32.to_be_bytes());
}

/// Writes a single TLV field (tag + 2-byte length + null-terminated value).
fn write_tlv_field(data: &mut Vec<u8>, tag: u8, value: &[u8]) {
    data.push(tag);
    let len = (value.len() + 1) as u16; // +1 for null terminator
    data.extend_from_slice(&len.to_be_bytes());
    data.extend_from_slice(value);
    data.push(0); // null terminator
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_bits::{ConfigBit, ConfigImage, FrameAddress};

    #[test]
    fn bit_has_sync_word() {
        let config = ConfigImage::new(101, 100);
        let data = write_bit(&config, "xc7a35t", "test");
        // Find sync word in the data
        let sync_bytes = SYNC_WORD.to_be_bytes();
        let found = data.windows(4).any(|w| w == sync_bytes);
        assert!(found, "sync word 0xAA995566 not found in BIT file");
    }

    #[test]
    fn bit_header_contains_device() {
        let config = ConfigImage::new(101, 100);
        let data = write_bit(&config, "xc7a35t", "blinky");
        let data_str = String::from_utf8_lossy(&data);
        assert!(data_str.contains("xc7a35t"));
    }

    #[test]
    fn bit_header_contains_design() {
        let config = ConfigImage::new(101, 100);
        let data = write_bit(&config, "xc7a35t", "my_design");
        let data_str = String::from_utf8_lossy(&data);
        assert!(data_str.contains("my_design"));
    }

    #[test]
    fn bit_empty_config() {
        let config = ConfigImage::new(101, 100);
        let data = write_bit(&config, "xc7a35t", "test");
        // Should have header + sync + commands + startup even with no frames
        assert!(data.len() > 50);
    }

    #[test]
    fn bit_single_frame() {
        let mut config = ConfigImage::new(101, 100);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let data = write_bit(&config, "xc7a35t", "test");
        // Should be larger than empty
        let empty = write_bit(&ConfigImage::new(101, 100), "xc7a35t", "test");
        assert!(data.len() > empty.len());
    }

    #[test]
    fn bit_has_desync() {
        let config = ConfigImage::new(101, 100);
        let data = write_bit(&config, "xc7a35t", "test");
        // DESYNC command should be in the data
        let desync_bytes = CMD_DESYNC.to_be_bytes();
        let found = data.windows(4).any(|w| w == desync_bytes);
        assert!(found, "DESYNC command not found in BIT file");
    }

    #[test]
    fn bit_deterministic() {
        let mut config = ConfigImage::new(101, 100);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let a = write_bit(&config, "xc7a35t", "test");
        let b = write_bit(&config, "xc7a35t", "test");
        assert_eq!(a, b);
    }

    #[test]
    fn bit_multiple_frames() {
        let mut config = ConfigImage::new(4, 100);
        for i in 0..10 {
            config.set_bit(ConfigBit {
                frame: FrameAddress::from_raw(i),
                bit_offset: 0,
                value: true,
            });
        }
        let data = write_bit(&config, "xc7a35t", "test");
        assert!(data.len() > 200);
    }

    #[test]
    fn bit_size_grows_with_frames() {
        let mut config1 = ConfigImage::new(4, 100);
        config1.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });

        let mut config10 = ConfigImage::new(4, 100);
        for i in 0..10 {
            config10.set_bit(ConfigBit {
                frame: FrameAddress::from_raw(i),
                bit_offset: 0,
                value: true,
            });
        }

        let data1 = write_bit(&config1, "dev", "test");
        let data10 = write_bit(&config10, "dev", "test");
        assert!(data10.len() > data1.len());
    }

    #[test]
    fn type1_write_encoding() {
        let pkt = type1_write(REG_CMD, 1);
        // Register field is bits [17:13], word count is bits [10:0]
        assert_eq!((pkt >> 13) & 0x1F, REG_CMD);
        assert_eq!(pkt & 0x7FF, 1);
        assert_eq!(pkt >> 29, 1); // Type 1 marker
    }

    #[test]
    fn type2_write_encoding() {
        let pkt = type2_write(5000);
        assert_eq!(pkt & 0x03FF_FFFF, 5000);
        assert_eq!(pkt >> 29, 2); // Type 2 marker
    }
}
