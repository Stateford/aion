//! Simulation signal state, multi-driver resolution, and drive strength model.
//!
//! Each simulation signal has a flat [`SimSignalId`] (distinct from the hierarchical
//! `SignalId` in `aion_ir`), a current value, and zero or more [`Driver`]s. When
//! multiple drivers exist, [`resolve_drivers`] selects the strongest and detects
//! conflicts.

use aion_common::LogicVec;
use aion_ir::arena::ArenaId;
use serde::{Deserialize, Serialize};

/// Opaque ID for a flattened simulation signal.
///
/// This is distinct from `aion_ir::SignalId` because the simulator flattens
/// the module hierarchy into a single namespace.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SimSignalId(u32);

impl SimSignalId {
    /// Creates a `SimSignalId` from a raw index.
    pub fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Returns the raw index.
    pub fn as_raw(self) -> u32 {
        self.0
    }
}

impl ArenaId for SimSignalId {
    fn from_raw(index: u32) -> Self {
        Self(index)
    }

    fn as_raw(self) -> u32 {
        self.0
    }
}

/// Drive strength levels for multi-driver resolution.
///
/// Ordered from weakest to strongest. When multiple drivers conflict,
/// the strongest drive wins; equal-strength conflicts produce X.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DriveStrength {
    /// High-impedance (not driven).
    HighImpedance,
    /// Weak drive (e.g., `weak0`, `weak1`).
    Weak,
    /// Pull drive (e.g., `pull0`, `pull1`).
    Pull,
    /// Strong drive (default for most signals).
    Strong,
    /// Supply-level drive (e.g., `supply0`, `supply1`).
    Supply,
}

/// A single driver contributing a value to a signal.
///
/// Multiple drivers on the same signal are resolved by [`resolve_drivers`].
#[derive(Clone, Debug, PartialEq)]
pub struct Driver {
    /// The value this driver is asserting.
    pub value: LogicVec,
    /// The strength of this driver.
    pub strength: DriveStrength,
}

/// The full runtime state of a simulation signal.
///
/// Tracks current and previous values for edge detection,
/// all active drivers for multi-driver resolution, and metadata.
#[derive(Clone, Debug)]
pub struct SimSignalState {
    /// Current signal value.
    pub value: LogicVec,
    /// Value at the previous delta cycle (for edge detection).
    pub previous_value: LogicVec,
    /// Resolved drive strength.
    pub strength: DriveStrength,
    /// Active drivers on this signal.
    pub drivers: Vec<Driver>,
    /// Hierarchical name for display and VCD output.
    pub name: String,
    /// Bit width of this signal.
    pub width: u32,
}

impl SimSignalState {
    /// Creates a new signal state initialized to the given value.
    pub fn new(name: String, width: u32, init_value: LogicVec) -> Self {
        Self {
            previous_value: init_value.clone(),
            value: init_value,
            strength: DriveStrength::Strong,
            drivers: Vec::new(),
            name,
            width,
        }
    }

    /// Creates a new signal state initialized to all-X (unknown).
    pub fn new_unknown(name: String, width: u32) -> Self {
        let mut value = LogicVec::new(width);
        for i in 0..width {
            value.set(i, aion_common::Logic::X);
        }
        Self {
            previous_value: value.clone(),
            value,
            strength: DriveStrength::Strong,
            drivers: Vec::new(),
            name,
            width,
        }
    }
}

