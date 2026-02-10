//! Parser for Project X-Ray `tile_type_*.json` files.
//!
//! Tile type files describe the internal structure of each tile type, including
//! the programmable interconnect points (PIPs), wires, and site pin-to-wire
//! mappings. This data is used to build routing graphs and map placed cells
//! to the routing fabric.

use serde::Deserialize;
use std::collections::HashMap;

/// A programmable interconnect point (PIP) within a tile.
///
/// PIPs are switches that connect two wires within a tile. Enabling a PIP
/// routes a signal from the source wire to the destination wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TilePip {
    /// The source wire name within the tile.
    pub src_wire: String,
    /// The destination wire name within the tile.
    pub dst_wire: String,
    /// Whether this PIP is bidirectional.
    pub is_bidi: bool,
    /// Whether this PIP is a route-through (passes through a site).
    pub is_routethrough: bool,
}

/// A site pin connecting a site to the tile's routing wires.
///
/// Site pins are the interface between placement sites (where cells live)
/// and the routing fabric (where signals travel between cells).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePin {
    /// The pin name within the site (e.g., "A1", "COUT", "D").
    pub pin_name: String,
    /// The wire name in the tile that this pin connects to.
    pub wire_name: String,
    /// The direction of the pin from the site's perspective.
    pub direction: SitePinDirection,
}

/// Direction of a site pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SitePinDirection {
    /// Signal flows into the site.
    Input,
    /// Signal flows out of the site.
    Output,
    /// Signal can flow in either direction.
    Bidirectional,
}

/// Parsed tile type data containing PIPs and site pin mappings.
#[derive(Debug, Clone)]
pub struct TileTypeData {
    /// The tile type name (e.g., "CLBLL_L", "INT_L").
    pub name: String,
    /// All PIPs within this tile type.
    pub pips: Vec<TilePip>,
    /// All wires defined within this tile type.
    pub wires: Vec<String>,
    /// Site pin-to-wire mappings, indexed by site name.
    pub site_pins: HashMap<String, Vec<SitePin>>,
}

/// Raw JSON structure for PIP deserialization.
#[derive(Deserialize)]
struct RawPip {
    src_wire: String,
    dst_wire: String,
    #[serde(default)]
    is_directional: bool,
    #[serde(default)]
    is_pseudo: bool,
}

/// Raw JSON structure for site pin deserialization.
#[derive(Deserialize)]
struct RawSitePin {
    pin_name: String,
    wire_name: String,
    direction: String,
}

/// Raw JSON structure for tile type deserialization.
#[derive(Deserialize)]
struct RawTileType {
    #[serde(default)]
    pips: Vec<RawPip>,
    #[serde(default)]
    wires: Vec<String>,
    #[serde(default)]
    site_pins: HashMap<String, Vec<RawSitePin>>,
}

/// Parses a tile type JSON string into [`TileTypeData`].
///
/// The input should be the contents of a `tile_type_*.json` file from the
/// Project X-Ray database.
///
/// # Errors
///
/// Returns an error string if the JSON is malformed or contains invalid
/// direction values.
pub fn parse_tile_type(name: &str, json: &str) -> Result<TileTypeData, String> {
    let raw: RawTileType =
        serde_json::from_str(json).map_err(|e| format!("tile_type JSON parse error: {e}"))?;

    let pips: Vec<TilePip> = raw
        .pips
        .into_iter()
        .map(|p| TilePip {
            src_wire: p.src_wire,
            dst_wire: p.dst_wire,
            is_bidi: !p.is_directional,
            is_routethrough: p.is_pseudo,
        })
        .collect();

    let mut site_pins = HashMap::new();
    for (site_name, raw_pins) in raw.site_pins {
        let mut pins = Vec::with_capacity(raw_pins.len());
        for rp in raw_pins {
            let direction = match rp.direction.as_str() {
                "IN" | "input" => SitePinDirection::Input,
                "OUT" | "output" => SitePinDirection::Output,
                "INOUT" | "bidir" | "bidirectional" => SitePinDirection::Bidirectional,
                other => {
                    return Err(format!(
                        "unknown site pin direction '{other}' for pin '{}'",
                        rp.pin_name
                    ))
                }
            };
            pins.push(SitePin {
                pin_name: rp.pin_name,
                wire_name: rp.wire_name,
                direction,
            });
        }
        site_pins.insert(site_name, pins);
    }

    Ok(TileTypeData {
        name: name.to_string(),
        pips,
        wires: raw.wires,
        site_pins,
    })
}

