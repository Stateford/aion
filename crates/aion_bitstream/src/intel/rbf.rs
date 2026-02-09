//! Intel/Altera RBF (Raw Binary File) format writer.
//!
//! RBF files contain only raw configuration frame data with no header or
//! metadata. They are the simplest bitstream format, used for passive
//! configuration and compressed bitstream loading.

use crate::config_bits::ConfigImage;

/// Writes an RBF file from the given configuration image.
///
/// The RBF format is simply the concatenation of all frame data words
/// in big-endian order, with frames sorted by ascending address. There
/// is no header, footer, or CRC — the data is used as-is for passive
/// serial or active serial configuration.
pub fn write_rbf(config: &ConfigImage) -> Vec<u8> {
    let frames = config.finalize();
    let total_words = frames.len() * config.frame_word_count as usize;
    let mut data = Vec::with_capacity(total_words * 4);

    for frame in &frames {
        for &word in &frame.data {
            data.extend_from_slice(&word.to_be_bytes());
        }
    }

    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_bits::{ConfigBit, ConfigImage, FrameAddress};

    #[test]
    fn rbf_empty() {
        let config = ConfigImage::new(4, 10);
        let data = write_rbf(&config);
        assert!(data.is_empty());
    }

    #[test]
    fn rbf_single_frame() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let data = write_rbf(&config);
        // 4 words × 4 bytes = 16 bytes
        assert_eq!(data.len(), 16);
    }

    #[test]
    fn rbf_no_header() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let data = write_rbf(&config);
        // First 4 bytes should be the first word of frame data, not any magic
        let first_word = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        // Bit 0 of word 0 is set
        assert_eq!(first_word, 1);
    }

    #[test]
    fn rbf_multiple_frames_sorted() {
        let mut config = ConfigImage::new(2, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(5),
            bit_offset: 0,
            value: true,
        });
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(2),
            bit_offset: 1,
            value: true,
        });
        let data = write_rbf(&config);
        // 2 frames × 2 words × 4 bytes = 16 bytes
        assert_eq!(data.len(), 16);
        // First frame (addr 2): word0 bit1 = 0x02
        let word0 = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(word0, 2); // bit 1 set
    }

    #[test]
    fn rbf_deterministic() {
        let mut config = ConfigImage::new(4, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 7,
            value: true,
        });
        let a = write_rbf(&config);
        let b = write_rbf(&config);
        assert_eq!(a, b);
    }

    #[test]
    fn rbf_frame_word_count_respected() {
        let mut config = ConfigImage::new(8, 10);
        config.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        let data = write_rbf(&config);
        // 8 words × 4 bytes = 32 bytes
        assert_eq!(data.len(), 32);
    }
}
