//! Shared data types for FPGA device architecture models.
//!
//! This module defines the structural elements of an FPGA device: tiles, sites,
//! BELs, wires, PIPs, timing delays, and resource usage summaries. These types
//! are used by the [`Architecture`](crate::Architecture) trait implementations
//! for specific device families.

use crate::ids::{BelId, PipId, SiteId, WireId};
use serde::{Deserialize, Serialize};

/// The type of a tile in the FPGA grid.
///
/// Each tile occupies one position in the device grid and contains zero or more
/// sites of a specific function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TileType {
    /// A tile containing configurable logic blocks (LUTs, FFs, carry chains).
    Logic,
    /// A tile containing block RAM resources.
    Bram,
    /// A tile containing DSP multiply-accumulate blocks.
    Dsp,
    /// A tile providing I/O pad connections to the package pins.
    Io,
    /// A tile containing clock management resources (PLLs, MMCMs).
    Clock,
    /// An empty tile with no programmable resources.
    Empty,
}

/// A single tile in the FPGA device grid.
///
/// Tiles are the coarse-grained building blocks of the device, arranged in a
/// regular grid. Each tile has a type that determines what resources it contains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tile {
    /// Column index in the device grid (0-based, left to right).
    pub col: u32,
    /// Row index in the device grid (0-based, bottom to top).
    pub row: u32,
    /// The functional type of this tile.
    pub tile_type: TileType,
    /// The sites (placement locations) contained in this tile.
    pub sites: Vec<SiteId>,
}

/// The functional type of a site within a tile.
///
/// Sites are the fine-grained placement locations where BELs reside.
/// A single tile may contain multiple sites of the same or different types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SiteType {
    /// A site containing a single look-up table.
    Lut,
    /// A site containing a single flip-flop.
    Ff,
    /// A site containing a paired LUT and flip-flop (Xilinx SLICEL/SLICEM style).
    LutFf,
    /// An adaptive logic module (Intel ALM style, contains 2 LUTs + 2 FFs).
    Alm,
    /// A block RAM site.
    BramSite,
    /// A DSP multiply-accumulate site.
    DspSite,
    /// An I/O pad site for external pin connections.
    IoPad,
    /// A PLL/MMCM clock management site.
    Pll,
}

/// The type of a basic element of logic (BEL) within a site.
///
/// BELs are the atomic programmable resources that cells are mapped onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BelType {
    /// A look-up table BEL, implementing arbitrary Boolean functions.
    Lut,
    /// A flip-flop BEL for sequential storage.
    Ff,
    /// A carry-chain BEL for arithmetic operations.
    Carry,
    /// A multiplexer BEL for routing within a site.
    Mux,
    /// A block RAM primitive BEL.
    BramPrimitive,
    /// A DSP multiply-accumulate primitive BEL.
    DspPrimitive,
    /// An I/O buffer BEL connecting to a package pin.
    IoBuf,
    /// A PLL/MMCM primitive BEL for clock synthesis.
    PllPrimitive,
}

/// A basic element of logic (BEL) within a site.
///
/// BELs are the smallest addressable resources in the FPGA fabric. During
/// placement, each technology-mapped cell is assigned to exactly one BEL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bel {
    /// The unique ID of this BEL.
    pub id: BelId,
    /// The instance name of this BEL (e.g., "A6LUT", "AFF").
    pub name: String,
    /// The functional type of this BEL.
    pub bel_type: BelType,
}

/// A site (placement location) within a tile.
///
/// Sites group related BELs together. For example, a Xilinx SLICEL site
/// contains 4 LUT BELs, 8 FF BELs, and carry chain BELs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    /// The unique ID of this site.
    pub id: SiteId,
    /// The functional type of this site.
    pub site_type: SiteType,
    /// The BELs contained in this site.
    pub bels: Vec<Bel>,
    /// The column of the tile containing this site.
    pub tile_col: u32,
    /// The row of the tile containing this site.
    pub tile_row: u32,
}

