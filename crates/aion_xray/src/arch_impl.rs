//! Architecture trait implementation backed by Project X-Ray data.
//!
//! Provides a real [`Architecture`] implementation for Xilinx Artix-7 devices
//! using tile grids, sites, and BELs populated from the X-Ray database rather
//! than hardcoded resource counts.

use crate::db::XRayDatabase;
use aion_arch::ids::{BelId, SiteId};
use aion_arch::tech_map::TechMapper;
use aion_arch::types::{
    Bel, BelType, Delay, ResourceUsage, RoutingGraph, Site, SiteType, Tile, TileType,
};
use aion_arch::xilinx::artix7::{Artix7, Artix7Mapper};
use aion_arch::Architecture;
use std::collections::HashMap;

/// Map from X-Ray tile type string to Aion [`TileType`].
fn classify_tile_type(xray_type: &str) -> TileType {
    match xray_type {
        "CLBLL_L" | "CLBLL_R" | "CLBLM_L" | "CLBLM_R" => TileType::Logic,
        "LIOB33" | "RIOB33" | "LIOB33_SING" | "RIOB33_SING" => TileType::Io,
        "BRAM_L" | "BRAM_R" => TileType::Bram,
        "DSP_L" | "DSP_R" => TileType::Dsp,
        "CMT_TOP_L_LOWER_B" | "CMT_TOP_R_LOWER_B" | "CMT_TOP_L_UPPER_T" | "CMT_TOP_R_UPPER_T" => {
            TileType::Clock
        }
        "INT_L" | "INT_R" => TileType::Interconnect,
        "HCLK_L" | "HCLK_R" | "HCLK_CLB" | "HCLK_IOI3" | "HCLK_BRAM" => TileType::Interconnect,
        _ => TileType::Empty,
    }
}

/// Map from X-Ray site type string to Aion [`SiteType`].
fn classify_site_type(xray_site_type: &str) -> Option<SiteType> {
    match xray_site_type {
        "SLICEL" | "SLICEM" => Some(SiteType::LutFf),
        "IOB33M" | "IOB33S" | "IOB33" => Some(SiteType::IoPad),
        "RAMB36E1" | "RAMB18E1" | "FIFO36E1" => Some(SiteType::BramSite),
        "DSP48E1" => Some(SiteType::DspSite),
        "MMCME2_ADV" | "PLLE2_ADV" => Some(SiteType::Pll),
        _ => None,
    }
}

/// Create BELs for a SLICEL/SLICEM site.
fn make_slice_bels(bel_id_base: u32) -> Vec<Bel> {
    let mut bels = Vec::with_capacity(12);
    let mut id = bel_id_base;

    // 4 LUTs (A through D)
    for letter in &['A', 'B', 'C', 'D'] {
        bels.push(Bel {
            id: BelId::from_raw(id),
            name: format!("{letter}6LUT"),
            bel_type: BelType::Lut,
        });
        id += 1;
    }

    // 8 FFs (AFF, AFF2, BFF, BFF2, ...)
    for letter in &['A', 'B', 'C', 'D'] {
        bels.push(Bel {
            id: BelId::from_raw(id),
            name: format!("{letter}FF"),
            bel_type: BelType::Ff,
        });
        id += 1;
        bels.push(Bel {
            id: BelId::from_raw(id),
            name: format!("{letter}FF2"),
            bel_type: BelType::Ff,
        });
        id += 1;
    }

    bels
}

/// Create BELs for an I/O pad site.
fn make_iobuf_bels(bel_id_base: u32) -> Vec<Bel> {
    vec![Bel {
        id: BelId::from_raw(bel_id_base),
        name: "IOB".to_string(),
        bel_type: BelType::IoBuf,
    }]
}

/// Create BELs for a BRAM site.
fn make_bram_bels(bel_id_base: u32) -> Vec<Bel> {
    vec![Bel {
        id: BelId::from_raw(bel_id_base),
        name: "RAMB36E1".to_string(),
        bel_type: BelType::BramPrimitive,
    }]
}

/// Create BELs for a DSP site.
fn make_dsp_bels(bel_id_base: u32) -> Vec<Bel> {
    vec![Bel {
        id: BelId::from_raw(bel_id_base),
        name: "DSP48E1".to_string(),
        bel_type: BelType::DspPrimitive,
    }]
}

