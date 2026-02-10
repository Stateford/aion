//! Parser for Project X-Ray `tilegrid.json` files.
//!
//! The tilegrid describes the physical layout of tiles on the FPGA die, including
//! each tile's grid position, type, frame base address, and site assignments.
//! Frame base addresses are used during bitstream generation to map features to
//! their physical configuration frame locations.

use serde::Deserialize;
use std::collections::HashMap;

/// A segment of configuration bits for one bus within a tile.
///
/// Each tile may have one or more bit segments (typically just `CLB_IO_CLK`),
/// each specifying where in the bitstream the tile's configuration data lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileBitSegment {
    /// The base frame address for this segment (e.g., 0x00020800).
    pub baseaddr: u32,
    /// Number of configuration frames spanned by this segment.
    pub frames: u32,
    /// Word offset within each frame where this tile's data starts.
    pub offset: u32,
    /// Number of 32-bit words per frame used by this tile.
    pub words: u32,
}

/// A single tile entry from the tilegrid.
///
/// Contains the tile's grid position, type, configuration bit layout, and
/// the sites (placement locations) it contains.
#[derive(Debug, Clone)]
pub struct TileGridEntry {
    /// Configuration bit segments indexed by bus name (e.g., "CLB_IO_CLK").
    pub bits: HashMap<String, TileBitSegment>,
    /// Column position in the tile grid.
    pub grid_x: u32,
    /// Row position in the tile grid.
    pub grid_y: u32,
    /// The tile type string (e.g., "CLBLL_L", "INT_L", "BRAM_L").
    pub tile_type: String,
    /// Sites within this tile, mapping site name to site type.
    pub sites: HashMap<String, String>,
}

/// The complete tilegrid for a device, mapping tile names to their entries.
pub type TileGrid = HashMap<String, TileGridEntry>;

/// Raw JSON structure for bit segment deserialization.
#[derive(Deserialize)]
struct RawBitSegment {
    baseaddr: String,
    frames: u32,
    offset: u32,
    words: u32,
}

/// Raw JSON structure for a tile entry deserialization.
#[derive(Deserialize)]
struct RawTileEntry {
    bits: HashMap<String, RawBitSegment>,
    grid_x: u32,
    grid_y: u32,
    #[serde(rename = "type")]
    tile_type: String,
    #[serde(default)]
    sites: HashMap<String, String>,
}

/// Parses a hex string like "0x00020800" into a u32.
fn parse_hex_addr(s: &str) -> Result<u32, String> {
    let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
    match stripped {
        Some(hex) => {
            u32::from_str_radix(hex, 16).map_err(|e| format!("invalid hex address '{s}': {e}"))
        }
        None => u32::from_str_radix(s, 16)
            .map_err(|e| format!("invalid hex address '{s}' (no 0x prefix): {e}")),
    }
}

