//! ConfigBitDatabase implementation using Project X-Ray segbits data.
//!
//! Maps logical cell/PIP configurations to physical configuration bits by
//! looking up feature names in the segbits database and computing real frame
//! addresses from the tilegrid.
//!
//! # Frame address computation
//!
//! For a feature in tile `CLBLL_L_X16Y149`:
//! 1. Lookup tile in tilegrid → `baseaddr=0x00020800`, `offset=99`, `words=2`
//! 2. Lookup feature in segbits → `[02_15, 02_17]`
//! 3. For each bit entry `(frame_offset=02, bit_position=15)`:
//!    - `frame = baseaddr + frame_offset = 0x00020802`
//!    - `bit_offset = (tile_word_offset * 32) + bit_position = 99*32 + 15 = 3183`

use crate::db::XRayDatabase;
use crate::segbits::SegBitEntry;
use crate::tilegrid::TileGridEntry;
use aion_arch::ids::{PipId, SiteId};
use aion_bitstream::config_bits::{ConfigBit, ConfigBitDatabase, FrameAddress};
use aion_common::LogicVec;
use aion_ir::PortDirection;

/// Number of 32-bit words per configuration frame in Xilinx 7-series devices.
const FRAME_WORD_COUNT: u32 = 101;

/// Estimated total frame count for xc7a35t.
///
/// This is an approximation; the real value depends on the exact device variant.
/// For xc7a35t: ~18,000 frames across all configuration columns.
const DEFAULT_TOTAL_FRAMES: u32 = 18_000;

/// ConfigBitDatabase implementation backed by Project X-Ray data.
///
/// Uses the segbits database to map features to real configuration bit positions
/// and the tilegrid to compute frame addresses.
#[derive(Debug)]
pub struct XRayConfigBitDb {
    /// Reference-counted database (shared with Artix7XRay).
    db: XRayDatabase,
    /// Mapping from SiteId → (tile_name, site_name_within_tile, tile_type).
    site_map: Vec<SiteMapEntry>,
}

/// Information needed to locate a site's configuration bits.
#[derive(Debug, Clone)]
struct SiteMapEntry {
    /// The tile name (e.g., "CLBLL_L_X16Y149").
    tile_name: String,
    /// The site name within the tile (e.g., "SLICEL_X0").
    site_local_name: String,
    /// The tile type (e.g., "CLBLL_L").
    tile_type: String,
}

impl XRayConfigBitDb {
    /// Creates a new config bit database from an X-Ray database.
    ///
    /// Builds an internal mapping from SiteId to tile/site names for
    /// feature lookup during bitstream generation.
    pub fn new(db: XRayDatabase, site_count: usize) -> Self {
        let mut site_map = Vec::with_capacity(site_count);

        // Build site_map by iterating tiles in deterministic order
        let mut tile_names: Vec<&String> = db.tilegrid.keys().collect();
        tile_names.sort();

        for tile_name in tile_names {
            let entry = &db.tilegrid[tile_name];
            let mut site_names: Vec<(&String, &String)> = entry.sites.iter().collect();
            site_names.sort_by_key(|(name, _)| name.as_str());

            for (site_name, site_type) in site_names {
                // Map X-Ray site types to local names used in segbits
                let site_local = site_type_to_segbits_prefix(site_type, site_name, entry);
                if let Some(local_name) = site_local {
                    site_map.push(SiteMapEntry {
                        tile_name: tile_name.clone(),
                        site_local_name: local_name,
                        tile_type: entry.tile_type.clone(),
                    });
                }
            }
        }

        Self { db, site_map }
    }

    /// Translates a segbits entry into a ConfigBit using tilegrid data.
    fn segbit_to_config_bit(
        &self,
        tile_entry: &TileGridEntry,
        seg_entry: &SegBitEntry,
    ) -> Option<ConfigBit> {
        // Use the primary bit segment (typically CLB_IO_CLK)
        let bit_seg = tile_entry.bits.values().next()?;

        let frame_addr = bit_seg.baseaddr + seg_entry.frame_offset;
        let bit_offset = bit_seg.offset * 32 + seg_entry.bit_position;
        let value = !seg_entry.inverted;

        Some(ConfigBit {
            frame: FrameAddress::from_raw(frame_addr),
            bit_offset,
            value,
        })
    }

    /// Looks up config bits for a feature in the segbits database.
    fn lookup_feature_bits(
        &self,
        tile_name: &str,
        feature: &str,
        tile_type: &str,
    ) -> Vec<ConfigBit> {
        let tile_entry = match self.db.tilegrid.get(tile_name) {
            Some(e) => e,
            None => return Vec::new(),
        };

        let segbits = match self.db.segbits_for_tile_type(tile_type) {
            Some(sb) => sb,
            None => return Vec::new(),
        };

        let full_feature = format!("{tile_type}.{feature}");
        let entries = match segbits.get(&full_feature) {
            Some(e) => e,
            None => return Vec::new(),
        };

        entries
            .iter()
            .filter_map(|se| self.segbit_to_config_bit(tile_entry, se))
            .collect()
    }
}