/// A routing wire segment in the device interconnect fabric.
///
/// Wires connect between PIPs and site pins, forming the routing network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wire {
    /// The unique ID of this wire.
    pub id: WireId,
    /// The name of this wire (e.g., "CLB_LL_N3LUT_0").
    pub name: String,
}

/// A programmable interconnect point (PIP) connecting two wires.
///
/// PIPs are the switches in the routing fabric that can be turned on to
/// connect one wire to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pip {
    /// The unique ID of this PIP.
    pub id: PipId,
    /// The source wire that drives this PIP.
    pub src_wire: WireId,
    /// The destination wire that this PIP drives.
    pub dst_wire: WireId,
    /// The delay through this PIP when enabled.
    pub delay: Delay,
}

/// A minimal routing graph for the device interconnect.
///
/// This is a placeholder for Phase 3 (place & route). Currently empty,
/// it will be populated with the full wire and PIP connectivity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingGraph {
    /// All routing wires in the device.
    pub wires: Vec<Wire>,
    /// All programmable interconnect points in the device.
    pub pips: Vec<Pip>,
}

/// A timing delay with min/typical/max corners.
///
/// Represents the propagation delay through a device element across
/// different process/voltage/temperature corners.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Delay {
    /// Minimum delay in nanoseconds (fast corner).
    pub min_ns: f64,
    /// Typical delay in nanoseconds (nominal corner).
    pub typ_ns: f64,
    /// Maximum delay in nanoseconds (slow corner).
    pub max_ns: f64,
}

impl Delay {
    /// A zero delay (no propagation time).
    pub const ZERO: Self = Self {
        min_ns: 0.0,
        typ_ns: 0.0,
        max_ns: 0.0,
    };

    /// Creates a new delay with the given min/typ/max values.
    pub fn new(min_ns: f64, typ_ns: f64, max_ns: f64) -> Self {
        Self {
            min_ns,
            typ_ns,
            max_ns,
        }
    }
}

impl Default for Delay {
    fn default() -> Self {
        Self::ZERO
    }
}

/// A summary of FPGA resource usage for a design or device.
///
/// Tracks how many of each major resource type are used or available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResourceUsage {
    /// Number of look-up tables used.
    pub luts: u32,
    /// Number of flip-flops used.
    pub ffs: u32,
    /// Number of block RAM tiles used.
    pub bram: u32,
    /// Number of DSP blocks used.
    pub dsp: u32,
    /// Number of I/O pads used.
    pub io: u32,
    /// Number of PLL/MMCM blocks used.
    pub pll: u32,
}