/// Create BELs for a PLL/MMCM site.
fn make_pll_bels(bel_id_base: u32) -> Vec<Bel> {
    vec![Bel {
        id: BelId::from_raw(bel_id_base),
        name: "MMCME2_ADV".to_string(),
        bel_type: BelType::PllPrimitive,
    }]
}

/// Architecture model for Xilinx Artix-7 backed by real Project X-Ray data.
///
/// Delegates resource counts and tech mapping to the base [`Artix7`] model,
/// but provides real tile grid, site, and BEL data from the X-Ray database.
#[derive(Debug)]
pub struct Artix7XRay {
    /// Base Artix-7 model for resource counts and tech mapping.
    base: Artix7,
    /// All tiles in the device, indexed sequentially.
    tiles: Vec<Tile>,
    /// Grid lookup: `tile_grid[col][row]` → index into `tiles`.
    tile_grid: Vec<Vec<Option<usize>>>,
    /// All sites in the device.
    sites: Vec<Site>,
    /// Sites grouped by type for fast lookup.
    sites_by_type: HashMap<SiteType, Vec<SiteId>>,
    /// Grid dimensions (columns, rows).
    grid_dims: (u32, u32),
    /// The loaded X-Ray database.
    db: XRayDatabase,
}

impl Artix7XRay {
    /// Creates an Artix-7 architecture from an X-Ray database.
    ///
    /// Builds the tile grid, creates sites with BELs, and indexes everything
    /// for fast lookups by the placement and routing engines.
    pub fn new(base: Artix7, db: XRayDatabase) -> Self {
        let mut tiles = Vec::new();
        let mut sites = Vec::new();
        let mut sites_by_type: HashMap<SiteType, Vec<SiteId>> = HashMap::new();
        let mut next_bel_id: u32 = 0;

        // Determine grid dimensions
        let max_x = db.tilegrid.values().map(|e| e.grid_x).max().unwrap_or(0);
        let max_y = db.tilegrid.values().map(|e| e.grid_y).max().unwrap_or(0);
        let cols = max_x + 1;
        let rows = max_y + 1;

        // Initialize grid
        let mut tile_grid = vec![vec![None; rows as usize]; cols as usize];

        // Sort tile names for deterministic order
        let mut tile_names: Vec<&String> = db.tilegrid.keys().collect();
        tile_names.sort();

        for tile_name in tile_names {
            let entry = &db.tilegrid[tile_name];
            let tile_type = classify_tile_type(&entry.tile_type);

            // Create sites for this tile
            let mut tile_site_ids = Vec::new();

            // Sort site names for deterministic order
            let mut site_names: Vec<(&String, &String)> = entry.sites.iter().collect();
            site_names.sort_by_key(|(name, _)| name.as_str());

            for (site_name, site_type_str) in site_names {
                if let Some(site_type) = classify_site_type(site_type_str) {
                    let site_id = SiteId::from_raw(sites.len() as u32);

                    let bels = match site_type {
                        SiteType::LutFf => {
                            let b = make_slice_bels(next_bel_id);
                            next_bel_id += b.len() as u32;
                            b
                        }
                        SiteType::IoPad => {
                            let b = make_iobuf_bels(next_bel_id);
                            next_bel_id += b.len() as u32;
                            b
                        }
                        SiteType::BramSite => {
                            let b = make_bram_bels(next_bel_id);
                            next_bel_id += b.len() as u32;
                            b
                        }
                        SiteType::DspSite => {
                            let b = make_dsp_bels(next_bel_id);
                            next_bel_id += b.len() as u32;
                            b
                        }
                        SiteType::Pll => {
                            let b = make_pll_bels(next_bel_id);
                            next_bel_id += b.len() as u32;
                            b
                        }
                        _ => Vec::new(),
                    };

                    sites.push(Site {
                        id: site_id,
                        name: site_name.clone(),
                        site_type,
                        bels,
                        tile_col: entry.grid_x,
                        tile_row: entry.grid_y,
                    });

                    tile_site_ids.push(site_id);
                    sites_by_type.entry(site_type).or_default().push(site_id);
                }
            }

            let tile_idx = tiles.len();
            tiles.push(Tile {
                name: tile_name.clone(),
                col: entry.grid_x,
                row: entry.grid_y,
                tile_type,
                sites: tile_site_ids,
            });

            if (entry.grid_x as usize) < tile_grid.len()
                && (entry.grid_y as usize) < tile_grid[entry.grid_x as usize].len()
            {
                tile_grid[entry.grid_x as usize][entry.grid_y as usize] = Some(tile_idx);
            }
        }

        Self {
            base,
            tiles,
            tile_grid,
            sites,
            sites_by_type,
            grid_dims: (cols, rows),
            db,
        }
    }

