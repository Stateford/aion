//! Opaque ID newtypes for timing graph entities.
//!
//! [`TimingNodeId`] and [`TimingEdgeId`] are thin `u32` wrappers used as arena
//! indices into the timing graph. They are `Copy`, `Hash`, and `Serialize`/`Deserialize`.

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
    /// Opaque, copyable ID for a node in the timing graph.
    TimingNodeId
);

define_id!(
    /// Opaque, copyable ID for an edge in the timing graph.
    TimingEdgeId
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn node_id_roundtrip() {
        let id = TimingNodeId::from_raw(42);
        assert_eq!(id.as_raw(), 42);
    }

    #[test]
    fn edge_id_roundtrip() {
        let id = TimingEdgeId::from_raw(99);
        assert_eq!(id.as_raw(), 99);
    }

    #[test]
    fn node_id_equality() {
        let a = TimingNodeId::from_raw(7);
        let b = TimingNodeId::from_raw(7);
        let c = TimingNodeId::from_raw(8);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn node_id_hash_in_set() {
        let mut set = HashSet::new();
        set.insert(TimingNodeId::from_raw(1));
        set.insert(TimingNodeId::from_raw(2));
        set.insert(TimingNodeId::from_raw(1));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn node_id_serde_roundtrip() {
        let id = TimingNodeId::from_raw(99);
        let json = serde_json::to_string(&id).unwrap();
        let restored: TimingNodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    #[test]
    fn edge_id_serde_roundtrip() {
        let id = TimingEdgeId::from_raw(55);
        let json = serde_json::to_string(&id).unwrap();
        let restored: TimingEdgeId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    #[test]
    fn id_zero_and_max() {
        let zero = TimingNodeId::from_raw(0);
        let max = TimingNodeId::from_raw(u32::MAX);
        assert_eq!(zero.as_raw(), 0);
        assert_eq!(max.as_raw(), u32::MAX);
        assert_ne!(zero, max);
    }

    #[test]
    fn id_debug_format() {
        let id = TimingEdgeId::from_raw(42);
        let debug = format!("{id:?}");
        assert!(debug.contains("42"));
    }
}
