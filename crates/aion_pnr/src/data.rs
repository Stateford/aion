//! Core PnR netlist data structures.
//!
//! Defines the physical netlist used during placement and routing: cells
//! (with optional placement), nets (driver + sinks), and pins (cell
//! connections to nets). The [`PnrNetlist`] is the central data structure
//! that flows through the entire place-and-route pipeline.

use crate::ids::{PnrCellId, PnrNetId, PnrPinId};
use crate::route_tree::RouteTree;
use aion_arch::ids::SiteId;
use aion_common::LogicVec;
use aion_ir::PortDirection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The physical netlist for place and route.
///
/// Contains all cells, nets, and pins in the design after technology mapping.
/// Each cell has an optional placement (site assignment), and each net has
/// an optional routing solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnrNetlist {
    /// All cells in the netlist.
    pub cells: Vec<PnrCell>,
    /// All nets in the netlist.
    pub nets: Vec<PnrNet>,
    /// All pins in the netlist.
    pub pins: Vec<PnrPin>,
    /// Auxiliary index: cell name to ID (rebuilt on deserialization).
    #[serde(skip)]
    pub cell_by_name: HashMap<String, PnrCellId>,
    /// Auxiliary index: net name to ID (rebuilt on deserialization).
    #[serde(skip)]
    pub net_by_name: HashMap<String, PnrNetId>,
}

impl PnrNetlist {
    /// Creates an empty PnR netlist.
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            nets: Vec::new(),
            pins: Vec::new(),
            cell_by_name: HashMap::new(),
            net_by_name: HashMap::new(),
        }
    }

    /// Adds a cell and returns its ID.
    pub fn add_cell(&mut self, mut cell: PnrCell) -> PnrCellId {
        let id = PnrCellId::from_raw(self.cells.len() as u32);
        cell.id = id;
        self.cell_by_name.insert(cell.name.clone(), id);
        self.cells.push(cell);
        id
    }

    /// Adds a net and returns its ID.
    pub fn add_net(&mut self, mut net: PnrNet) -> PnrNetId {
        let id = PnrNetId::from_raw(self.nets.len() as u32);
        net.id = id;
        self.net_by_name.insert(net.name.clone(), id);
        self.nets.push(net);
        id
    }

    /// Adds a pin and returns its ID.
    pub fn add_pin(&mut self, mut pin: PnrPin) -> PnrPinId {
        let id = PnrPinId::from_raw(self.pins.len() as u32);
        pin.id = id;
        self.pins.push(pin);
        id
    }

    /// Returns the cell with the given ID.
    pub fn cell(&self, id: PnrCellId) -> &PnrCell {
        &self.cells[id.as_raw() as usize]
    }

    /// Returns a mutable reference to the cell with the given ID.
    pub fn cell_mut(&mut self, id: PnrCellId) -> &mut PnrCell {
        &mut self.cells[id.as_raw() as usize]
    }

    /// Returns the net with the given ID.
    pub fn net(&self, id: PnrNetId) -> &PnrNet {
        &self.nets[id.as_raw() as usize]
    }

    /// Returns a mutable reference to the net with the given ID.
    pub fn net_mut(&mut self, id: PnrNetId) -> &mut PnrNet {
        &mut self.nets[id.as_raw() as usize]
    }

    /// Returns the pin with the given ID.
    pub fn pin(&self, id: PnrPinId) -> &PnrPin {
        &self.pins[id.as_raw() as usize]
    }

    /// Returns the number of cells.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Returns the number of nets.
    pub fn net_count(&self) -> usize {
        self.nets.len()
    }

    /// Returns the number of pins.
    pub fn pin_count(&self) -> usize {
        self.pins.len()
    }

    /// Rebuilds auxiliary indices after deserialization.
    pub fn rebuild_indices(&mut self) {
        self.cell_by_name.clear();
        for (i, cell) in self.cells.iter().enumerate() {
            self.cell_by_name
                .insert(cell.name.clone(), PnrCellId::from_raw(i as u32));
        }
        self.net_by_name.clear();
        for (i, net) in self.nets.iter().enumerate() {
            self.net_by_name
                .insert(net.name.clone(), PnrNetId::from_raw(i as u32));
        }
    }

    /// Returns whether all cells have been placed.
    pub fn is_fully_placed(&self) -> bool {
        self.cells.iter().all(|c| c.placement.is_some())
    }

    /// Returns whether all nets have been routed.
    pub fn is_fully_routed(&self) -> bool {
        self.nets.iter().all(|n| n.routing.is_some())
    }

    /// Returns the number of placed cells.
    pub fn placed_count(&self) -> usize {
        self.cells.iter().filter(|c| c.placement.is_some()).count()
    }

    /// Returns the number of routed nets.
    pub fn routed_count(&self) -> usize {
        self.nets.iter().filter(|n| n.routing.is_some()).count()
    }
}

