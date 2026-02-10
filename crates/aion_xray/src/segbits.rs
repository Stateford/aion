//! Parser for Project X-Ray `segbits_*.db` files.
//!
//! Segbits files map tile features (like LUT initialization bits, PIP enables,
//! and FF configuration) to physical bit positions within configuration frames.
//! Each line maps a feature path to one or more frame-relative bit positions.
//!
//! # Format
//!
//! ```text
//! CLBLL_L.SLICEL_X0.ALUT.INIT[00] 00_14 00_15
//! CLBLL_L.SLICEL_X0.AFF.ZRST !01_42
//! INT_L.NL1BEG1.SS2END0 28_13
//! ```
//!
//! Each bit entry has the format `[!]frame_bit` where `frame` is the frame
//! offset relative to the tile's base address, and `bit` is the bit position
//! within the tile's word range. The `!` prefix indicates an inverted bit
//! (the bit must be 0 for the feature to be enabled).

use std::collections::HashMap;

/// A single bit position within a segbits entry.
///
/// Identifies a specific bit relative to the tile's base frame address,
/// with an optional inversion flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegBitEntry {
    /// Frame offset relative to the tile's base address.
    pub frame_offset: u32,
    /// Bit position within the tile's word range in that frame.
    pub bit_position: u32,
    /// If true, the bit must be 0 for the feature to be active (inverted logic).
    pub inverted: bool,
}

/// A mapping from feature names to their configuration bit positions.
///
/// Feature names follow the pattern `TILE_TYPE.SITE.FEATURE[INDEX]` or
/// `TILE_TYPE.DST_WIRE.SRC_WIRE` for PIPs.
pub type SegBitsMap = HashMap<String, Vec<SegBitEntry>>;

/// Parses a single bit specifier like "00_14" or "!01_42".
///
/// Returns a [`SegBitEntry`] with the frame offset, bit position, and
/// inversion flag.
///
/// # Errors
///
/// Returns an error string if the format is invalid.
pub fn parse_bit_spec(spec: &str) -> Result<SegBitEntry, String> {
    let (inverted, rest) = if let Some(s) = spec.strip_prefix('!') {
        (true, s)
    } else {
        (false, spec)
    };

    let parts: Vec<&str> = rest.split('_').collect();
    if parts.len() != 2 {
        return Err(format!(
            "invalid bit spec '{spec}': expected format 'frame_bit' or '!frame_bit'"
        ));
    }

    let frame_offset = parts[0]
        .parse::<u32>()
        .map_err(|e| format!("invalid frame offset in '{spec}': {e}"))?;
    let bit_position = parts[1]
        .parse::<u32>()
        .map_err(|e| format!("invalid bit position in '{spec}': {e}"))?;

    Ok(SegBitEntry {
        frame_offset,
        bit_position,
        inverted,
    })
}

/// Parses a segbits database string into a [`SegBitsMap`].
///
/// Each line in the input maps a feature name to one or more bit specifiers.
/// Empty lines and lines starting with `#` are skipped.
///
/// # Errors
///
/// Returns an error string if any line has an invalid format.
pub fn parse_segbits(content: &str) -> Result<SegBitsMap, String> {
    let mut map = HashMap::new();

    for (line_no, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.split_whitespace();
        let feature = parts
            .next()
            .ok_or_else(|| format!("line {}: empty feature name", line_no + 1))?;

        let mut bits = Vec::new();
        for bit_spec in parts {
            let entry =
                parse_bit_spec(bit_spec).map_err(|e| format!("line {}: {e}", line_no + 1))?;
            bits.push(entry);
        }

        if bits.is_empty() {
            return Err(format!(
                "line {}: feature '{feature}' has no bit specifiers",
                line_no + 1
            ));
        }

        map.insert(feature.to_string(), bits);
    }

    Ok(map)
}