    /// Returns a reference to the underlying X-Ray database.
    pub fn database(&self) -> &XRayDatabase {
        &self.db
    }

    /// Returns the number of tiles in the device grid.
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Returns the number of sites in the device.
    pub fn site_count(&self) -> usize {
        self.sites.len()
    }
}

impl Architecture for Artix7XRay {
    fn family_name(&self) -> &str {
        self.base.family_name()
    }

    fn device_name(&self) -> &str {
        self.base.device_name()
    }

    fn total_luts(&self) -> u32 {
        self.base.total_luts()
    }

    fn total_ffs(&self) -> u32 {
        self.base.total_ffs()
    }

    fn total_bram(&self) -> u32 {
        self.base.total_bram()
    }

    fn total_dsp(&self) -> u32 {
        self.base.total_dsp()
    }

    fn total_io(&self) -> u32 {
        self.base.total_io()
    }

    fn total_pll(&self) -> u32 {
        self.base.total_pll()
    }

    fn lut_input_count(&self) -> u32 {
        self.base.lut_input_count()
    }

    fn resource_summary(&self) -> ResourceUsage {
        self.base.resource_summary()
    }

    fn tech_mapper(&self) -> Box<dyn TechMapper> {
        Box::new(Artix7Mapper)
    }

    fn grid_dimensions(&self) -> (u32, u32) {
        self.grid_dims
    }

    fn get_tile(&self, col: u32, row: u32) -> Option<&Tile> {
        let col_idx = col as usize;
        let row_idx = row as usize;
        self.tile_grid
            .get(col_idx)
            .and_then(|column| column.get(row_idx))
            .and_then(|idx| idx.map(|i| &self.tiles[i]))
    }

    fn get_site(&self, id: SiteId) -> Option<&Site> {
        self.sites.get(id.as_raw() as usize)
    }

    fn sites_of_type(&self, site_type: SiteType) -> Vec<SiteId> {
        self.sites_by_type
            .get(&site_type)
            .cloned()
            .unwrap_or_default()
    }

    fn routing_graph(&self) -> &RoutingGraph {
        // Routing graph will be built in Milestone 4
        static EMPTY: RoutingGraph = RoutingGraph {
            wires: Vec::new(),
            pips: Vec::new(),
        };
        &EMPTY
    }

    fn pip_delay(&self, _pip: aion_arch::PipId) -> Delay {
        // Real delays will be added in Milestone 4
        Delay::new(0.1, 0.2, 0.3)
    }

    fn wire_delay(&self, _wire: aion_arch::WireId) -> Delay {
        Delay::new(0.01, 0.02, 0.03)
    }

    fn cell_delay(&self, cell_type: &str) -> Delay {
        match cell_type {
            "LUT6" | "LUT5" | "LUT4" | "LUT3" | "LUT2" | "LUT1" => Delay::new(0.1, 0.12, 0.15),
            "FDRE" | "FDSE" | "FDCE" | "FDPE" => Delay::new(0.0, 0.0, 0.0),
            "CARRY4" => Delay::new(0.05, 0.07, 0.1),
            _ => Delay::ZERO,
        }
    }

    fn setup_time(&self, cell_type: &str) -> Delay {
        match cell_type {
            "FDRE" | "FDSE" | "FDCE" | "FDPE" => Delay::new(0.03, 0.04, 0.06),
            _ => Delay::ZERO,
        }
    }

    fn hold_time(&self, cell_type: &str) -> Delay {
        match cell_type {
            "FDRE" | "FDSE" | "FDCE" | "FDPE" => Delay::new(0.01, 0.02, 0.03),
            _ => Delay::ZERO,
        }
    }

