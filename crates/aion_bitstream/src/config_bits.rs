//! Configuration bit data structures and database trait.
//!
//! Defines the types used to represent individual configuration bits, frames,
//! and the accumulated configuration image. The `ConfigBitDatabase` trait
//! abstracts the mapping from logical cells/PIPs to physical configuration bits.

use aion_arch::ids::{PipId, SiteId};
use aion_common::LogicVec;
use aion_ir::PortDirection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An opaque frame address identifying a single configuration frame.
///
/// Frame addresses are ordered so that frames can be written to the bitstream
/// in a deterministic, sorted order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FrameAddress(u32);

impl FrameAddress {
    /// Creates a new frame address from a raw index.
    pub fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Returns the raw numeric value of this frame address.
    pub fn as_raw(self) -> u32 {
        self.0
    }
}

/// A single configuration bit in the device fabric.
///
/// Identifies a specific bit position within a specific frame, along with
/// the value to program (true = 1, false = 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConfigBit {
    /// The frame containing this bit.
    pub frame: FrameAddress,
    /// The bit offset within the frame (in bits, not words).
    pub bit_offset: u32,
    /// The value to program (true = 1).
    pub value: bool,
}

/// A single configuration frame containing packed 32-bit words.
///
/// Each frame has a unique address and a fixed-size data payload. Bits within
/// the frame are set by `ConfigImage::set_bit()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFrame {
    /// The address of this frame.
    pub address: FrameAddress,
    /// Packed 32-bit words forming the frame data.
    pub data: Vec<u32>,
}

/// An accumulated configuration image built from individual config bits.
///
/// Frames are created on demand as bits are set. After all bits have been
/// programmed, call `finalize()` to get sorted frames ready for serialization
/// into a bitstream file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigImage {
    /// Frames indexed by address for efficient random access during assembly.
    frames: BTreeMap<FrameAddress, Vec<u32>>,
    /// Number of 32-bit words per frame.
    pub frame_word_count: u32,
    /// Total number of frames in the device.
    pub total_frame_count: u32,
}

impl ConfigImage {
    /// Creates a new empty configuration image.
    ///
    /// All frames start with zero-filled data. Frames are allocated on demand
    /// when bits are set.
    pub fn new(frame_word_count: u32, total_frame_count: u32) -> Self {
        Self {
            frames: BTreeMap::new(),
            frame_word_count,
            total_frame_count,
        }
    }

    /// Sets a single configuration bit in the image.
    ///
    /// Creates the frame if it doesn't exist yet. Bits are packed into 32-bit
    /// words in big-endian bit order (bit 0 = MSB of word 0).
    pub fn set_bit(&mut self, bit: ConfigBit) {
        let word_count = self.frame_word_count as usize;
        let frame_data = self
            .frames
            .entry(bit.frame)
            .or_insert_with(|| vec![0u32; word_count]);

        let word_idx = (bit.bit_offset / 32) as usize;
        let bit_idx = bit.bit_offset % 32;

        if word_idx < frame_data.len() {
            if bit.value {
                frame_data[word_idx] |= 1 << bit_idx;
            } else {
                frame_data[word_idx] &= !(1 << bit_idx);
            }
        }
    }

    /// Returns the number of frames that have been modified.
    pub fn active_frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Finalizes the image and returns sorted configuration frames.
    ///
    /// Frames are returned in ascending address order, suitable for
    /// sequential programming into the device.
    pub fn finalize(&self) -> Vec<ConfigFrame> {
        self.frames
            .iter()
            .map(|(&addr, data)| ConfigFrame {
                address: addr,
                data: data.clone(),
            })
            .collect()
    }

    /// Returns whether any bits have been set in the image.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// Trait for mapping logical cell/PIP configurations to physical config bits.
///
/// Implementations translate the logical placement (which cell at which site)
/// into specific frame addresses and bit offsets. Different device families
/// have different mappings; simplified placeholder implementations are used
/// until real vendor databases (Mistral/Project X-Ray) are integrated.
pub trait ConfigBitDatabase {
    /// Returns the configuration bits for a LUT at the given site.
    ///
    /// The `init` parameter contains the LUT initialization (truth table) bits,
    /// and `input_count` is the number of LUT inputs (typically 4 or 6).
    fn lut_config_bits(&self, site: SiteId, init: &LogicVec, input_count: u8) -> Vec<ConfigBit>;

    /// Returns the configuration bits for a flip-flop at the given site.
    fn ff_config_bits(&self, site: SiteId) -> Vec<ConfigBit>;

    /// Returns the configuration bits for an I/O buffer at the given site.
    ///
    /// The `direction` and `standard` parameters specify the I/O configuration
    /// (e.g., input LVCMOS33, output LVTTL).
    fn iobuf_config_bits(
        &self,
        site: SiteId,
        direction: PortDirection,
        standard: &str,
    ) -> Vec<ConfigBit>;