/// Returns the expected filename for a tile type JSON file.
pub fn tile_type_filename(tile_type: &str) -> String {
    format!("tile_type_{}.json", tile_type.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_tile_type() {
        let json = r#"{
            "pips": [],
            "wires": [],
            "site_pins": {}
        }"#;
        let data = parse_tile_type("EMPTY", json).unwrap();
        assert_eq!(data.name, "EMPTY");
        assert!(data.pips.is_empty());
        assert!(data.wires.is_empty());
        assert!(data.site_pins.is_empty());
    }

    #[test]
    fn parse_tile_type_defaults() {
        let json = "{}";
        let data = parse_tile_type("EMPTY", json).unwrap();
        assert!(data.pips.is_empty());
        assert!(data.wires.is_empty());
    }

    #[test]
    fn parse_tile_type_with_pips() {
        let json = r#"{
            "pips": [
                {
                    "src_wire": "NL1BEG1",
                    "dst_wire": "NL1END1",
                    "is_directional": true,
                    "is_pseudo": false
                },
                {
                    "src_wire": "WL1BEG0",
                    "dst_wire": "WL1END0",
                    "is_directional": false,
                    "is_pseudo": false
                }
            ],
            "wires": ["NL1BEG1", "NL1END1", "WL1BEG0", "WL1END0"]
        }"#;
        let data = parse_tile_type("INT_L", json).unwrap();
        assert_eq!(data.pips.len(), 2);

        assert_eq!(data.pips[0].src_wire, "NL1BEG1");
        assert_eq!(data.pips[0].dst_wire, "NL1END1");
        assert!(!data.pips[0].is_bidi);
        assert!(!data.pips[0].is_routethrough);

        assert!(data.pips[1].is_bidi);

        assert_eq!(data.wires.len(), 4);
    }

    #[test]
    fn parse_tile_type_with_routethrough() {
        let json = r#"{
            "pips": [
                {
                    "src_wire": "CLBLL_L_A",
                    "dst_wire": "CLBLL_L_AMUX",
                    "is_directional": true,
                    "is_pseudo": true
                }
            ]
        }"#;
        let data = parse_tile_type("CLBLL_L", json).unwrap();
        assert!(data.pips[0].is_routethrough);
    }

    #[test]
    fn parse_tile_type_with_site_pins() {
        let json = r#"{
            "site_pins": {
                "SLICEL_X0": [
                    {"pin_name": "A1", "wire_name": "CLBLL_L_A1", "direction": "IN"},
                    {"pin_name": "AMUX", "wire_name": "CLBLL_L_AMUX", "direction": "OUT"},
                    {"pin_name": "D_I", "wire_name": "CLBLL_L_DI", "direction": "INOUT"}
                ]
            }
        }"#;
        let data = parse_tile_type("CLBLL_L", json).unwrap();
        let pins = &data.site_pins["SLICEL_X0"];
        assert_eq!(pins.len(), 3);

        assert_eq!(pins[0].pin_name, "A1");
        assert_eq!(pins[0].wire_name, "CLBLL_L_A1");
        assert_eq!(pins[0].direction, SitePinDirection::Input);

        assert_eq!(pins[1].pin_name, "AMUX");
        assert_eq!(pins[1].direction, SitePinDirection::Output);

        assert_eq!(pins[2].direction, SitePinDirection::Bidirectional);
    }

    #[test]
    fn parse_tile_type_alternative_directions() {
        let json = r#"{
            "site_pins": {
                "SITE0": [
                    {"pin_name": "P0", "wire_name": "W0", "direction": "input"},
                    {"pin_name": "P1", "wire_name": "W1", "direction": "output"},
                    {"pin_name": "P2", "wire_name": "W2", "direction": "bidir"}
                ]
            }
        }"#;
        let data = parse_tile_type("TEST", json).unwrap();
        let pins = &data.site_pins["SITE0"];
        assert_eq!(pins[0].direction, SitePinDirection::Input);
        assert_eq!(pins[1].direction, SitePinDirection::Output);
        assert_eq!(pins[2].direction, SitePinDirection::Bidirectional);
    }

    #[test]
    fn parse_tile_type_unknown_direction() {
        let json = r#"{
            "site_pins": {
                "SITE0": [
                    {"pin_name": "P0", "wire_name": "W0", "direction": "UNKNOWN"}
                ]
            }
        }"#;
        let result = parse_tile_type("TEST", json);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown site pin direction"));
    }

    #[test]
    fn parse_tile_type_invalid_json() {
        assert!(parse_tile_type("BAD", "not json").is_err());
    }

    #[test]
    fn parse_tile_type_multiple_sites() {
        let json = r#"{
            "site_pins": {
                "SLICEL_X0": [
                    {"pin_name": "A1", "wire_name": "W_A1", "direction": "IN"}
                ],
                "SLICEL_X1": [
                    {"pin_name": "B1", "wire_name": "W_B1", "direction": "IN"}
                ]
            }
        }"#;
        let data = parse_tile_type("CLBLL_L", json).unwrap();
        assert_eq!(data.site_pins.len(), 2);
        assert!(data.site_pins.contains_key("SLICEL_X0"));
        assert!(data.site_pins.contains_key("SLICEL_X1"));
    }

    #[test]
    fn tile_pip_equality() {
        let a = TilePip {
            src_wire: "A".to_string(),
            dst_wire: "B".to_string(),
            is_bidi: false,
            is_routethrough: false,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn tile_type_filename_format() {
        assert_eq!(tile_type_filename("int_l"), "tile_type_INT_L.json");
        assert_eq!(tile_type_filename("CLBLL_L"), "tile_type_CLBLL_L.json");
    }

    #[test]
    fn site_pin_direction_variants() {
        assert_ne!(SitePinDirection::Input, SitePinDirection::Output);
        assert_ne!(SitePinDirection::Input, SitePinDirection::Bidirectional);
        assert_ne!(SitePinDirection::Output, SitePinDirection::Bidirectional);
    }
}