    fn clock_to_out(&self, cell_type: &str) -> Delay {
        match cell_type {
            "FDRE" | "FDSE" | "FDCE" | "FDPE" => Delay::new(0.1, 0.15, 0.2),
            _ => Delay::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tilegrid::{TileBitSegment, TileGridEntry};
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Creates a minimal in-memory XRayDatabase for testing.
    fn make_test_db() -> XRayDatabase {
        let mut tilegrid = HashMap::new();

        // CLB tile with two SLICEL sites
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
        clb_sites.insert("SLICE_X1Y0".to_string(), "SLICEL".to_string());
        tilegrid.insert(
            "CLBLL_L_X0Y0".to_string(),
            TileGridEntry {
                bits: clb_bits,
                grid_x: 1,
                grid_y: 1,
                tile_type: "CLBLL_L".to_string(),
                sites: clb_sites,
            },
        );

        // INT tile (no sites)
        let mut int_bits = HashMap::new();
        int_bits.insert(
            "CLB_IO_CLK".to_string(),
            TileBitSegment {
                baseaddr: 0x00020800,
                frames: 26,
                offset: 50,
                words: 2,
            },
        );
        tilegrid.insert(
            "INT_L_X1Y1".to_string(),
            TileGridEntry {
                bits: int_bits,
                grid_x: 2,
                grid_y: 1,
                tile_type: "INT_L".to_string(),
                sites: HashMap::new(),
            },
        );

        // IO tile
        let mut io_sites = HashMap::new();
        io_sites.insert("IOB_X0Y0".to_string(), "IOB33M".to_string());
        tilegrid.insert(
            "LIOB33_X0Y0".to_string(),
            TileGridEntry {
                bits: HashMap::new(),
                grid_x: 0,
                grid_y: 0,
                tile_type: "LIOB33".to_string(),
                sites: io_sites,
            },
        );

        XRayDatabase {
            part: "xc7a35t".to_string(),
            tilegrid,
            segbits: HashMap::new(),
            tile_types: HashMap::new(),
            db_path: PathBuf::from("/test"),
        }
    }

    #[test]
    fn classify_clb_tiles() {
        assert_eq!(classify_tile_type("CLBLL_L"), TileType::Logic);
        assert_eq!(classify_tile_type("CLBLL_R"), TileType::Logic);
        assert_eq!(classify_tile_type("CLBLM_L"), TileType::Logic);
        assert_eq!(classify_tile_type("CLBLM_R"), TileType::Logic);
    }

    #[test]
    fn classify_int_tiles() {
        assert_eq!(classify_tile_type("INT_L"), TileType::Interconnect);
        assert_eq!(classify_tile_type("INT_R"), TileType::Interconnect);
    }

    #[test]
    fn classify_io_tiles() {
        assert_eq!(classify_tile_type("LIOB33"), TileType::Io);
        assert_eq!(classify_tile_type("RIOB33"), TileType::Io);
    }

    #[test]
    fn classify_bram_tiles() {
        assert_eq!(classify_tile_type("BRAM_L"), TileType::Bram);
        assert_eq!(classify_tile_type("BRAM_R"), TileType::Bram);
    }

    #[test]
    fn classify_dsp_tiles() {
        assert_eq!(classify_tile_type("DSP_L"), TileType::Dsp);
        assert_eq!(classify_tile_type("DSP_R"), TileType::Dsp);
    }

    #[test]
    fn classify_clock_tiles() {
        assert_eq!(classify_tile_type("CMT_TOP_L_LOWER_B"), TileType::Clock);
    }

    #[test]
    fn classify_unknown_tile() {
        assert_eq!(classify_tile_type("UNKNOWN_TYPE"), TileType::Empty);
    }

    #[test]
    fn classify_site_types() {
        assert_eq!(classify_site_type("SLICEL"), Some(SiteType::LutFf));
        assert_eq!(classify_site_type("SLICEM"), Some(SiteType::LutFf));
        assert_eq!(classify_site_type("IOB33M"), Some(SiteType::IoPad));
        assert_eq!(classify_site_type("RAMB36E1"), Some(SiteType::BramSite));
        assert_eq!(classify_site_type("DSP48E1"), Some(SiteType::DspSite));
        assert_eq!(classify_site_type("MMCME2_ADV"), Some(SiteType::Pll));
        assert_eq!(classify_site_type("UNKNOWN"), None);
    }

    #[test]
    fn build_arch_from_db() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        assert_eq!(arch.tile_count(), 3);
        assert_eq!(arch.site_count(), 3); // 2 SLICEL + 1 IOB
    }

    #[test]
    fn grid_dimensions() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let (cols, rows) = arch.grid_dimensions();
        assert_eq!(cols, 3); // 0, 1, 2
        assert_eq!(rows, 2); // 0, 1
    }

