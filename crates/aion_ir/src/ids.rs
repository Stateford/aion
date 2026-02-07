//! Opaque ID newtypes for all IR entities.
//!
//! Each ID is a thin `u32` wrapper that is `Copy`, `Hash`, and `Serialize`/`Deserialize`.
//! IDs are created by [`Arena::alloc`](crate::arena::Arena::alloc) and used for O(1) lookup.

use crate::arena::ArenaId;
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

        impl ArenaId for $name {
            fn from_raw(index: u32) -> Self {
                Self(index)
            }

            fn as_raw(self) -> u32 {
                self.0
            }
        }
    };
}

define_id!(
    /// Opaque, copyable ID for a module in the design.
    ModuleId
);

define_id!(
    /// Opaque, copyable ID for a signal within a module.
    SignalId
);

define_id!(
    /// Opaque, copyable ID for a cell (primitive or instantiation) within a module.
    CellId
);

define_id!(
    /// Opaque, copyable ID for a process/always block within a module.
    ProcessId
);

define_id!(
    /// Opaque, copyable ID for a port on a module.
    PortId
);

define_id!(
    /// Opaque, copyable ID for an interned type in the [`TypeDb`](crate::types::TypeDb).
    TypeId
);

define_id!(
    /// Opaque, copyable ID for a clock domain.
    ClockDomainId
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn id_roundtrip() {
        let id = ModuleId::from_raw(42);
        assert_eq!(id.as_raw(), 42);
    }

    #[test]
    fn id_equality() {
        let a = SignalId::from_raw(7);
        let b = SignalId::from_raw(7);
        let c = SignalId::from_raw(8);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn id_hash_in_set() {
        let mut set = HashSet::new();
        set.insert(CellId::from_raw(1));
        set.insert(CellId::from_raw(2));
        set.insert(CellId::from_raw(1));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn id_serde_roundtrip() {
        let id = ProcessId::from_raw(99);
        let json = serde_json::to_string(&id).unwrap();
        let restored: ProcessId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    #[test]
    fn all_id_types_construct() {
        let _ = ModuleId::from_raw(0);
        let _ = SignalId::from_raw(0);
        let _ = CellId::from_raw(0);
        let _ = ProcessId::from_raw(0);
        let _ = PortId::from_raw(0);
        let _ = TypeId::from_raw(0);
        let _ = ClockDomainId::from_raw(0);
    }
}