impl Default for PnrNetlist {
    fn default() -> Self {
        Self::new()
    }
}

/// The type of a PnR cell, determining what physical resource it maps to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PnrCellType {
    /// A look-up table with the given number of inputs and initialization vector.
    Lut {
        /// Number of inputs to the LUT (typically 4 or 6).
        inputs: u8,
        /// LUT initialization bits (truth table).
        init: LogicVec,
    },
    /// A D flip-flop (edge-triggered register).
    Dff,
    /// A carry chain cell for arithmetic operations.
    Carry,
    /// A block RAM configured with the given parameters.
    Bram(BramConfig),
    /// A DSP block configured with the given parameters.
    Dsp(DspConfig),
    /// An I/O buffer connecting to a package pin.
    Iobuf {
        /// Direction of the I/O buffer.
        direction: PortDirection,
        /// I/O standard (e.g., "LVCMOS33", "LVDS").
        standard: String,
    },
    /// A PLL/MMCM clock management block.
    Pll(PllConfig),
}

/// Configuration for a block RAM cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BramConfig {
    /// Data width in bits.
    pub width: u32,
    /// Memory depth (number of entries).
    pub depth: u32,
}

/// Configuration for a DSP block cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DspConfig {
    /// Width of the A operand in bits.
    pub width_a: u32,
    /// Width of the B operand in bits.
    pub width_b: u32,
}

/// Configuration for a PLL/MMCM clock management cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PllConfig {
    /// Input frequency in MHz.
    pub input_freq_mhz: f64,
    /// Output frequency in MHz.
    pub output_freq_mhz: f64,
}

/// A cell in the PnR netlist.
///
/// Represents a single physical resource (LUT, FF, BRAM, DSP, I/O) that needs
/// to be placed onto a device site and connected via routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnrCell {
    /// The unique ID of this cell.
    pub id: PnrCellId,
    /// Human-readable cell name (e.g., "lut_0", "ff_clk_d").
    pub name: String,
    /// The physical cell type.
    pub cell_type: PnrCellType,
    /// The site this cell is placed on (`None` = unplaced).
    pub placement: Option<SiteId>,
    /// Whether this cell's placement is fixed (e.g., I/O pads).
    pub is_fixed: bool,
}

/// A net in the PnR netlist.
///
/// Represents a signal connecting one driver pin to one or more sink pins.
/// After routing, the net has a [`RouteTree`] describing the physical wiring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnrNet {
    /// The unique ID of this net.
    pub id: PnrNetId,
    /// Human-readable net name (e.g., "clk", "data_bus[3]").
    pub name: String,
    /// The driver pin (source) of this net.
    pub driver: PnrPinId,
    /// The sink pins (destinations) of this net.
    pub sinks: Vec<PnrPinId>,
    /// The routing solution for this net (`None` = unrouted).
    pub routing: Option<RouteTree>,
    /// Whether this net is on the critical timing path.
    pub timing_critical: bool,
}