/// Resolves multiple drivers to a single value and strength.
///
/// Resolution rules:
/// 1. If no drivers, returns all-Z at HighImpedance strength.
/// 2. Finds the maximum drive strength among all drivers.
/// 3. Collects all drivers at that strength level.
/// 4. If exactly one driver at max strength, its value wins.
/// 5. If multiple drivers at max strength, per-bit resolution:
///    - Same bit value across all drivers → that value
///    - Conflicting bit values → X
pub fn resolve_drivers(drivers: &[Driver], width: u32) -> (LogicVec, DriveStrength) {
    if drivers.is_empty() {
        let mut z = LogicVec::new(width);
        for i in 0..width {
            z.set(i, aion_common::Logic::Z);
        }
        return (z, DriveStrength::HighImpedance);
    }

    if drivers.len() == 1 {
        return (drivers[0].value.clone(), drivers[0].strength);
    }

    // Find the maximum strength
    let max_strength = drivers.iter().map(|d| d.strength).max().unwrap();

    // Collect drivers at max strength
    let strongest: Vec<&Driver> = drivers
        .iter()
        .filter(|d| d.strength == max_strength)
        .collect();

    if strongest.len() == 1 {
        return (strongest[0].value.clone(), max_strength);
    }

    // Multiple drivers at same strength → per-bit resolution
    let mut result = LogicVec::new(width);
    for bit in 0..width {
        let first_val = strongest[0].value.get(bit);
        let all_same = strongest.iter().all(|d| d.value.get(bit) == first_val);
        if all_same {
            result.set(bit, first_val);
        } else {
            result.set(bit, aion_common::Logic::X);
        }
    }

    (result, max_strength)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{Logic, LogicVec};

    #[test]
    fn sim_signal_id_roundtrip() {
        let id = SimSignalId::from_raw(42);
        assert_eq!(id.as_raw(), 42);
    }

    #[test]
    fn sim_signal_id_arena_id() {
        let id = <SimSignalId as ArenaId>::from_raw(7);
        assert_eq!(ArenaId::as_raw(id), 7);
    }

    #[test]
    fn sim_signal_id_equality() {
        let a = SimSignalId::from_raw(1);
        let b = SimSignalId::from_raw(1);
        let c = SimSignalId::from_raw(2);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn drive_strength_ordering() {
        assert!(DriveStrength::HighImpedance < DriveStrength::Weak);
        assert!(DriveStrength::Weak < DriveStrength::Pull);
        assert!(DriveStrength::Pull < DriveStrength::Strong);
        assert!(DriveStrength::Strong < DriveStrength::Supply);
    }

    #[test]
    fn signal_state_new() {
        let v = LogicVec::from_u64(5, 4);
        let s = SimSignalState::new("top.clk".into(), 4, v.clone());
        assert_eq!(s.value, v);
        assert_eq!(s.previous_value, v);
        assert_eq!(s.width, 4);
        assert_eq!(s.name, "top.clk");
    }

    #[test]
    fn signal_state_new_unknown() {
        let s = SimSignalState::new_unknown("top.reg".into(), 8);
        assert_eq!(s.width, 8);
        for i in 0..8 {
            assert_eq!(s.value.get(i), Logic::X);
        }
    }

    #[test]
    fn resolve_no_drivers() {
        let (val, str) = resolve_drivers(&[], 4);
        assert_eq!(str, DriveStrength::HighImpedance);
        for i in 0..4 {
            assert_eq!(val.get(i), Logic::Z);
        }
    }

    #[test]
    fn resolve_single_driver() {
        let d = Driver {
            value: LogicVec::from_u64(0b1010, 4),
            strength: DriveStrength::Strong,
        };
        let (val, str) = resolve_drivers(&[d], 4);
        assert_eq!(str, DriveStrength::Strong);
        assert_eq!(val, LogicVec::from_u64(0b1010, 4));
    }

    #[test]
    fn resolve_stronger_wins() {
        let weak = Driver {
            value: LogicVec::from_u64(0b0000, 4),
            strength: DriveStrength::Weak,
        };
        let strong = Driver {
            value: LogicVec::from_u64(0b1111, 4),
            strength: DriveStrength::Strong,
        };
        let (val, str) = resolve_drivers(&[weak, strong], 4);
        assert_eq!(str, DriveStrength::Strong);
        assert_eq!(val, LogicVec::from_u64(0b1111, 4));
    }

    #[test]
    fn resolve_same_strength_same_value() {
        let a = Driver {
            value: LogicVec::from_u64(0b1010, 4),
            strength: DriveStrength::Strong,
        };
        let b = Driver {
            value: LogicVec::from_u64(0b1010, 4),
            strength: DriveStrength::Strong,
        };
        let (val, str) = resolve_drivers(&[a, b], 4);
        assert_eq!(str, DriveStrength::Strong);
        assert_eq!(val, LogicVec::from_u64(0b1010, 4));
    }

    #[test]
    fn resolve_same_strength_conflict_produces_x() {
        let a = Driver {
            value: LogicVec::from_u64(0b1100, 4),
            strength: DriveStrength::Strong,
        };
        let b = Driver {
            value: LogicVec::from_u64(0b1010, 4),
            strength: DriveStrength::Strong,
        };
        let (val, str) = resolve_drivers(&[a, b], 4);
        assert_eq!(str, DriveStrength::Strong);
        // Bits 3,2: 1,1 vs 1,0 → 1,X; bits 1,0: 0,0 vs 1,0 → X,0
        assert_eq!(val.get(0), Logic::Zero); // both 0
        assert_eq!(val.get(1), Logic::X); // 0 vs 1
        assert_eq!(val.get(2), Logic::X); // 1 vs 0
        assert_eq!(val.get(3), Logic::One); // both 1
    }

    #[test]
    fn resolve_z_driver_weakest() {
        let z_driver = Driver {
            value: LogicVec::new(4), // all zero, but at Z strength
            strength: DriveStrength::HighImpedance,
        };
        let strong = Driver {
            value: LogicVec::from_u64(0b1111, 4),
            strength: DriveStrength::Strong,
        };
        let (val, str) = resolve_drivers(&[z_driver, strong], 4);
        assert_eq!(str, DriveStrength::Strong);
        assert_eq!(val, LogicVec::from_u64(0b1111, 4));
    }

    #[test]
    fn serde_roundtrip_signal_id() {
        let id = SimSignalId::from_raw(99);
        let json = serde_json::to_string(&id).unwrap();
        let back: SimSignalId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn serde_roundtrip_strength() {
        let s = DriveStrength::Pull;
        let json = serde_json::to_string(&s).unwrap();
        let back: DriveStrength = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