    #[test]
    fn get_tile_by_coords() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let tile = arch.get_tile(1, 1);
        assert!(tile.is_some());
        let t = tile.unwrap();
        assert_eq!(t.tile_type, TileType::Logic);
        assert_eq!(t.name, "CLBLL_L_X0Y0");
    }

    #[test]
    fn get_tile_out_of_bounds() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        assert!(arch.get_tile(100, 100).is_none());
    }

    #[test]
    fn get_site_by_id() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        // Sites are created in tile name order: CLBLL_L_X0Y0 (2 SLICELs),
        // INT_L_X1Y1 (no sites), LIOB33_X0Y0 (1 IOB)
        let site0 = arch.get_site(SiteId::from_raw(0)).unwrap();
        assert_eq!(site0.site_type, SiteType::LutFf);

        let site2 = arch.get_site(SiteId::from_raw(2)).unwrap();
        assert_eq!(site2.site_type, SiteType::IoPad);
    }

    #[test]
    fn sites_of_type_lutff() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let lutff_sites = arch.sites_of_type(SiteType::LutFf);
        assert_eq!(lutff_sites.len(), 2);
    }

    #[test]
    fn sites_of_type_iopad() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let io_sites = arch.sites_of_type(SiteType::IoPad);
        assert_eq!(io_sites.len(), 1);
    }

    #[test]
    fn sites_of_type_empty() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let pll_sites = arch.sites_of_type(SiteType::Pll);
        assert!(pll_sites.is_empty());
    }

    #[test]
    fn delegated_resource_counts() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        assert_eq!(arch.family_name(), "artix7");
        assert_eq!(arch.total_luts(), 20_800);
        assert_eq!(arch.total_ffs(), 41_600);
        assert_eq!(arch.lut_input_count(), 6);
    }

    #[test]
    fn slice_bels() {
        let bels = make_slice_bels(0);
        assert_eq!(bels.len(), 12); // 4 LUTs + 8 FFs
        assert_eq!(bels[0].bel_type, BelType::Lut);
        assert_eq!(bels[0].name, "A6LUT");
        assert_eq!(bels[4].bel_type, BelType::Ff);
        assert_eq!(bels[4].name, "AFF");
        assert_eq!(bels[5].name, "AFF2");
    }

    #[test]
    fn iobuf_bels() {
        let bels = make_iobuf_bels(0);
        assert_eq!(bels.len(), 1);
        assert_eq!(bels[0].bel_type, BelType::IoBuf);
    }

    #[test]
    fn bram_bels() {
        let bels = make_bram_bels(0);
        assert_eq!(bels.len(), 1);
        assert_eq!(bels[0].bel_type, BelType::BramPrimitive);
    }

    #[test]
    fn dsp_bels() {
        let bels = make_dsp_bels(0);
        assert_eq!(bels.len(), 1);
        assert_eq!(bels[0].bel_type, BelType::DspPrimitive);
    }

    #[test]
    fn tech_mapper_works() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let mapper = arch.tech_mapper();
        assert_eq!(mapper.lut_input_count(), 6);
    }

    #[test]
    fn cell_delays() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let lut_delay = arch.cell_delay("LUT6");
        assert!(lut_delay.typ_ns > 0.0);

        let ff_delay = arch.cell_delay("FDRE");
        assert_eq!(ff_delay.typ_ns, 0.0);
    }

    #[test]
    fn setup_hold_times() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let setup = arch.setup_time("FDRE");
        assert!(setup.typ_ns > 0.0);

        let hold = arch.hold_time("FDRE");
        assert!(hold.typ_ns > 0.0);
    }

    #[test]
    fn routing_graph_empty_placeholder() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        let rg = arch.routing_graph();
        assert!(rg.wires.is_empty());
        assert!(rg.pips.is_empty());
    }

    #[test]
    fn database_accessor() {
        let db = make_test_db();
        let (base, _) = Artix7::new("xc7a35ticpg236-1L");
        let arch = Artix7XRay::new(base, db);

        assert_eq!(arch.database().part, "xc7a35t");
    }
}