/// Returns the feature prefix for a given tile type.
///
/// For example, segbits for `CLBLL_L` tiles have features prefixed with
/// `CLBLL_L.`, and INT tiles use `INT_L.` or `INT_R.`.
pub fn segbits_filename(tile_type: &str) -> String {
    format!("segbits_{}.db", tile_type.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_bit_spec() {
        let entry = parse_bit_spec("00_14").unwrap();
        assert_eq!(entry.frame_offset, 0);
        assert_eq!(entry.bit_position, 14);
        assert!(!entry.inverted);
    }

    #[test]
    fn parse_inverted_bit_spec() {
        let entry = parse_bit_spec("!01_42").unwrap();
        assert_eq!(entry.frame_offset, 1);
        assert_eq!(entry.bit_position, 42);
        assert!(entry.inverted);
    }

    #[test]
    fn parse_large_frame_offset() {
        let entry = parse_bit_spec("35_127").unwrap();
        assert_eq!(entry.frame_offset, 35);
        assert_eq!(entry.bit_position, 127);
    }

    #[test]
    fn parse_invalid_bit_spec_no_underscore() {
        assert!(parse_bit_spec("0014").is_err());
    }

    #[test]
    fn parse_invalid_bit_spec_too_many_parts() {
        assert!(parse_bit_spec("00_14_99").is_err());
    }

    #[test]
    fn parse_invalid_bit_spec_non_numeric() {
        assert!(parse_bit_spec("xx_yy").is_err());
    }

    #[test]
    fn parse_lut_init_segbits() {
        let content = "\
CLBLL_L.SLICEL_X0.ALUT.INIT[00] 00_14 00_15
CLBLL_L.SLICEL_X0.ALUT.INIT[01] 00_16
";
        let map = parse_segbits(content).unwrap();
        assert_eq!(map.len(), 2);

        let init0 = &map["CLBLL_L.SLICEL_X0.ALUT.INIT[00]"];
        assert_eq!(init0.len(), 2);
        assert_eq!(init0[0].frame_offset, 0);
        assert_eq!(init0[0].bit_position, 14);
        assert_eq!(init0[1].frame_offset, 0);
        assert_eq!(init0[1].bit_position, 15);

        let init1 = &map["CLBLL_L.SLICEL_X0.ALUT.INIT[01]"];
        assert_eq!(init1.len(), 1);
        assert_eq!(init1[0].bit_position, 16);
    }

    #[test]
    fn parse_ff_segbits_with_inversion() {
        let content = "CLBLL_L.SLICEL_X0.AFF.ZRST !01_42\n";
        let map = parse_segbits(content).unwrap();
        let bits = &map["CLBLL_L.SLICEL_X0.AFF.ZRST"];
        assert_eq!(bits.len(), 1);
        assert!(bits[0].inverted);
        assert_eq!(bits[0].frame_offset, 1);
        assert_eq!(bits[0].bit_position, 42);
    }

    #[test]
    fn parse_pip_segbits() {
        let content = "INT_L.NL1BEG1.SS2END0 28_13\n";
        let map = parse_segbits(content).unwrap();
        let bits = &map["INT_L.NL1BEG1.SS2END0"];
        assert_eq!(bits.len(), 1);
        assert_eq!(bits[0].frame_offset, 28);
        assert_eq!(bits[0].bit_position, 13);
    }

    #[test]
    fn parse_segbits_skips_empty_and_comments() {
        let content = "\
# This is a comment
CLBLL_L.SLICEL_X0.ALUT.INIT[00] 00_14

# Another comment
CLBLL_L.SLICEL_X0.ALUT.INIT[01] 00_16
";
        let map = parse_segbits(content).unwrap();
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_segbits_multi_bit_pip() {
        let content = "INT_L.EE2BEG0.NN6END0 05_20 05_21 !05_22\n";
        let map = parse_segbits(content).unwrap();
        let bits = &map["INT_L.EE2BEG0.NN6END0"];
        assert_eq!(bits.len(), 3);
        assert!(!bits[0].inverted);
        assert!(!bits[1].inverted);
        assert!(bits[2].inverted);
    }

    #[test]
    fn parse_segbits_empty_input() {
        let map = parse_segbits("").unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn parse_segbits_only_comments() {
        let map = parse_segbits("# comment\n# another\n").unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn parse_segbits_no_bits_is_error() {
        let result = parse_segbits("CLBLL_L.FEATURE\n");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no bit specifiers"));
    }

    #[test]
    fn segbits_filename_lowercase() {
        assert_eq!(segbits_filename("CLBLL_L"), "segbits_clbll_l.db");
        assert_eq!(segbits_filename("INT_R"), "segbits_int_r.db");
    }

    #[test]
    fn seg_bit_entry_equality() {
        let a = SegBitEntry {
            frame_offset: 5,
            bit_position: 10,
            inverted: false,
        };
        let b = SegBitEntry {
            frame_offset: 5,
            bit_position: 10,
            inverted: false,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn seg_bit_entry_inequality_inverted() {
        let a = SegBitEntry {
            frame_offset: 5,
            bit_position: 10,
            inverted: false,
        };
        let b = SegBitEntry {
            frame_offset: 5,
            bit_position: 10,
            inverted: true,
        };
        assert_ne!(a, b);
    }
}
