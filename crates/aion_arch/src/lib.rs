//! FPGA device architecture models for the Aion toolchain.
//!
//! This crate provides the [`Architecture`] trait that abstracts over different
//! FPGA device families, and the [`TechMapper`] trait for mapping generic IR
//! cells to device-specific primitives. Concrete implementations are provided
//! for Intel Cyclone IV E, Intel Cyclone V, and Xilinx Artix-7 families.
//!
//! # Usage
//!
//! Use [`load_architecture`] to create an architecture model by family and
//! device name:
//!
//! ```
//! use aion_arch::load_architecture;
//!
//! let arch = load_architecture("cyclone_v", "5CSEMA5F31C6").unwrap();
//! assert_eq!(arch.total_luts(), 32_070);
//! ```
//!
//! # Architecture
//!
//! - Phase 2 methods (resource counts, tech mapping) are fully implemented.
//! - Phase 3 methods (grid topology, routing, timing) have default stub
//!   implementations returning empty/zero values, to be filled in during
//!   place-and-route development.

#![warn(missing_docs)]

pub mod ids;
pub mod intel;
pub mod tech_map;
pub mod types;
pub mod xilinx;

use ids::SiteId;
use tech_map::TechMapper;
use types::{Delay, ResourceUsage, RoutingGraph, Site, Tile};

pub use ids::{BelId, PipId, WireId};
pub use intel::cyclone_iv::{CycloneIv, CycloneIvMapper};
pub use intel::cyclone_v::{CycloneV, CycloneVMapper};
pub use intel::IntelFamily;
pub use tech_map::{
    ArithmeticPattern, ArithmeticPatternKind, LogicCone, LutMapping, MapResult, MemoryCell,
};
pub use types::{Bel, BelType, Pip, SiteType, TileType, Wire};
pub use xilinx::artix7::{Artix7, Artix7Mapper};
pub use xilinx::XilinxFamily;

use aion_common::{AionResult, InternalError};

/// The core trait for an FPGA device architecture model.
///
/// Provides resource counts, technology mapping, and (in Phase 3) grid topology,
/// routing graph, and timing information. Each supported device family has a
/// concrete implementation of this trait.
///
/// Phase 2 methods (resource queries, tech mapper) must be implemented by
/// all architectures. Phase 3 methods (grid, routing, timing) have default
/// implementations returning empty/zero values.
pub trait Architecture: std::fmt::Debug {
    // --- Phase 2: Resource queries (required) ---

    /// Returns the canonical family name (e.g., "cyclone_v", "artix7").
    fn family_name(&self) -> &str;

    /// Returns the device part number (e.g., "5CSEMA5F31C6").
    fn device_name(&self) -> &str;

    /// Returns the total number of LUTs (or ALMs for Intel) in the device.
    fn total_luts(&self) -> u32;

    /// Returns the total number of flip-flops in the device.
    fn total_ffs(&self) -> u32;

    /// Returns the total number of block RAM tiles in the device.
    fn total_bram(&self) -> u32;

    /// Returns the total number of DSP blocks in the device.
    fn total_dsp(&self) -> u32;

    /// Returns the total number of user I/O pins on the device.
    fn total_io(&self) -> u32;

    /// Returns the total number of PLL/MMCM blocks in the device.
    fn total_pll(&self) -> u32;

    /// Returns the number of inputs per LUT on this device (typically 4 or 6).
    fn lut_input_count(&self) -> u32;

    /// Returns a summary of the total device resources.
    fn resource_summary(&self) -> ResourceUsage;

    /// Creates a technology mapper for this device family.
    fn tech_mapper(&self) -> Box<dyn TechMapper>;

    // --- Phase 3: Grid topology (default stubs) ---

    /// Returns the device grid dimensions as (columns, rows).
    ///
    /// Default returns (0, 0) — will be populated in Phase 3.
    fn grid_dimensions(&self) -> (u32, u32) {
        (0, 0)
    }

    /// Returns the tile at the given grid coordinates, if it exists.
    ///
    /// Default returns `None` — will be populated in Phase 3.
    fn get_tile(&self, _col: u32, _row: u32) -> Option<&Tile> {
        None
    }

    /// Returns the site with the given ID, if it exists.
    ///
    /// Default returns `None` — will be populated in Phase 3.
    fn get_site(&self, _id: SiteId) -> Option<&Site> {
        None
    }