/// Maps an X-Ray site type + site name to the segbits prefix used in feature names.
///
/// For example, in a CLBLL_L tile, the first SLICEL is "SLICEL_X0" in segbits,
/// and the second is "SLICEL_X1".
fn site_type_to_segbits_prefix(
    site_type: &str,
    site_name: &str,
    tile_entry: &TileGridEntry,
) -> Option<String> {
    match site_type {
        "SLICEL" | "SLICEM" => {
            // Determine X index within the tile
            let mut sorted_sites: Vec<(&String, &String)> = tile_entry
                .sites
                .iter()
                .filter(|(_, st)| *st == site_type)
                .collect();
            sorted_sites.sort_by_key(|(n, _)| n.as_str());

            let idx = sorted_sites
                .iter()
                .position(|(n, _)| *n == site_name)
                .unwrap_or(0);
            Some(format!("{site_type}_X{idx}"))
        }
        "IOB33M" | "IOB33S" | "IOB33" => {
            // IO sites use Y-index within the tile
            let mut sorted_sites: Vec<(&String, &String)> = tile_entry
                .sites
                .iter()
                .filter(|(_, st)| st.starts_with("IOB"))
                .collect();
            sorted_sites.sort_by_key(|(n, _)| n.as_str());

            let idx = sorted_sites
                .iter()
                .position(|(n, _)| *n == site_name)
                .unwrap_or(0);
            Some(format!("IOB_Y{idx}"))
        }
        "RAMB36E1" => Some("RAMB36".to_string()),
        "RAMB18E1" => Some("RAMB18".to_string()),
        "DSP48E1" => Some("DSP48E1".to_string()),
        "MMCME2_ADV" => Some("MMCME2".to_string()),
        "PLLE2_ADV" => Some("PLLE2".to_string()),
        _ => None,
    }
}

impl ConfigBitDatabase for XRayConfigBitDb {
    fn lut_config_bits(&self, site: SiteId, init: &LogicVec, _input_count: u8) -> Vec<ConfigBit> {
        let entry = match self.site_map.get(site.as_raw() as usize) {
            Some(e) => e,
            None => return Vec::new(),
        };

        let mut bits = Vec::new();
        let width = init.width();

        // Map each init bit to the corresponding ALUT/BLUT/CLUT/DLUT INIT[N] feature
        // Default to ALUT; real mapping would check the BEL assignment
        for i in 0..width {
            let logic_val = init.get(i);
            if logic_val == aion_common::Logic::One {
                let feature = format!("{}.ALUT.INIT[{i:02}]", entry.site_local_name);
                let mut fb = self.lookup_feature_bits(&entry.tile_name, &feature, &entry.tile_type);
                bits.append(&mut fb);
            }
        }

        bits
    }

    fn ff_config_bits(&self, site: SiteId) -> Vec<ConfigBit> {
        let entry = match self.site_map.get(site.as_raw() as usize) {
            Some(e) => e,
            None => return Vec::new(),
        };

        // Enable the FF by setting the ZINI and ZRST features
        let mut bits = Vec::new();
        for suffix in &["AFF.ZINI", "AFF.ZRST"] {
            let feature = format!("{}.{suffix}", entry.site_local_name);
            let mut fb = self.lookup_feature_bits(&entry.tile_name, &feature, &entry.tile_type);
            bits.append(&mut fb);
        }
        bits
    }

    fn iobuf_config_bits(
        &self,
        site: SiteId,
        _direction: PortDirection,
        _standard: &str,
    ) -> Vec<ConfigBit> {
        let entry = match self.site_map.get(site.as_raw() as usize) {
            Some(e) => e,
            None => return Vec::new(),
        };

        // Set pulltype and drive strength features
        let mut bits = Vec::new();
        for suffix in &["PULLTYPE.PULLDOWN", "DRIVE.I12", "SLEW.SLOW"] {
            let feature = format!("{}.{suffix}", entry.site_local_name);
            let mut fb = self.lookup_feature_bits(&entry.tile_name, &feature, &entry.tile_type);
            bits.append(&mut fb);
        }
        bits
    }

    fn pip_config_bits(&self, _pip: PipId) -> Vec<ConfigBit> {
        // PIP config bits will be properly mapped in Milestone 4
        // when we have a real routing graph with PIP → tile/wire mapping
        Vec::new()
    }

