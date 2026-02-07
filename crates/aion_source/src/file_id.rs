//! Opaque identifier for source files loaded into a compilation session.

use serde::{Deserialize, Serialize};

/// Opaque identifier for a source file loaded into the [`SourceDb`](crate::SourceDb).
///
/// Each source file gets a unique `FileId` when loaded. These IDs are used
/// in [`Span`](crate::Span) to associate byte ranges with their source file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FileId(u32);

impl FileId {
    /// A dummy file ID used for synthetic spans (e.g., compiler-generated code).
    pub const DUMMY: FileId = FileId(u32::MAX);

    /// Creates a `FileId` from a raw `u32` value.
    pub fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw `u32` value of this `FileId`.
    pub fn as_raw(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_as_raw_roundtrip() {
        let id = FileId::from_raw(42);
        assert_eq!(id.as_raw(), 42);
    }

    #[test]
    fn dummy_differs_from_normal() {
        let normal = FileId::from_raw(0);
        assert_ne!(FileId::DUMMY, normal);
        assert_eq!(FileId::DUMMY.as_raw(), u32::MAX);
    }

    #[test]
    fn serde_roundtrip() {
        let id = FileId::from_raw(7);
        let json = serde_json::to_string(&id).unwrap();
        let back: FileId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
