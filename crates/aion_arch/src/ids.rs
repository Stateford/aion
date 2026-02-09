//! Opaque ID newtypes for FPGA architecture entities.
//!
//! Each ID is a thin `u32` wrapper that is `Copy`, `Hash`, and `Serialize`/`Deserialize`.
//! These IDs reference tiles, sites, BELs, wires, and PIPs within a device model.

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
    };
}

define_id!(
    /// Opaque, copyable ID for a site (placement location) in the device grid.
    SiteId
);

define_id!(
    /// Opaque, copyable ID for a BEL (basic element of logic) within a site.
    BelId
);

define_id!(
    /// Opaque, copyable ID for a routing wire in the device fabric.
    WireId
);

define_id!(
    /// Opaque, copyable ID for a programmable interconnect point (PIP) connecting wires.
    PipId
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn id_roundtrip() {
        let id = SiteId::from_raw(42);
        assert_eq!(id.as_raw(), 42);
    }

    #[test]
    fn id_equality() {
        let a = BelId::from_raw(7);
        let b = BelId::from_raw(7);
        let c = BelId::from_raw(8);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn id_hash_in_set() {
        let mut set = HashSet::new();
        set.insert(WireId::from_raw(1));
        set.insert(WireId::from_raw(2));
        set.insert(WireId::from_raw(1));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn id_serde_roundtrip() {
        let id = PipId::from_raw(99);
        let json = serde_json::to_string(&id).unwrap();
        let restored: PipId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    #[test]
    fn all_id_types_construct() {
        let _ = SiteId::from_raw(0);
        let _ = BelId::from_raw(0);
        let _ = WireId::from_raw(0);
        let _ = PipId::from_raw(0);
    }

    #[test]
    fn id_zero_and_max() {
        let zero = SiteId::from_raw(0);
        let max = SiteId::from_raw(u32::MAX);
        assert_eq!(zero.as_raw(), 0);
        assert_eq!(max.as_raw(), u32::MAX);
        assert_ne!(zero, max);
    }

    #[test]
    fn id_debug_format() {
        let id = BelId::from_raw(42);
        let debug = format!("{id:?}");
        assert!(debug.contains("42"));
    }
}