impl ResourceUsage {
    /// Returns the total number of logic resources (LUTs + FFs).
    pub fn total_logic(&self) -> u32 {
        self.luts + self.ffs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_type_variants() {
        let types = [
            TileType::Logic,
            TileType::Bram,
            TileType::Dsp,
            TileType::Io,
            TileType::Clock,
            TileType::Empty,
        ];
        for (i, t) in types.iter().enumerate() {
            for (j, u) in types.iter().enumerate() {
                if i == j {
                    assert_eq!(t, u);
                } else {
                    assert_ne!(t, u);
                }
            }
        }
    }

    #[test]
    fn tile_construction() {
        let tile = Tile {
            col: 5,
            row: 10,
            tile_type: TileType::Logic,
            sites: vec![SiteId::from_raw(0), SiteId::from_raw(1)],
        };
        assert_eq!(tile.col, 5);
        assert_eq!(tile.row, 10);
        assert_eq!(tile.tile_type, TileType::Logic);
        assert_eq!(tile.sites.len(), 2);
    }

    #[test]
    fn site_type_variants() {
        let types = [
            SiteType::Lut,
            SiteType::Ff,
            SiteType::LutFf,
            SiteType::Alm,
            SiteType::BramSite,
            SiteType::DspSite,
            SiteType::IoPad,
            SiteType::Pll,
        ];
        assert_eq!(types.len(), 8);
        assert_ne!(types[0], types[1]);
    }

    #[test]
    fn bel_type_variants() {
        let types = [
            BelType::Lut,
            BelType::Ff,
            BelType::Carry,
            BelType::Mux,
            BelType::BramPrimitive,
            BelType::DspPrimitive,
            BelType::IoBuf,
            BelType::PllPrimitive,
        ];
        assert_eq!(types.len(), 8);
        assert_ne!(types[0], types[1]);
    }

    #[test]
    fn bel_construction() {
        let bel = Bel {
            id: BelId::from_raw(0),
            name: "A6LUT".to_string(),
            bel_type: BelType::Lut,
        };
        assert_eq!(bel.name, "A6LUT");
        assert_eq!(bel.bel_type, BelType::Lut);
    }

    #[test]
    fn site_construction() {
        let site = Site {
            id: SiteId::from_raw(0),
            site_type: SiteType::LutFf,
            bels: vec![
                Bel {
                    id: BelId::from_raw(0),
                    name: "A6LUT".to_string(),
                    bel_type: BelType::Lut,
                },
                Bel {
                    id: BelId::from_raw(1),
                    name: "AFF".to_string(),
                    bel_type: BelType::Ff,
                },
            ],
            tile_col: 3,
            tile_row: 7,
        };
        assert_eq!(site.bels.len(), 2);
        assert_eq!(site.tile_col, 3);
    }

    #[test]
    fn delay_zero() {
        let d = Delay::ZERO;
        assert_eq!(d.min_ns, 0.0);
        assert_eq!(d.typ_ns, 0.0);
        assert_eq!(d.max_ns, 0.0);
    }

    #[test]
    fn delay_new() {
        let d = Delay::new(0.1, 0.2, 0.3);
        assert_eq!(d.min_ns, 0.1);
        assert_eq!(d.typ_ns, 0.2);
        assert_eq!(d.max_ns, 0.3);
    }

    #[test]
    fn delay_default() {
        let d = Delay::default();
        assert_eq!(d, Delay::ZERO);
    }

    #[test]
    fn resource_usage_default() {
        let r = ResourceUsage::default();
        assert_eq!(r.luts, 0);
        assert_eq!(r.ffs, 0);
        assert_eq!(r.bram, 0);
        assert_eq!(r.dsp, 0);
        assert_eq!(r.io, 0);
        assert_eq!(r.pll, 0);
    }

    #[test]
    fn resource_usage_total_logic() {
        let r = ResourceUsage {
            luts: 100,
            ffs: 200,
            bram: 5,
            dsp: 3,
            io: 10,
            pll: 1,
        };
        assert_eq!(r.total_logic(), 300);
    }

    #[test]
    fn routing_graph_default() {
        let g = RoutingGraph::default();
        assert!(g.wires.is_empty());
        assert!(g.pips.is_empty());
    }

    #[test]
    fn tile_type_serde_roundtrip() {
        let t = TileType::Dsp;
        let json = serde_json::to_string(&t).unwrap();
        let restored: TileType = serde_json::from_str(&json).unwrap();
        assert_eq!(t, restored);
    }

    #[test]
    fn resource_usage_serde_roundtrip() {
        let r = ResourceUsage {
            luts: 1000,
            ffs: 2000,
            bram: 50,
            dsp: 20,
            io: 100,
            pll: 4,
        };
        let json = serde_json::to_string(&r).unwrap();
        let restored: ResourceUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(r, restored);
    }

    #[test]
    fn delay_serde_roundtrip() {
        let d = Delay::new(0.5, 1.0, 1.5);
        let json = serde_json::to_string(&d).unwrap();
        let restored: Delay = serde_json::from_str(&json).unwrap();
        assert_eq!(d, restored);
    }

    #[test]
    fn wire_and_pip_construction() {
        let w = Wire {
            id: WireId::from_raw(0),
            name: "CLB_LL_N3LUT_0".to_string(),
        };
        assert_eq!(w.name, "CLB_LL_N3LUT_0");

        let p = Pip {
            id: PipId::from_raw(0),
            src_wire: WireId::from_raw(0),
            dst_wire: WireId::from_raw(1),
            delay: Delay::new(0.1, 0.2, 0.3),
        };
        assert_eq!(p.src_wire, WireId::from_raw(0));
        assert_eq!(p.dst_wire, WireId::from_raw(1));
    }
}
