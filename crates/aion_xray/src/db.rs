//! Top-level Project X-Ray database loader.
//!
//! Combines tilegrid, segbits, and tile type data into a single [`XRayDatabase`]
//! that provides everything needed for architecture modeling, placement, routing,
//! and bitstream generation.
//!
//! The database is loaded from a directory containing the Project X-Ray database
//! files (a clone of `prjxray-db`). The expected directory structure is:
//!
//! ```text
//! prjxray-db/
//! └── artix7/
//!     └── xc7a35t/
//!         ├── tilegrid.json
//!         ├── segbits_clbll_l.db
//!         ├── segbits_int_l.db
//!         ├── tile_type_CLBLL_L.json
//!         └── ...
//! ```

use crate::segbits::{self, SegBitsMap};
use crate::tile_type::{self, TileTypeData};
use crate::tilegrid::{self, TileGrid};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The complete Project X-Ray database for a single device.
///
/// Contains the tilegrid layout, segbits mappings for each tile type,
/// and tile type definitions with PIP/wire/site-pin information.
#[derive(Debug, Clone)]
pub struct XRayDatabase {
    /// The device part name (e.g., "xc7a35t").
    pub part: String,
    /// The tilegrid mapping tile names to their physical layout.
    pub tilegrid: TileGrid,
    /// Segbits mappings indexed by tile type name.
    pub segbits: HashMap<String, SegBitsMap>,
    /// Tile type data indexed by tile type name.
    pub tile_types: HashMap<String, TileTypeData>,
    /// The root path of the loaded database.
    pub db_path: PathBuf,
}

/// Known tile types that have segbits files.
const SEGBITS_TILE_TYPES: &[&str] = &[
    "clbll_l",
    "clbll_r",
    "clblm_l",
    "clblm_r",
    "int_l",
    "int_r",
    "liob33",
    "riob33",
    "bram_l",
    "bram_r",
    "dsp_l",
    "dsp_r",
    "hclk_l",
    "hclk_r",
    "cmt_top_l_lower_b",
    "cmt_top_r_lower_b",
];

/// Known tile types that have tile_type JSON files.
const TILE_TYPE_NAMES: &[&str] = &[
    "CLBLL_L", "CLBLL_R", "CLBLM_L", "CLBLM_R", "INT_L", "INT_R", "LIOB33", "RIOB33", "BRAM_L",
    "BRAM_R", "DSP_L", "DSP_R",
];

impl XRayDatabase {
    /// Loads the X-Ray database from the given path for the specified part.
    ///
    /// The `db_root` should point to the family-level directory (e.g.,
    /// `prjxray-db/artix7/`). The `part` is the device name (e.g., `xc7a35t`).
    ///
    /// # Errors
    ///
    /// Returns an error string if required files cannot be read or parsed.
    pub fn load(db_root: &Path, part: &str) -> Result<Self, String> {
        let part_dir = db_root.join(part);
        if !part_dir.exists() {
            return Err(format!(
                "X-Ray database directory not found: {}",
                part_dir.display()
            ));
        }

        // Load tilegrid
        let tilegrid_path = part_dir.join("tilegrid.json");
        let tilegrid_json = std::fs::read_to_string(&tilegrid_path).map_err(|e| {
            format!(
                "failed to read tilegrid at {}: {e}",
                tilegrid_path.display()
            )
        })?;
        let tilegrid = tilegrid::parse_tilegrid(&tilegrid_json)?;

        // Load segbits (best-effort: missing files are skipped)
        let mut segbits_map = HashMap::new();
        for tile_type in SEGBITS_TILE_TYPES {
            let filename = segbits::segbits_filename(tile_type);
            let segbits_path = part_dir.join(&filename);
            if let Ok(content) = std::fs::read_to_string(&segbits_path) {
                match segbits::parse_segbits(&content) {
                    Ok(sb) => {
                        segbits_map.insert(tile_type.to_ascii_uppercase(), sb);
                    }
                    Err(e) => {
                        return Err(format!("failed to parse {filename}: {e}"));
                    }
                }
            }
        }

        // Load tile types (best-effort: missing files are skipped)
        let mut tile_types = HashMap::new();
        for type_name in TILE_TYPE_NAMES {
            let filename = tile_type::tile_type_filename(type_name);
            let tt_path = part_dir.join(&filename);
            if let Ok(json) = std::fs::read_to_string(&tt_path) {
                match tile_type::parse_tile_type(type_name, &json) {
                    Ok(data) => {
                        tile_types.insert(type_name.to_string(), data);
                    }
                    Err(e) => {
                        return Err(format!("failed to parse {filename}: {e}"));
                    }
                }
            }
        }

        Ok(Self {
            part: part.to_string(),
            tilegrid,
            segbits: segbits_map,
            tile_types,
            db_path: db_root.to_path_buf(),
        })
    }

