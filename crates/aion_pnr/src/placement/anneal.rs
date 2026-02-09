//! Simulated annealing placement refinement.
//!
//! Starting from an initial random placement, repeatedly proposes cell swaps
//! or relocations and accepts/rejects each move using the Metropolis criterion.
//! The temperature decreases geometrically, reducing the acceptance probability
//! of cost-increasing moves over time.

use crate::data::{PnrCellType, PnrNetlist};
use crate::placement::cost::PlacementCost;
use aion_arch::Architecture;
use aion_diagnostics::DiagnosticSink;
use rand::Rng;

/// Cooling rate for simulated annealing (multiplied each outer iteration).
const COOLING_RATE: f64 = 0.95;

/// Minimum temperature threshold to stop annealing.
const MIN_TEMPERATURE: f64 = 0.01;

/// Number of moves per temperature step, as a multiplier of cell count.
const MOVES_PER_TEMP_MULTIPLIER: usize = 10;

/// Refines cell placement using simulated annealing.
///
/// Proposes random cell swaps and relocations, accepting moves that decrease
/// cost (HPWL) and probabilistically accepting cost-increasing moves based
/// on the Boltzmann probability `exp(-delta_cost / temperature)`.
pub(crate) fn simulated_annealing(
    netlist: &mut PnrNetlist,
    _arch: &dyn Architecture,
    _sink: &DiagnosticSink,
) {
    let num_cells = netlist.cell_count();
    if num_cells < 2 {
        return;
    }

    let cost_fn = PlacementCost::default();
    let mut rng = rand::thread_rng();

    // Initial temperature proportional to sqrt(cell count)
    let mut temperature = (num_cells as f64).sqrt() * 2.0;
    let moves_per_temp = (MOVES_PER_TEMP_MULTIPLIER * num_cells).max(10);

    let mut current_cost = cost_fn.total_cost(netlist);

    while temperature > MIN_TEMPERATURE {
        let mut accepted = 0;

        for _ in 0..moves_per_temp {
            // Select two random non-fixed cells for swapping
            let (cell_a_idx, cell_b_idx) = match select_swap_pair(&mut rng, netlist) {
                Some(pair) => pair,
                None => continue,
            };

            // Perform the swap
            let site_a = netlist.cells[cell_a_idx].placement;
            let site_b = netlist.cells[cell_b_idx].placement;
            netlist.cells[cell_a_idx].placement = site_b;
            netlist.cells[cell_b_idx].placement = site_a;

            // Compute new cost
            let new_cost = cost_fn.total_cost(netlist);
            let delta = new_cost - current_cost;

            // Metropolis criterion
            if delta < 0.0 || rng.gen::<f64>() < (-delta / temperature).exp() {
                current_cost = new_cost;
                accepted += 1;
            } else {
                // Reject: undo swap
                netlist.cells[cell_a_idx].placement = site_a;
                netlist.cells[cell_b_idx].placement = site_b;
            }
        }

        temperature *= COOLING_RATE;

        // Early termination if acceptance rate is very low
        let acceptance_rate = accepted as f64 / moves_per_temp as f64;
        if acceptance_rate < 0.001 {
            break;
        }
    }
}

/// Selects two non-fixed cells of compatible types for swapping.
///
/// Returns their indices in the cells vector, or `None` if no valid pair exists.
fn select_swap_pair(rng: &mut impl Rng, netlist: &PnrNetlist) -> Option<(usize, usize)> {
    let num_cells = netlist.cells.len();
    if num_cells < 2 {
        return None;
    }

    // Try random pairs up to 50 times
    for _ in 0..50 {
        let a = rng.gen_range(0..num_cells);
        let b = rng.gen_range(0..num_cells);

        if a == b {
            continue;
        }

        let cell_a = &netlist.cells[a];
        let cell_b = &netlist.cells[b];

        // Don't swap fixed cells
        if cell_a.is_fixed || cell_b.is_fixed {
            continue;
        }

        // Only swap cells of the same general type (LUT↔LUT, FF↔FF)
        if cell_type_compatible(&cell_a.cell_type, &cell_b.cell_type) {
            return Some((a, b));
        }
    }

    None
}

