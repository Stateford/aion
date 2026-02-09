//! Simplified Intel/Altera configuration bit database.
//!
//! Provides a structurally valid but not hardware-accurate mapping from
//! logical cell/PIP configurations to physical configuration bits. This
//! placeholder produces deterministic, well-formed bitstreams suitable
//! for testing and development. It will be replaced with real Mistral
//! database data for production use.

use crate::config_bits::{ConfigBit, ConfigBitDatabase, FrameAddress};
use aion_arch::ids::{PipId, SiteId};
use aion_common::LogicVec;
use aion_ir::PortDirection;

/// Number of 32-bit words per configuration frame for Intel devices.
const INTEL_FRAME_WORD_COUNT: u32 = 40;

/// Total number of configuration frames for Intel devices.
const INTEL_TOTAL_FRAMES: u32 = 100;

/// Stride between LUT sites in frame address space.
const LUT_STRIDE: u32 = 4;

/// Simplified configuration bit database for Intel/Altera devices.
///
/// Maps cells and PIPs to config bits using deterministic formulas based
/// on site/PIP IDs. The mapping is structurally valid (bits are within
/// frame bounds) but does not match real hardware.
#[derive(Debug)]
pub struct SimplifiedIntelDb;

impl ConfigBitDatabase for SimplifiedIntelDb {
    fn lut_config_bits(&self, site: SiteId, init: &LogicVec, _input_count: u8) -> Vec<ConfigBit> {
        let base_frame = site.as_raw() * LUT_STRIDE;
        let mut bits = Vec::new();

        for i in 0..init.width().min(INTEL_FRAME_WORD_COUNT * 32) {
            let val = init.get(i);
            bits.push(ConfigBit {
                frame: FrameAddress::from_raw(base_frame % INTEL_TOTAL_FRAMES),
                bit_offset: i,
                value: val == aion_common::Logic::One,
            });
        }

        bits
    }

    fn ff_config_bits(&self, site: SiteId) -> Vec<ConfigBit> {
        let frame = (site.as_raw() * LUT_STRIDE + 1) % INTEL_TOTAL_FRAMES;
        vec![ConfigBit {
            frame: FrameAddress::from_raw(frame),
            bit_offset: site.as_raw() % (INTEL_FRAME_WORD_COUNT * 32),
            value: true, // FF enable bit
        }]
    }

    fn iobuf_config_bits(
        &self,
        site: SiteId,
        direction: PortDirection,
        _standard: &str,
    ) -> Vec<ConfigBit> {
        let frame = (site.as_raw() * 2) % INTEL_TOTAL_FRAMES;
        let dir_bit = match direction {
            PortDirection::Input => 0,
            PortDirection::Output => 1,
            PortDirection::InOut => 2,
        };
        vec![
            ConfigBit {
                frame: FrameAddress::from_raw(frame),
                bit_offset: site.as_raw() % (INTEL_FRAME_WORD_COUNT * 32),
                value: true, // IO enable
            },
            ConfigBit {
                frame: FrameAddress::from_raw(frame),
                bit_offset: (site.as_raw() + 1) % (INTEL_FRAME_WORD_COUNT * 32),
                value: dir_bit & 1 != 0, // direction bit 0
            },
            ConfigBit {
                frame: FrameAddress::from_raw(frame),
                bit_offset: (site.as_raw() + 2) % (INTEL_FRAME_WORD_COUNT * 32),
                value: dir_bit & 2 != 0, // direction bit 1
            },
        ]
    }

    fn pip_config_bits(&self, pip: PipId) -> Vec<ConfigBit> {
        let frame = (pip.as_raw() / 32) % INTEL_TOTAL_FRAMES;
        let bit = pip.as_raw() % (INTEL_FRAME_WORD_COUNT * 32);
        vec![ConfigBit {
            frame: FrameAddress::from_raw(frame),
            bit_offset: bit,
            value: true,
        }]
    }

    fn bram_config_bits(&self, site: SiteId, width: u32, _depth: u32) -> Vec<ConfigBit> {
        let base_frame = (site.as_raw() * 8 + 50) % INTEL_TOTAL_FRAMES;
        let mut bits = Vec::new();
        // Width configuration bits
        for i in 0..width.min(16) {
            bits.push(ConfigBit {
                frame: FrameAddress::from_raw(base_frame),
                bit_offset: i,
                value: (width >> i) & 1 != 0,
            });
        }
        // Enable bit
        bits.push(ConfigBit {
            frame: FrameAddress::from_raw(base_frame),
            bit_offset: 16,
            value: true,
        });
        bits
    }

    fn dsp_config_bits(&self, site: SiteId, width_a: u32, _width_b: u32) -> Vec<ConfigBit> {
        let base_frame = (site.as_raw() * 8 + 70) % INTEL_TOTAL_FRAMES;
        let mut bits = Vec::new();
        // Width-A configuration
        for i in 0..width_a.min(8) {
            bits.push(ConfigBit {
                frame: FrameAddress::from_raw(base_frame),
                bit_offset: i,
                value: (width_a >> i) & 1 != 0,
            });
        }
        // Enable bit
        bits.push(ConfigBit {
            frame: FrameAddress::from_raw(base_frame),
            bit_offset: 8,
            value: true,
        });
        bits
    }