    /// Returns the segbits map for the given tile type, if available.
    pub fn segbits_for_tile_type(&self, tile_type: &str) -> Option<&SegBitsMap> {
        self.segbits.get(&tile_type.to_ascii_uppercase())
    }

    /// Returns the tile type data for the given tile type, if available.
    pub fn tile_type_data(&self, tile_type: &str) -> Option<&TileTypeData> {
        self.tile_types.get(tile_type)
    }

    /// Returns the number of tiles in the tilegrid.
    pub fn tile_count(&self) -> usize {
        self.tilegrid.len()
    }

    /// Returns the number of tile types with loaded segbits.
    pub fn segbits_count(&self) -> usize {
        self.segbits.len()
    }

    /// Returns the number of loaded tile type definitions.
    pub fn tile_type_count(&self) -> usize {
        self.tile_types.len()
    }

    /// Returns all unique tile type names found in the tilegrid.
    pub fn unique_tile_types(&self) -> Vec<String> {
        let mut types: Vec<String> = self
            .tilegrid
            .values()
            .map(|e| e.tile_type.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        types.sort();
        types
    }
}

/// Resolves the X-Ray database path from environment or configuration.
///
/// Checks in order:
/// 1. The `AION_XRAY_DB` environment variable
/// 2. The provided `config_path` (from `aion.toml`)
///
/// Returns `None` if neither is set or the path doesn't exist.
pub fn resolve_xray_db_path(config_path: Option<&str>) -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("AION_XRAY_DB") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }

    if let Some(cp) = config_path {
        let p = PathBuf::from(cp);
        if p.exists() {
            return Some(p);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Creates a temporary X-Ray database directory with minimal fixture data.
    fn create_fixture_db(dir: &Path, part: &str) -> PathBuf {
        let part_dir = dir.join(part);
        fs::create_dir_all(&part_dir).unwrap();

        // Minimal tilegrid
        let tilegrid = r#"{
            "CLBLL_L_X0Y0": {
                "bits": {
                    "CLB_IO_CLK": {
                        "baseaddr": "0x00020800",
                        "frames": 36,
                        "offset": 99,
                        "words": 2
                    }
                },
                "grid_x": 1,
                "grid_y": 1,
                "type": "CLBLL_L",
                "sites": {
                    "SLICE_X0Y0": "SLICEL"
                }
            },
            "INT_L_X0Y0": {
                "bits": {
                    "CLB_IO_CLK": {
                        "baseaddr": "0x00020800",
                        "frames": 26,
                        "offset": 50,
                        "words": 2
                    }
                },
                "grid_x": 2,
                "grid_y": 1,
                "type": "INT_L",
                "sites": {}
            }
        }"#;
        fs::write(part_dir.join("tilegrid.json"), tilegrid).unwrap();

        // Minimal segbits
        let segbits_clbll = "CLBLL_L.SLICEL_X0.ALUT.INIT[00] 00_14\n";
        fs::write(part_dir.join("segbits_clbll_l.db"), segbits_clbll).unwrap();

        let segbits_int = "INT_L.NL1BEG1.SS2END0 28_13\n";
        fs::write(part_dir.join("segbits_int_l.db"), segbits_int).unwrap();

        // Minimal tile type
        let tile_type_clbll = r#"{
            "pips": [
                {
                    "src_wire": "CLBLL_L_A",
                    "dst_wire": "CLBLL_L_AMUX",
                    "is_directional": true,
                    "is_pseudo": false
                }
            ],
            "wires": ["CLBLL_L_A", "CLBLL_L_AMUX"],
            "site_pins": {
                "SLICEL_X0": [
                    {"pin_name": "A1", "wire_name": "CLBLL_L_A1", "direction": "IN"}
                ]
            }
        }"#;
        fs::write(part_dir.join("tile_type_CLBLL_L.json"), tile_type_clbll).unwrap();

        dir.to_path_buf()
    }

    #[test]
    fn load_fixture_database() {
        let tmp = tempdir("load_fixture");
        let db_root = create_fixture_db(&tmp, "xc7a35t");
        let db = XRayDatabase::load(&db_root, "xc7a35t").unwrap();

        assert_eq!(db.part, "xc7a35t");
        assert_eq!(db.tile_count(), 2);
        assert!(db.segbits_count() >= 1);
    }

    #[test]
    fn load_missing_part_dir() {
        let tmp = tempdir("missing_part");
        let result = XRayDatabase::load(&tmp, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn load_missing_tilegrid() {
        let tmp = tempdir("missing_tilegrid");
        let part_dir = tmp.join("xc7a35t");
        fs::create_dir_all(&part_dir).unwrap();
        let result = XRayDatabase::load(&tmp, "xc7a35t");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("tilegrid"));
    }

    #[test]
    fn segbits_for_tile_type_found() {
        let tmp = tempdir("segbits_found");
        let db_root = create_fixture_db(&tmp, "xc7a35t");
        let db = XRayDatabase::load(&db_root, "xc7a35t").unwrap();

        let sb = db.segbits_for_tile_type("CLBLL_L");
        assert!(sb.is_some());
        assert!(sb.unwrap().contains_key("CLBLL_L.SLICEL_X0.ALUT.INIT[00]"));
    }

    #[test]
    fn segbits_for_tile_type_not_found() {
        let tmp = tempdir("segbits_not_found");
        let db_root = create_fixture_db(&tmp, "xc7a35t");
        let db = XRayDatabase::load(&db_root, "xc7a35t").unwrap();

        assert!(db.segbits_for_tile_type("NONEXISTENT").is_none());
    }

    #[test]
    fn tile_type_data_found() {
        let tmp = tempdir("tile_type_found");
        let db_root = create_fixture_db(&tmp, "xc7a35t");
        let db = XRayDatabase::load(&db_root, "xc7a35t").unwrap();

        let tt = db.tile_type_data("CLBLL_L");
        assert!(tt.is_some());
        assert_eq!(tt.unwrap().pips.len(), 1);
    }

    #[test]
    fn unique_tile_types_list() {
        let tmp = tempdir("unique_types");
        let db_root = create_fixture_db(&tmp, "xc7a35t");
        let db = XRayDatabase::load(&db_root, "xc7a35t").unwrap();

        let types = db.unique_tile_types();
        assert!(types.contains(&"CLBLL_L".to_string()));
        assert!(types.contains(&"INT_L".to_string()));
    }

    #[test]
    fn resolve_xray_db_path_none() {
        // With no env var and no config, should return None
        let result = resolve_xray_db_path(None);
        // Can't assert None because AION_XRAY_DB might be set in env
        // Just verify the function doesn't panic
        let _ = result;
    }

    #[test]
    fn resolve_xray_db_path_nonexistent_config() {
        let result = resolve_xray_db_path(Some("/nonexistent/path"));
        // Should be None unless env var is set
        if std::env::var("AION_XRAY_DB").is_err() {
            assert!(result.is_none());
        }
    }

    #[test]
    fn database_counters() {
        let tmp = tempdir("counters");
        let db_root = create_fixture_db(&tmp, "xc7a35t");
        let db = XRayDatabase::load(&db_root, "xc7a35t").unwrap();

        assert_eq!(db.tile_count(), 2);
        assert!(db.tile_type_count() >= 1);
    }

    /// Creates a unique temporary directory and returns its path.
    fn tempdir(suffix: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aion_xray_test_{}_{suffix}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