    /// Returns an iterator over all sites of the given type.
    ///
    /// Default returns an empty vector — will be populated in Phase 3.
    fn sites_of_type(&self, _site_type: types::SiteType) -> Vec<SiteId> {
        Vec::new()
    }

    // --- Phase 3: Routing (default stubs) ---

    /// Returns the device routing graph.
    ///
    /// Default returns an empty graph — will be populated in Phase 3.
    fn routing_graph(&self) -> &RoutingGraph {
        &EMPTY_ROUTING_GRAPH
    }

    /// Returns the delay through a PIP.
    ///
    /// Default returns zero delay — will be populated in Phase 3.
    fn pip_delay(&self, _pip: ids::PipId) -> Delay {
        Delay::ZERO
    }

    /// Returns the delay along a wire segment.
    ///
    /// Default returns zero delay — will be populated in Phase 3.
    fn wire_delay(&self, _wire: ids::WireId) -> Delay {
        Delay::ZERO
    }

    // --- Phase 3: Timing (default stubs) ---

    /// Returns the combinational delay through a cell of the given type.
    ///
    /// The `cell_type` parameter is a string identifier (e.g., "LUT6", "CARRY4")
    /// to avoid circular dependencies with downstream crates.
    ///
    /// Default returns zero delay — will be populated in Phase 3.
    fn cell_delay(&self, _cell_type: &str) -> Delay {
        Delay::ZERO
    }

    /// Returns the setup time for the given cell type relative to its clock.
    ///
    /// Default returns zero delay — will be populated in Phase 3.
    fn setup_time(&self, _cell_type: &str) -> Delay {
        Delay::ZERO
    }

    /// Returns the hold time for the given cell type relative to its clock.
    ///
    /// Default returns zero delay — will be populated in Phase 3.
    fn hold_time(&self, _cell_type: &str) -> Delay {
        Delay::ZERO
    }

    /// Returns the clock-to-output delay for the given cell type.
    ///
    /// Default returns zero delay — will be populated in Phase 3.
    fn clock_to_out(&self, _cell_type: &str) -> Delay {
        Delay::ZERO
    }
}

/// A static empty routing graph used as the default return value.
static EMPTY_ROUTING_GRAPH: RoutingGraph = RoutingGraph {
    wires: Vec::new(),
    pips: Vec::new(),
};