    fn frame_word_count(&self) -> u32 {
        INTEL_FRAME_WORD_COUNT
    }

    fn total_frame_count(&self) -> u32 {
        INTEL_TOTAL_FRAMES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lut_produces_bits() {
        let db = SimplifiedIntelDb;
        let init = LogicVec::all_zero(16);
        let bits = db.lut_config_bits(SiteId::from_raw(0), &init, 4);
        assert_eq!(bits.len(), 16);
    }

    #[test]
    fn lut_bits_reflect_init() {
        let db = SimplifiedIntelDb;
        let mut init = LogicVec::all_zero(4);
        init.set(0, aion_common::Logic::One);
        init.set(2, aion_common::Logic::One);
        let bits = db.lut_config_bits(SiteId::from_raw(0), &init, 4);
        assert!(bits[0].value);
        assert!(!bits[1].value);
        assert!(bits[2].value);
        assert!(!bits[3].value);
    }

    #[test]
    fn ff_produces_enable_bit() {
        let db = SimplifiedIntelDb;
        let bits = db.ff_config_bits(SiteId::from_raw(0));
        assert_eq!(bits.len(), 1);
        assert!(bits[0].value);
    }

    #[test]
    fn iobuf_produces_direction_bits() {
        let db = SimplifiedIntelDb;
        let input_bits =
            db.iobuf_config_bits(SiteId::from_raw(0), PortDirection::Input, "LVCMOS33");
        let output_bits =
            db.iobuf_config_bits(SiteId::from_raw(0), PortDirection::Output, "LVCMOS33");
        assert_eq!(input_bits.len(), 3);
        assert_eq!(output_bits.len(), 3);
        // Direction bits differ
        assert_ne!(input_bits[1].value, output_bits[1].value);
    }

    #[test]
    fn pip_produces_single_bit() {
        let db = SimplifiedIntelDb;
        let bits = db.pip_config_bits(PipId::from_raw(0));
        assert_eq!(bits.len(), 1);
        assert!(bits[0].value);
    }

    #[test]
    fn bram_produces_bits() {
        let db = SimplifiedIntelDb;
        let bits = db.bram_config_bits(SiteId::from_raw(0), 18, 1024);
        assert!(!bits.is_empty());
        // Last bit is enable
        assert!(bits.last().unwrap().value);
    }

    #[test]
    fn dsp_produces_bits() {
        let db = SimplifiedIntelDb;
        let bits = db.dsp_config_bits(SiteId::from_raw(0), 18, 18);
        assert!(!bits.is_empty());
        assert!(bits.last().unwrap().value);
    }

    #[test]
    fn frame_dimensions() {
        let db = SimplifiedIntelDb;
        assert_eq!(db.frame_word_count(), INTEL_FRAME_WORD_COUNT);
        assert_eq!(db.total_frame_count(), INTEL_TOTAL_FRAMES);
    }

    #[test]
    fn bits_within_valid_range() {
        let db = SimplifiedIntelDb;
        let max_bit = INTEL_FRAME_WORD_COUNT * 32;
        let max_frame = INTEL_TOTAL_FRAMES;

        // Check LUT bits
        let bits = db.lut_config_bits(SiteId::from_raw(5), &LogicVec::all_zero(16), 4);
        for bit in &bits {
            assert!(
                bit.frame.as_raw() < max_frame,
                "frame {} >= {}",
                bit.frame.as_raw(),
                max_frame
            );
            assert!(
                bit.bit_offset < max_bit,
                "bit_offset {} >= {}",
                bit.bit_offset,
                max_bit
            );
        }

        // Check FF bits
        let bits = db.ff_config_bits(SiteId::from_raw(10));
        for bit in &bits {
            assert!(bit.frame.as_raw() < max_frame);
            assert!(bit.bit_offset < max_bit);
        }

        // Check IO bits
        let bits = db.iobuf_config_bits(SiteId::from_raw(20), PortDirection::Output, "LVTTL");
        for bit in &bits {
            assert!(bit.frame.as_raw() < max_frame);
            assert!(bit.bit_offset < max_bit);
        }

        // Check PIP bits
        let bits = db.pip_config_bits(PipId::from_raw(500));
        for bit in &bits {
            assert!(bit.frame.as_raw() < max_frame);
            assert!(bit.bit_offset < max_bit);
        }
    }

    #[test]
    fn different_sites_produce_different_frames() {
        let db = SimplifiedIntelDb;
        let bits_0 = db.lut_config_bits(SiteId::from_raw(0), &LogicVec::all_zero(16), 4);
        let bits_1 = db.lut_config_bits(SiteId::from_raw(1), &LogicVec::all_zero(16), 4);
        // Different sites should map to different frames
        assert_ne!(bits_0[0].frame, bits_1[0].frame);
    }
}