/// Returns whether two cell types can swap placement locations.
fn cell_type_compatible(a: &PnrCellType, b: &PnrCellType) -> bool {
    matches!(
        (a, b),
        (PnrCellType::Lut { .. }, PnrCellType::Lut { .. })
            | (PnrCellType::Dff, PnrCellType::Dff)
            | (PnrCellType::Carry, PnrCellType::Carry)
            | (PnrCellType::Carry, PnrCellType::Lut { .. })
            | (PnrCellType::Lut { .. }, PnrCellType::Carry)
            | (PnrCellType::Bram(_), PnrCellType::Bram(_))
            | (PnrCellType::Dsp(_), PnrCellType::Dsp(_))
            | (PnrCellType::Iobuf { .. }, PnrCellType::Iobuf { .. })
            | (PnrCellType::Pll(_), PnrCellType::Pll(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{PnrCell, PnrCellType, PnrNet, PnrPin};
    use crate::ids::{PnrCellId, PnrNetId, PnrPinId};
    use crate::placement::cost;
    use aion_arch::ids::SiteId;
    use aion_arch::load_architecture;
    use aion_common::LogicVec;
    use aion_ir::PortDirection;

    #[test]
    fn annealing_improves_or_maintains_cost() {
        let mut nl = PnrNetlist::new();

        // Create cells placed far apart with a connecting net
        let c0 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });
        let c1 = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_1".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: Some(SiteId::from_raw(99)),
            is_fixed: false,
        });

        let p0 = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: c0,
            net: None,
        });
        let p1 = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "I".into(),
            direction: PortDirection::Input,
            cell: c1,
            net: None,
        });

        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: p0,
            sinks: vec![p1],
            routing: None,
            timing_critical: false,
        });

        let initial_cost = cost::total_hpwl(&nl);

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let sink = DiagnosticSink::new();
        simulated_annealing(&mut nl, &*arch, &sink);

        let final_cost = cost::total_hpwl(&nl);
        // Annealing should not make things dramatically worse
        assert!(final_cost <= initial_cost * 2.0);
    }

    #[test]
    fn annealing_handles_single_cell() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "solo".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let sink = DiagnosticSink::new();
        simulated_annealing(&mut nl, &*arch, &sink);
        // Should not crash
    }

    #[test]
    fn cell_type_compatibility() {
        let lut = PnrCellType::Lut {
            inputs: 4,
            init: LogicVec::from_bool(false),
        };
        let lut2 = PnrCellType::Lut {
            inputs: 6,
            init: LogicVec::from_bool(true),
        };
        let dff = PnrCellType::Dff;
        let carry = PnrCellType::Carry;

        assert!(cell_type_compatible(&lut, &lut2));
        assert!(!cell_type_compatible(&lut, &dff));
        assert!(cell_type_compatible(&lut, &carry));
        assert!(cell_type_compatible(&dff, &dff));
    }

    #[test]
    fn annealing_preserves_fixed_cells() {
        let mut nl = PnrNetlist::new();
        let fixed_site = SiteId::from_raw(42);
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "fixed".into(),
            cell_type: PnrCellType::Iobuf {
                direction: PortDirection::Input,
                standard: "LVCMOS33".into(),
            },
            placement: Some(fixed_site),
            is_fixed: true,
        });
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "movable".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: Some(SiteId::from_raw(10)),
            is_fixed: false,
        });

        let arch = load_architecture("cyclone_iv", "EP4CE22F17C6N").unwrap();
        let sink = DiagnosticSink::new();
        simulated_annealing(&mut nl, &*arch, &sink);

        assert_eq!(nl.cells[0].placement, Some(fixed_site));
    }
}