/// Loads an architecture model for the given family and device.
///
/// Supported families: `"cyclone_iv"`, `"cyclone_v"`, `"artix7"`.
///
/// If the device part number is not found within the family, falls back to the
/// smallest known device and returns a warning-level result (the `Architecture`
/// is still usable). Returns an error only if the family name is unknown.
///
/// # Errors
///
/// Returns `InternalError` if the family name is not recognized.
pub fn load_architecture(family: &str, device: &str) -> AionResult<Box<dyn Architecture>> {
    match family.to_ascii_lowercase().as_str() {
        "cyclone_iv" | "cycloneiv" | "cyclone-iv" | "cyclone4" | "cyclone_4" => {
            let (arch, _exact) = CycloneIv::new(device);
            Ok(Box::new(arch))
        }
        "cyclone_v" | "cyclonev" | "cyclone-v" => {
            let (arch, _exact) = CycloneV::new(device);
            Ok(Box::new(arch))
        }
        "artix7" | "artix-7" | "artix_7" => {
            let (arch, _exact) = Artix7::new(device);
            Ok(Box::new(arch))
        }
        _ => Err(InternalError::new(format!(
            "unknown FPGA family: {family:?}. Supported: cyclone_iv, cyclone_v, artix7"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_cyclone_iv() {
        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        assert_eq!(arch.family_name(), "cyclone_iv");
        assert_eq!(arch.device_name(), "EP4CE22F17C6N");
        assert_eq!(arch.total_luts(), 22_320);
        assert_eq!(arch.lut_input_count(), 4);
    }

    #[test]
    fn load_cyclone_iv_aliases() {
        let arch = load_architecture("cycloneiv", "EP4CE22F17C6N").unwrap();
        assert_eq!(arch.family_name(), "cyclone_iv");

        let arch = load_architecture("cyclone-iv", "EP4CE22F17C6N").unwrap();
        assert_eq!(arch.family_name(), "cyclone_iv");

        let arch = load_architecture("cyclone4", "EP4CE22F17C6N").unwrap();
        assert_eq!(arch.family_name(), "cyclone_iv");

        let arch = load_architecture("cyclone_4", "EP4CE22F17C6N").unwrap();
        assert_eq!(arch.family_name(), "cyclone_iv");
    }

    #[test]
    fn load_cyclone_iv_tech_mapper() {
        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let mapper = arch.tech_mapper();
        assert_eq!(mapper.lut_input_count(), 4);
        assert_eq!(mapper.max_bram_depth(), 9_216);
        assert_eq!(mapper.max_bram_width(), 36);
    }

    #[test]
    fn load_cyclone_v() {
        let arch = load_architecture("cyclone_v", "5CSEMA5F31C6").unwrap();
        assert_eq!(arch.family_name(), "cyclone_v");
        assert_eq!(arch.device_name(), "5CSEMA5F31C6");
        assert_eq!(arch.total_luts(), 32_070);
    }

    #[test]
    fn load_cyclone_v_alias() {
        let arch = load_architecture("cyclonev", "5CSEMA5F31C6").unwrap();
        assert_eq!(arch.family_name(), "cyclone_v");
    }

    #[test]
    fn load_cyclone_v_hyphen() {
        let arch = load_architecture("cyclone-v", "5CSEMA5F31C6").unwrap();
        assert_eq!(arch.family_name(), "cyclone_v");
    }

    #[test]
    fn load_artix7() {
        let arch = load_architecture("artix7", "xc7a100tcsg324-1").unwrap();
        assert_eq!(arch.family_name(), "artix7");
        assert_eq!(arch.device_name(), "xc7a100tcsg324-1");
        assert_eq!(arch.total_luts(), 63_400);
    }

    #[test]
    fn load_artix7_alias() {
        let arch = load_architecture("artix-7", "xc7a100tcsg324-1").unwrap();
        assert_eq!(arch.family_name(), "artix7");
    }

    #[test]
    fn load_artix7_underscore() {
        let arch = load_architecture("artix_7", "xc7a100tcsg324-1").unwrap();
        assert_eq!(arch.family_name(), "artix7");
    }

    #[test]
    fn load_unknown_family() {
        let result = load_architecture("spartan3", "xc3s500e");
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.message.contains("unknown FPGA family"));
    }

    #[test]
    fn load_unknown_device_fallback() {
        let arch = load_architecture("cyclone_v", "UNKNOWN_PART").unwrap();
        // Falls back to smallest device
        assert_eq!(arch.device_name(), "5CEBA4F23C7");
    }

    #[test]
    fn architecture_case_insensitive() {
        let arch = load_architecture("CYCLONE_V", "5CSEMA5F31C6").unwrap();
        assert_eq!(arch.family_name(), "cyclone_v");
    }

    #[test]
    fn architecture_default_methods() {
        let arch = load_architecture("artix7", "xc7a35ticpg236-1L").unwrap();
        assert_eq!(arch.grid_dimensions(), (0, 0));
        assert!(arch.get_tile(0, 0).is_none());
        assert!(arch.get_site(SiteId::from_raw(0)).is_none());
        assert!(arch.sites_of_type(types::SiteType::Lut).is_empty());
        assert!(arch.routing_graph().wires.is_empty());
        assert_eq!(arch.pip_delay(PipId::from_raw(0)), Delay::ZERO);
        assert_eq!(arch.wire_delay(WireId::from_raw(0)), Delay::ZERO);
        assert_eq!(arch.cell_delay("LUT6"), Delay::ZERO);
        assert_eq!(arch.setup_time("FDRE"), Delay::ZERO);
        assert_eq!(arch.hold_time("FDRE"), Delay::ZERO);
        assert_eq!(arch.clock_to_out("FDRE"), Delay::ZERO);
    }

    #[test]
    fn architecture_tech_mapper() {
        let arch = load_architecture("cyclone_v", "5CSEMA5F31C6").unwrap();
        let mapper = arch.tech_mapper();
        assert_eq!(mapper.lut_input_count(), 6);
        assert_eq!(mapper.max_bram_depth(), 10_240);
    }

    #[test]
    fn architecture_resource_summary() {
        let arch = load_architecture("artix7", "xc7a200tffg1156-1").unwrap();
        let summary = arch.resource_summary();
        assert_eq!(summary.luts, 134_600);
        assert_eq!(summary.total_logic(), 134_600 + 269_200);
    }
}