    /// Returns the configuration bits for a programmable interconnect point.
    fn pip_config_bits(&self, pip: PipId) -> Vec<ConfigBit>;

    /// Returns the configuration bits for a block RAM at the given site.
    fn bram_config_bits(&self, site: SiteId, width: u32, depth: u32) -> Vec<ConfigBit>;

    /// Returns the configuration bits for a DSP block at the given site.
    fn dsp_config_bits(&self, site: SiteId, width_a: u32, width_b: u32) -> Vec<ConfigBit>;

    /// Returns the number of 32-bit words in each configuration frame.
    fn frame_word_count(&self) -> u32;

    /// Returns the total number of configuration frames for the device.
    fn total_frame_count(&self) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_address_roundtrip() {
        let addr = FrameAddress::from_raw(42);
        assert_eq!(addr.as_raw(), 42);
    }

    #[test]
    fn frame_address_ordering() {
        let a = FrameAddress::from_raw(1);
        let b = FrameAddress::from_raw(5);
        let c = FrameAddress::from_raw(3);
        let mut addrs = [b, c, a];
        addrs.sort();
        assert_eq!(addrs[0].as_raw(), 1);
        assert_eq!(addrs[1].as_raw(), 3);
        assert_eq!(addrs[2].as_raw(), 5);
    }

    #[test]
    fn frame_address_serde() {
        let addr = FrameAddress::from_raw(99);
        let json = serde_json::to_string(&addr).unwrap();
        let back: FrameAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(addr, back);
    }

    #[test]
    fn config_bit_creation() {
        let bit = ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 15,
            value: true,
        };
        assert_eq!(bit.frame.as_raw(), 0);
        assert_eq!(bit.bit_offset, 15);
        assert!(bit.value);
    }

    #[test]
    fn config_image_empty() {
        let img = ConfigImage::new(40, 100);
        assert!(img.is_empty());
        assert_eq!(img.active_frame_count(), 0);
        assert!(img.finalize().is_empty());
    }

    #[test]
    fn config_image_set_bit() {
        let mut img = ConfigImage::new(4, 10);
        img.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 0,
            value: true,
        });
        assert!(!img.is_empty());
        assert_eq!(img.active_frame_count(), 1);

        let frames = img.finalize();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].address.as_raw(), 0);
        assert_eq!(frames[0].data[0] & 1, 1);
    }

    #[test]
    fn config_image_set_bit_high_offset() {
        let mut img = ConfigImage::new(4, 10);
        img.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(2),
            bit_offset: 33, // word 1, bit 1
            value: true,
        });
        let frames = img.finalize();
        assert_eq!(frames[0].data[1] & 0b10, 0b10);
    }

    #[test]
    fn config_image_overwrite_bit() {
        let mut img = ConfigImage::new(4, 10);
        img.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 5,
            value: true,
        });
        // Overwrite to false
        img.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 5,
            value: false,
        });
        let frames = img.finalize();
        assert_eq!(frames[0].data[0] & (1 << 5), 0);
    }

    #[test]
    fn config_image_multiple_frames() {
        let mut img = ConfigImage::new(4, 10);
        img.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(5),
            bit_offset: 0,
            value: true,
        });
        img.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(2),
            bit_offset: 0,
            value: true,
        });
        let frames = img.finalize();
        assert_eq!(frames.len(), 2);
        // Sorted by address
        assert_eq!(frames[0].address.as_raw(), 2);
        assert_eq!(frames[1].address.as_raw(), 5);
    }

    #[test]
    fn config_image_frame_word_count() {
        let mut img = ConfigImage::new(8, 10);
        img.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 200, // word 6
            value: true,
        });
        let frames = img.finalize();
        assert_eq!(frames[0].data.len(), 8);
    }

    #[test]
    fn config_frame_serde() {
        let frame = ConfigFrame {
            address: FrameAddress::from_raw(7),
            data: vec![0xDEAD_BEEF, 0x1234_5678],
        };
        let json = serde_json::to_string(&frame).unwrap();
        let back: ConfigFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(frame, back);
    }

    #[test]
    fn config_bit_serde() {
        let bit = ConfigBit {
            frame: FrameAddress::from_raw(3),
            bit_offset: 42,
            value: true,
        };
        let json = serde_json::to_string(&bit).unwrap();
        let back: ConfigBit = serde_json::from_str(&json).unwrap();
        assert_eq!(bit, back);
    }

    #[test]
    fn config_image_out_of_range_bit_is_noop() {
        let mut img = ConfigImage::new(2, 10); // 2 words = 64 bits max
        img.set_bit(ConfigBit {
            frame: FrameAddress::from_raw(0),
            bit_offset: 200, // way beyond 64 bits
            value: true,
        });
        let frames = img.finalize();
        // Frame was created but bit was out of range, so data is all zeros
        assert_eq!(frames[0].data, vec![0, 0]);
    }
}
