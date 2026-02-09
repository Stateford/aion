//! Random initial placement.
//!
//! Assigns each unplaced cell to a random compatible site. In Phase 2,
//! site IDs are synthetic (generated from resource counts) since the device
//! grid is not yet populated. This provides a valid starting point for
//! simulated annealing refinement.

use crate::data::{PnrCellType, PnrNetlist};
use aion_arch::ids::SiteId;
use aion_arch::Architecture;
use aion_diagnostics::DiagnosticSink;
use rand::Rng;

/// Assigns each unplaced cell to a random site.
///
/// Uses synthetic site IDs derived from the architecture's total resource
/// counts. Each cell type maps to a different range of site IDs to ensure
/// compatibility. Fixed cells are left unchanged.
pub(crate) fn random_placement(
    netlist: &mut PnrNetlist,
    arch: &dyn Architecture,
    _sink: &DiagnosticSink,
) {
    let mut rng = rand::thread_rng();

    // Site ID ranges for each resource type (synthetic for Phase 2)
    let lut_base: u32 = 0;
    let lut_max = arch.total_luts();
    let ff_base = lut_max;
    let ff_max = ff_base + arch.total_ffs();
    let bram_base = ff_max;
    let bram_max = bram_base + arch.total_bram();
    let dsp_base = bram_max;
    let dsp_max = dsp_base + arch.total_dsp();
    let io_base = dsp_max;
    let io_max = io_base + arch.total_io();
    let pll_base = io_max;
    let pll_max = pll_base + arch.total_pll();

    // Track used sites to avoid conflicts
    let mut used_sites = std::collections::HashSet::new();

    // Pre-collect fixed cell sites
    for cell in &netlist.cells {
        if cell.is_fixed {
            if let Some(site) = cell.placement {
                used_sites.insert(site);
            }
        }
    }

    for i in 0..netlist.cells.len() {
        if netlist.cells[i].is_fixed {
            // Fixed cells may not have a placement yet (e.g., IO cells with no pin assignment)
            if netlist.cells[i].placement.is_none() {
                let (base, max) = site_range_for_type(
                    &netlist.cells[i].cell_type,
                    lut_base,
                    lut_max,
                    ff_base,
                    ff_max,
                    bram_base,
                    bram_max,
                    dsp_base,
                    dsp_max,
                    io_base,
                    io_max,
                    pll_base,
                    pll_max,
                );
                if let Some(site) = find_unused_site(&mut rng, base, max, &used_sites) {
                    netlist.cells[i].placement = Some(site);
                    used_sites.insert(site);
                }
            }
            continue;
        }

        let (base, max) = site_range_for_type(
            &netlist.cells[i].cell_type,
            lut_base,
            lut_max,
            ff_base,
            ff_max,
            bram_base,
            bram_max,
            dsp_base,
            dsp_max,
            io_base,
            io_max,
            pll_base,
            pll_max,
        );

        if let Some(site) = find_unused_site(&mut rng, base, max, &used_sites) {
            netlist.cells[i].placement = Some(site);
            used_sites.insert(site);
        }
    }
}

/// Returns the (base, max) site ID range for a given cell type.
#[allow(clippy::too_many_arguments)]
fn site_range_for_type(
    cell_type: &PnrCellType,
    lut_base: u32,
    lut_max: u32,
    ff_base: u32,
    ff_max: u32,
    bram_base: u32,
    bram_max: u32,
    dsp_base: u32,
    dsp_max: u32,
    io_base: u32,
    io_max: u32,
    pll_base: u32,
    pll_max: u32,
) -> (u32, u32) {
    match cell_type {
        PnrCellType::Lut { .. } | PnrCellType::Carry => (lut_base, lut_max),
        PnrCellType::Dff => (ff_base, ff_max),
        PnrCellType::Bram(_) => (bram_base, bram_max),
        PnrCellType::Dsp(_) => (dsp_base, dsp_max),
        PnrCellType::Iobuf { .. } => (io_base, io_max),
        PnrCellType::Pll(_) => (pll_base, pll_max),
    }
}

/// Finds an unused site ID in the given range.
fn find_unused_site(
    rng: &mut impl Rng,
    base: u32,
    max: u32,
    used: &std::collections::HashSet<SiteId>,
) -> Option<SiteId> {
    if base >= max {
        return None;
    }

    // Try random first (fast for sparse usage)
    for _ in 0..100 {
        let site = SiteId::from_raw(rng.gen_range(base..max));
        if !used.contains(&site) {
            return Some(site);
        }
    }

    // Fall back to linear scan
    for i in base..max {
        let site = SiteId::from_raw(i);
        if !used.contains(&site) {
            return Some(site);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{PnrCell, PnrCellType};
    use crate::ids::PnrCellId;
    use aion_arch::load_architecture;
    use aion_common::LogicVec;

    #[test]
    fn random_placement_assigns_sites() {
        let mut nl = PnrNetlist::new();
        for i in 0..10 {
            nl.add_cell(PnrCell {
                id: PnrCellId::from_raw(0),
                name: format!("lut_{i}"),
                cell_type: PnrCellType::Lut {
                    inputs: 4,
                    init: LogicVec::from_bool(false),
                },
                placement: None,
                is_fixed: false,
            });
        }

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let sink = DiagnosticSink::new();
        random_placement(&mut nl, &*arch, &sink);

        assert!(nl.is_fully_placed());
        // All placements should be unique
        let sites: std::collections::HashSet<_> =
            nl.cells.iter().map(|c| c.placement.unwrap()).collect();
        assert_eq!(sites.len(), 10);
    }

    #[test]
    fn random_placement_preserves_fixed() {
        let mut nl = PnrNetlist::new();
        let fixed_site = SiteId::from_raw(999);
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "fixed_io".into(),
            cell_type: PnrCellType::Iobuf {
                direction: aion_ir::PortDirection::Input,
                standard: "LVCMOS33".into(),
            },
            placement: Some(fixed_site),
            is_fixed: true,
        });

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let sink = DiagnosticSink::new();
        random_placement(&mut nl, &*arch, &sink);

        assert_eq!(nl.cells[0].placement, Some(fixed_site));
    }

    #[test]
    fn random_placement_different_cell_types() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: None,
            is_fixed: false,
        });
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "ff_0".into(),
            cell_type: PnrCellType::Dff,
            placement: None,
            is_fixed: false,
        });

        let arch = load_architecture("artix7", "xc7a100tcsg324-1").unwrap();
        let sink = DiagnosticSink::new();
        random_placement(&mut nl, &*arch, &sink);

        assert!(nl.is_fully_placed());
        // Different cell types should get different site ranges
        let lut_site = nl.cells[0].placement.unwrap().as_raw();
        let ff_site = nl.cells[1].placement.unwrap().as_raw();
        assert_ne!(lut_site, ff_site);
    }

    #[test]
    fn find_unused_site_basic() {
        let mut rng = rand::thread_rng();
        let used = std::collections::HashSet::new();
        let site = find_unused_site(&mut rng, 0, 100, &used);
        assert!(site.is_some());
    }

    #[test]
    fn find_unused_site_empty_range() {
        let mut rng = rand::thread_rng();
        let used = std::collections::HashSet::new();
        let site = find_unused_site(&mut rng, 0, 0, &used);
        assert!(site.is_none());
    }
}