/// A pin on a cell in the PnR netlist.
///
/// Pins connect cells to nets. Each pin belongs to exactly one cell and
/// is optionally connected to one net.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnrPin {
    /// The unique ID of this pin.
    pub id: PnrPinId,
    /// Human-readable pin name (e.g., "I0", "O", "D", "Q").
    pub name: String,
    /// Direction of the pin relative to the cell.
    pub direction: PortDirection,
    /// The cell that owns this pin.
    pub cell: PnrCellId,
    /// The net this pin is connected to (`None` = unconnected).
    pub net: Option<PnrNetId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_netlist() {
        let nl = PnrNetlist::new();
        assert_eq!(nl.cell_count(), 0);
        assert_eq!(nl.net_count(), 0);
        assert_eq!(nl.pin_count(), 0);
        assert!(nl.is_fully_placed());
        assert!(nl.is_fully_routed());
    }

    #[test]
    fn add_cell() {
        let mut nl = PnrNetlist::new();
        let id = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: None,
            is_fixed: false,
        });
        assert_eq!(nl.cell_count(), 1);
        assert_eq!(nl.cell(id).name, "lut_0");
        assert!(nl.cell_by_name.contains_key("lut_0"));
    }

    #[test]
    fn add_net_and_pin() {
        let mut nl = PnrNetlist::new();
        let cell_id = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(false),
            },
            placement: None,
            is_fixed: false,
        });

        let pin_id = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: cell_id,
            net: None,
        });

        let net_id = nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: pin_id,
            sinks: vec![],
            routing: None,
            timing_critical: false,
        });

        assert_eq!(nl.net_count(), 1);
        assert_eq!(nl.pin_count(), 1);
        assert_eq!(nl.net(net_id).driver, pin_id);
        assert_eq!(nl.pin(pin_id).cell, cell_id);
    }

    #[test]
    fn placement_tracking() {
        let mut nl = PnrNetlist::new();
        let id = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Dff,
            placement: None,
            is_fixed: false,
        });
        assert!(!nl.is_fully_placed());
        assert_eq!(nl.placed_count(), 0);

        nl.cell_mut(id).placement = Some(SiteId::from_raw(5));
        assert!(nl.is_fully_placed());
        assert_eq!(nl.placed_count(), 1);
    }

    #[test]
    fn routing_tracking() {
        let mut nl = PnrNetlist::new();
        let pin = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: PnrCellId::from_raw(0),
            net: None,
        });
        let net_id = nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: pin,
            sinks: vec![],
            routing: None,
            timing_critical: false,
        });
        assert!(!nl.is_fully_routed());
        assert_eq!(nl.routed_count(), 0);

        nl.net_mut(net_id).routing = Some(RouteTree::stub());
        assert!(nl.is_fully_routed());
        assert_eq!(nl.routed_count(), 1);
    }

    #[test]
    fn cell_types() {
        let _lut = PnrCellType::Lut {
            inputs: 6,
            init: LogicVec::all_zero(64),
        };
        let _dff = PnrCellType::Dff;
        let _carry = PnrCellType::Carry;
        let _bram = PnrCellType::Bram(BramConfig {
            width: 18,
            depth: 1024,
        });
        let _dsp = PnrCellType::Dsp(DspConfig {
            width_a: 18,
            width_b: 18,
        });
        let _io = PnrCellType::Iobuf {
            direction: PortDirection::Input,
            standard: "LVCMOS33".into(),
        };
        let _pll = PnrCellType::Pll(PllConfig {
            input_freq_mhz: 50.0,
            output_freq_mhz: 100.0,
        });
    }

    #[test]
    fn rebuild_indices() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "cell_a".into(),
            cell_type: PnrCellType::Dff,
            placement: None,
            is_fixed: false,
        });
        let pin = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: PnrCellId::from_raw(0),
            net: None,
        });
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_a".into(),
            driver: pin,
            sinks: vec![],
            routing: None,
            timing_critical: false,
        });

        // Clear indices
        nl.cell_by_name.clear();
        nl.net_by_name.clear();
        assert!(!nl.cell_by_name.contains_key("cell_a"));

        // Rebuild
        nl.rebuild_indices();
        assert!(nl.cell_by_name.contains_key("cell_a"));
        assert!(nl.net_by_name.contains_key("net_a"));
    }

    #[test]
    fn serde_roundtrip() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 4,
                init: LogicVec::from_bool(true),
            },
            placement: Some(SiteId::from_raw(3)),
            is_fixed: false,
        });
        let pin = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: PnrCellId::from_raw(0),
            net: None,
        });
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: pin,
            sinks: vec![],
            routing: None,
            timing_critical: true,
        });

        let json = serde_json::to_string(&nl).unwrap();
        let mut restored: PnrNetlist = serde_json::from_str(&json).unwrap();
        restored.rebuild_indices();

        assert_eq!(restored.cell_count(), 1);
        assert_eq!(restored.net_count(), 1);
        assert!(restored.cell_by_name.contains_key("lut_0"));
        assert!(restored.net_by_name.contains_key("net_0"));
    }

    #[test]
    fn default_netlist() {
        let nl = PnrNetlist::default();
        assert_eq!(nl.cell_count(), 0);
    }

    #[test]
    fn fixed_cell() {
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "io_pad".into(),
            cell_type: PnrCellType::Iobuf {
                direction: PortDirection::Input,
                standard: "LVCMOS33".into(),
            },
            placement: Some(SiteId::from_raw(0)),
            is_fixed: true,
        });
        assert!(nl.cell(PnrCellId::from_raw(0)).is_fixed);
    }
}