    fn bram_config_bits(&self, site: SiteId, _width: u32, _depth: u32) -> Vec<ConfigBit> {
        let entry = match self.site_map.get(site.as_raw() as usize) {
            Some(e) => e,
            None => return Vec::new(),
        };

        // Set basic BRAM enable features
        let feature = format!("{}.EN", entry.site_local_name);
        self.lookup_feature_bits(&entry.tile_name, &feature, &entry.tile_type)
    }

    fn dsp_config_bits(&self, site: SiteId, _width_a: u32, _width_b: u32) -> Vec<ConfigBit> {
        let entry = match self.site_map.get(site.as_raw() as usize) {
            Some(e) => e,
            None => return Vec::new(),
        };

        // Set basic DSP enable features
        let feature = format!("{}.EN", entry.site_local_name);
        self.lookup_feature_bits(&entry.tile_name, &feature, &entry.tile_type)
    }

    fn frame_word_count(&self) -> u32 {
        FRAME_WORD_COUNT
    }

    fn total_frame_count(&self) -> u32 {
        DEFAULT_TOTAL_FRAMES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segbits::SegBitsMap;
    use crate::tilegrid::{TileBitSegment, TileGrid, TileGridEntry};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_test_db_with_segbits() -> XRayDatabase {
        let mut tilegrid: TileGrid = HashMap::new();

        let mut clb_bits = HashMap::new();
        clb_bits.insert(
            "CLB_IO_CLK".to_string(),
            TileBitSegment {
                baseaddr: 0x00020800,
                frames: 36,
                offset: 99,
                words: 2,
            },
        );
        let mut clb_sites = HashMap::new();
        clb_sites.insert("SLICE_X0Y0".to_string(), "SLICEL".to_string());
        tilegrid.insert(
            "CLBLL_L_X0Y0".to_string(),
            TileGridEntry {
                bits: clb_bits,
                grid_x: 0,
                grid_y: 0,
                tile_type: "CLBLL_L".to_string(),
                sites: clb_sites,
            },
        );

        // Build segbits
        let mut segbits_clbll: SegBitsMap = HashMap::new();
        segbits_clbll.insert(
            "CLBLL_L.SLICEL_X0.ALUT.INIT[00]".to_string(),
            vec![SegBitEntry {
                frame_offset: 0,
                bit_position: 14,
                inverted: false,
            }],
        );
        segbits_clbll.insert(
            "CLBLL_L.SLICEL_X0.ALUT.INIT[01]".to_string(),
            vec![SegBitEntry {
                frame_offset: 0,
                bit_position: 16,
                inverted: false,
            }],
        );
        segbits_clbll.insert(
            "CLBLL_L.SLICEL_X0.AFF.ZINI".to_string(),
            vec![SegBitEntry {
                frame_offset: 1,
                bit_position: 42,
                inverted: true,
            }],
        );
        segbits_clbll.insert(
            "CLBLL_L.SLICEL_X0.AFF.ZRST".to_string(),
            vec![SegBitEntry {
                frame_offset: 1,
                bit_position: 43,
                inverted: false,
            }],
        );

        let mut segbits = HashMap::new();
        segbits.insert("CLBLL_L".to_string(), segbits_clbll);

        XRayDatabase {
            part: "xc7a35t".to_string(),
            tilegrid,
            segbits,
            tile_types: HashMap::new(),
            db_path: PathBuf::from("/test"),
        }
    }

    #[test]
    fn config_bit_db_frame_constants() {
        let db = make_test_db_with_segbits();
        let config_db = XRayConfigBitDb::new(db, 1);
        assert_eq!(config_db.frame_word_count(), 101);
        assert_eq!(config_db.total_frame_count(), 18_000);
    }

    #[test]
    fn lut_config_bits_single_init_bit() {
        let db = make_test_db_with_segbits();
        let config_db = XRayConfigBitDb::new(db, 1);

        // Create a 2-bit init vector with only bit 0 set
        let mut init = LogicVec::new(2);
        init.set(0, aion_common::Logic::One);
        init.set(1, aion_common::Logic::Zero);

        let bits = config_db.lut_config_bits(SiteId::from_raw(0), &init, 6);
        assert_eq!(bits.len(), 1);
        assert_eq!(bits[0].frame.as_raw(), 0x00020800); // baseaddr + 0
        assert_eq!(bits[0].bit_offset, 99 * 32 + 14); // offset*32 + bit_pos
        assert!(bits[0].value);
    }

    #[test]
    fn lut_config_bits_two_init_bits() {
        let db = make_test_db_with_segbits();
        let config_db = XRayConfigBitDb::new(db, 1);

        let mut init = LogicVec::new(2);
        init.set(0, aion_common::Logic::One);
        init.set(1, aion_common::Logic::One);

        let bits = config_db.lut_config_bits(SiteId::from_raw(0), &init, 6);
        assert_eq!(bits.len(), 2);
    }

    #[test]
    fn lut_config_bits_all_zero() {
        let db = make_test_db_with_segbits();
        let config_db = XRayConfigBitDb::new(db, 1);

        let init = LogicVec::new(4);
        let bits = config_db.lut_config_bits(SiteId::from_raw(0), &init, 6);
        assert!(bits.is_empty());
    }

    #[test]
    fn ff_config_bits() {
        let db = make_test_db_with_segbits();
        let config_db = XRayConfigBitDb::new(db, 1);

        let bits = config_db.ff_config_bits(SiteId::from_raw(0));
        assert_eq!(bits.len(), 2); // ZINI + ZRST

        // ZINI is inverted (segbit has inverted=true), so value should be false
        let zini_bit = bits.iter().find(|b| b.bit_offset == 99 * 32 + 42);
        assert!(zini_bit.is_some());
        assert!(!zini_bit.unwrap().value); // inverted

        // ZRST is normal
        let zrst_bit = bits.iter().find(|b| b.bit_offset == 99 * 32 + 43);
        assert!(zrst_bit.is_some());
        assert!(zrst_bit.unwrap().value);
    }

    #[test]
    fn config_bits_invalid_site_id() {
        let db = make_test_db_with_segbits();
        let config_db = XRayConfigBitDb::new(db, 1);

        let init = LogicVec::new(4);
        let bits = config_db.lut_config_bits(SiteId::from_raw(999), &init, 6);
        assert!(bits.is_empty());
    }

    #[test]
    fn pip_config_bits_placeholder() {
        let db = make_test_db_with_segbits();
        let config_db = XRayConfigBitDb::new(db, 1);

        let bits = config_db.pip_config_bits(PipId::from_raw(0));
        assert!(bits.is_empty()); // Placeholder until Milestone 4
    }

    #[test]
    fn frame_address_computation() {
        let db = make_test_db_with_segbits();
        let config_db = XRayConfigBitDb::new(db, 1);

        // Check that frame addresses are correct for LUT bits
        let mut init = LogicVec::new(2);
        init.set(0, aion_common::Logic::One);

        let bits = config_db.lut_config_bits(SiteId::from_raw(0), &init, 6);
        assert_eq!(bits[0].frame.as_raw(), 0x00020800);
    }

    #[test]
    fn site_type_to_prefix_slicel() {
        let mut sites = HashMap::new();
        sites.insert("SLICE_X0Y0".to_string(), "SLICEL".to_string());
        sites.insert("SLICE_X1Y0".to_string(), "SLICEL".to_string());

        let entry = TileGridEntry {
            bits: HashMap::new(),
            grid_x: 0,
            grid_y: 0,
            tile_type: "CLBLL_L".to_string(),
            sites,
        };

        let prefix = site_type_to_segbits_prefix("SLICEL", "SLICE_X0Y0", &entry);
        assert_eq!(prefix.as_deref(), Some("SLICEL_X0"));

        let prefix = site_type_to_segbits_prefix("SLICEL", "SLICE_X1Y0", &entry);
        assert_eq!(prefix.as_deref(), Some("SLICEL_X1"));
    }

    #[test]
    fn site_type_to_prefix_io() {
        let mut sites = HashMap::new();
        sites.insert("IOB_X0Y0".to_string(), "IOB33M".to_string());
        sites.insert("IOB_X0Y1".to_string(), "IOB33S".to_string());

        let entry = TileGridEntry {
            bits: HashMap::new(),
            grid_x: 0,
            grid_y: 0,
            tile_type: "LIOB33".to_string(),
            sites,
        };

        let prefix = site_type_to_segbits_prefix("IOB33M", "IOB_X0Y0", &entry);
        assert_eq!(prefix.as_deref(), Some("IOB_Y0"));
    }

    #[test]
    fn site_type_to_prefix_bram() {
        let entry = TileGridEntry {
            bits: HashMap::new(),
            grid_x: 0,
            grid_y: 0,
            tile_type: "BRAM_L".to_string(),
            sites: HashMap::new(),
        };

        assert_eq!(
            site_type_to_segbits_prefix("RAMB36E1", "RAMB36_X0Y0", &entry),
            Some("RAMB36".to_string())
        );
        assert_eq!(
            site_type_to_segbits_prefix("DSP48E1", "DSP48_X0Y0", &entry),
            Some("DSP48E1".to_string())
        );
    }

    #[test]
    fn site_type_to_prefix_unknown() {
        let entry = TileGridEntry {
            bits: HashMap::new(),
            grid_x: 0,
            grid_y: 0,
            tile_type: "UNKNOWN".to_string(),
            sites: HashMap::new(),
        };

        assert!(site_type_to_segbits_prefix("UNKNOWN_TYPE", "S0", &entry).is_none());
    }
}
