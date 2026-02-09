//! Opaque ID newtypes for place-and-route entities.
//!
//! [`PnrCellId`], [`PnrNetId`], and [`PnrPinId`] are thin `u32` wrappers used
//! as arena indices into the PnR netlist. They are `Copy`, `Hash`, and
//! `Serialize`/`Deserialize`.

use serde::{Deserialize, Serialize};

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
        pub struct $name(u32);

        impl $name {
            /// Creates an ID from a raw `u32` index.
            pub fn from_raw(index: u32) -> Self {
                Self(index)
            }

            /// Returns the raw `u32` index.
            pub fn as_raw(self) -> u32 {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_id!(
    /// Opaque, copyable ID for a cell in the PnR netlist.
    PnrCellId
);

define_id!(
    /// Opaque, copyable ID for a net in the PnR netlist.
    PnrNetId
);

define_id!(
    /// Opaque, copyable ID for a pin in the PnR netlist.
    PnrPinId
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn cell_id_roundtrip() {
        let id = PnrCellId::from_raw(42);
        assert_eq!(id.as_raw(), 42);
    }

    #[test]
    fn net_id_roundtrip() {
        let id = PnrNetId::from_raw(99);
        assert_eq!(id.as_raw(), 99);
    }

    #[test]
    fn pin_id_roundtrip() {
        let id = PnrPinId::from_raw(7);
        assert_eq!(id.as_raw(), 7);
    }

    #[test]
    fn id_equality() {
        let a = PnrCellId::from_raw(3);
        let b = PnrCellId::from_raw(3);
        let c = PnrCellId::from_raw(4);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn id_hash_in_set() {
        let mut set = HashSet::new();
        set.insert(PnrNetId::from_raw(1));
        set.insert(PnrNetId::from_raw(2));
        set.insert(PnrNetId::from_raw(1));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn id_serde_roundtrip() {
        let id = PnrPinId::from_raw(55);
        let json = serde_json::to_string(&id).unwrap();
        let restored: PnrPinId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    #[test]
    fn id_zero_and_max() {
        let zero = PnrCellId::from_raw(0);
        let max = PnrCellId::from_raw(u32::MAX);
        assert_ne!(zero, max);
        assert_eq!(zero.as_raw(), 0);
        assert_eq!(max.as_raw(), u32::MAX);
    }

    #[test]
    fn id_display() {
        let id = PnrNetId::from_raw(42);
        assert_eq!(format!("{id}"), "42");
    }

    #[test]
    fn id_debug_format() {
        let id = PnrCellId::from_raw(42);
        let debug = format!("{id:?}");
        assert!(debug.contains("42"));
    }
}