/// Parses a tilegrid JSON string into a [`TileGrid`].
///
/// The input should be the contents of a `tilegrid.json` file from the
/// Project X-Ray database.
///
/// # Errors
///
/// Returns an error string if the JSON is malformed or contains invalid
/// hex base addresses.
pub fn parse_tilegrid(json: &str) -> Result<TileGrid, String> {
    let raw: HashMap<String, RawTileEntry> =
        serde_json::from_str(json).map_err(|e| format!("tilegrid JSON parse error: {e}"))?;

    let mut grid = HashMap::with_capacity(raw.len());
    for (tile_name, raw_entry) in raw {
        let mut bits = HashMap::with_capacity(raw_entry.bits.len());
        for (bus_name, raw_seg) in raw_entry.bits {
            let baseaddr = parse_hex_addr(&raw_seg.baseaddr)?;
            bits.insert(
                bus_name,
                TileBitSegment {
                    baseaddr,
                    frames: raw_seg.frames,
                    offset: raw_seg.offset,
                    words: raw_seg.words,
                },
            );
        }
        grid.insert(
            tile_name,
            TileGridEntry {
                bits,
                grid_x: raw_entry.grid_x,
                grid_y: raw_entry.grid_y,
                tile_type: raw_entry.tile_type,
                sites: raw_entry.sites,
            },
        );
    }
    Ok(grid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_addr_with_prefix() {
        assert_eq!(parse_hex_addr("0x00020800").unwrap(), 0x00020800);
    }

    #[test]
    fn parse_hex_addr_uppercase_prefix() {
        assert_eq!(parse_hex_addr("0X00FF00FF").unwrap(), 0x00FF00FF);
    }

    #[test]
    fn parse_hex_addr_no_prefix() {
        assert_eq!(parse_hex_addr("DEADBEEF").unwrap(), 0xDEADBEEF);
    }

    #[test]
    fn parse_hex_addr_zero() {
        assert_eq!(parse_hex_addr("0x00000000").unwrap(), 0);
    }

    #[test]
    fn parse_hex_addr_invalid() {
        assert!(parse_hex_addr("0xGGGG").is_err());
    }

    #[test]
    fn parse_single_clb_tile() {
        let json = r#"{
            "CLBLL_L_X16Y149": {
                "bits": {
                    "CLB_IO_CLK": {
                        "baseaddr": "0x00020800",
                        "frames": 36,
                        "offset": 99,
                        "words": 2
                    }
                },
                "grid_x": 33,
                "grid_y": 1,
                "type": "CLBLL_L",
                "sites": {
                    "SLICE_X24Y149": "SLICEL",
                    "SLICE_X25Y149": "SLICEL"
                }
            }
        }"#;
        let grid = parse_tilegrid(json).unwrap();
        assert_eq!(grid.len(), 1);

        let entry = &grid["CLBLL_L_X16Y149"];
        assert_eq!(entry.grid_x, 33);
        assert_eq!(entry.grid_y, 1);
        assert_eq!(entry.tile_type, "CLBLL_L");
        assert_eq!(entry.sites.len(), 2);
        assert_eq!(entry.sites["SLICE_X24Y149"], "SLICEL");

        let seg = &entry.bits["CLB_IO_CLK"];
        assert_eq!(seg.baseaddr, 0x00020800);
        assert_eq!(seg.frames, 36);
        assert_eq!(seg.offset, 99);
        assert_eq!(seg.words, 2);
    }

    #[test]
    fn parse_tile_without_sites() {
        let json = r#"{
            "NULL_X0Y0": {
                "bits": {},
                "grid_x": 0,
                "grid_y": 0,
                "type": "NULL"
            }
        }"#;
        let grid = parse_tilegrid(json).unwrap();
        let entry = &grid["NULL_X0Y0"];
        assert!(entry.sites.is_empty());
        assert!(entry.bits.is_empty());
    }

    #[test]
    fn parse_int_tile() {
        let json = r#"{
            "INT_L_X10Y20": {
                "bits": {
                    "CLB_IO_CLK": {
                        "baseaddr": "0x00400000",
                        "frames": 26,
                        "offset": 50,
                        "words": 2
                    }
                },
                "grid_x": 21,
                "grid_y": 130,
                "type": "INT_L",
                "sites": {}
            }
        }"#;
        let grid = parse_tilegrid(json).unwrap();
        let entry = &grid["INT_L_X10Y20"];
        assert_eq!(entry.tile_type, "INT_L");
        assert!(entry.sites.is_empty());
        assert_eq!(entry.bits["CLB_IO_CLK"].baseaddr, 0x00400000);
    }

    #[test]
    fn parse_bram_tile() {
        let json = r#"{
            "BRAM_L_X6Y100": {
                "bits": {
                    "BLOCK_RAM": {
                        "baseaddr": "0x00800000",
                        "frames": 128,
                        "offset": 0,
                        "words": 10
                    }
                },
                "grid_x": 13,
                "grid_y": 50,
                "type": "BRAM_L",
                "sites": {
                    "RAMB36_X0Y20": "RAMB36E1",
                    "RAMB18_X0Y40": "RAMB18E1",
                    "RAMB18_X0Y41": "RAMB18E1"
                }
            }
        }"#;
        let grid = parse_tilegrid(json).unwrap();
        let entry = &grid["BRAM_L_X6Y100"];
        assert_eq!(entry.tile_type, "BRAM_L");
        assert_eq!(entry.sites.len(), 3);
        assert_eq!(entry.bits["BLOCK_RAM"].frames, 128);
    }

    #[test]
    fn parse_multiple_tiles() {
        let json = r#"{
            "CLBLL_L_X0Y0": {
                "bits": {},
                "grid_x": 0,
                "grid_y": 0,
                "type": "CLBLL_L",
                "sites": {}
            },
            "INT_L_X0Y0": {
                "bits": {},
                "grid_x": 1,
                "grid_y": 0,
                "type": "INT_L",
                "sites": {}
            },
            "LIOB33_X0Y0": {
                "bits": {},
                "grid_x": 2,
                "grid_y": 0,
                "type": "LIOB33",
                "sites": {}
            }
        }"#;
        let grid = parse_tilegrid(json).unwrap();
        assert_eq!(grid.len(), 3);
        assert_eq!(grid["CLBLL_L_X0Y0"].tile_type, "CLBLL_L");
        assert_eq!(grid["INT_L_X0Y0"].tile_type, "INT_L");
        assert_eq!(grid["LIOB33_X0Y0"].tile_type, "LIOB33");
    }

    #[test]
    fn parse_multiple_bit_segments() {
        let json = r#"{
            "HCLK_L_X0Y0": {
                "bits": {
                    "CLB_IO_CLK": {
                        "baseaddr": "0x00000000",
                        "frames": 36,
                        "offset": 0,
                        "words": 1
                    },
                    "BLOCK_RAM": {
                        "baseaddr": "0x00100000",
                        "frames": 128,
                        "offset": 0,
                        "words": 1
                    }
                },
                "grid_x": 0,
                "grid_y": 0,
                "type": "HCLK_L",
                "sites": {}
            }
        }"#;
        let grid = parse_tilegrid(json).unwrap();
        let entry = &grid["HCLK_L_X0Y0"];
        assert_eq!(entry.bits.len(), 2);
        assert_eq!(entry.bits["CLB_IO_CLK"].baseaddr, 0);
        assert_eq!(entry.bits["BLOCK_RAM"].baseaddr, 0x00100000);
    }

    #[test]
    fn parse_invalid_json() {
        let result = parse_tilegrid("not valid json");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("parse error"));
    }

    #[test]
    fn parse_invalid_hex_address() {
        let json = r#"{
            "BAD_X0Y0": {
                "bits": {
                    "CLB_IO_CLK": {
                        "baseaddr": "0xZZZZZZZZ",
                        "frames": 1,
                        "offset": 0,
                        "words": 1
                    }
                },
                "grid_x": 0,
                "grid_y": 0,
                "type": "BAD",
                "sites": {}
            }
        }"#;
        let result = parse_tilegrid(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid hex"));
    }

    #[test]
    fn tile_bit_segment_equality() {
        let a = TileBitSegment {
            baseaddr: 0x100,
            frames: 36,
            offset: 10,
            words: 2,
        };
        let b = TileBitSegment {
            baseaddr: 0x100,
            frames: 36,
            offset: 10,
            words: 2,
        };
        assert_eq!(a, b);
    }
}
