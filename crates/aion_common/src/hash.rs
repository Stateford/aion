//! Content hashing for cache invalidation and incremental compilation.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A 128-bit content hash computed using XXH3 for cache invalidation.
///
/// Two files with the same `ContentHash` are assumed to have identical content.
/// Used throughout the toolchain to detect when source files or intermediate
/// artifacts have changed and need recompilation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash([u8; 16]);

impl ContentHash {
    /// Computes a content hash from a byte slice using XXH3-128.
    pub fn from_bytes(data: &[u8]) -> Self {
        let hash = xxhash_rust::xxh3::xxh3_128(data);
        Self(hash.to_le_bytes())
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({:02x}{:02x}..)", self.0[0], self.0[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let a = ContentHash::from_bytes(b"hello world");
        let b = ContentHash::from_bytes(b"hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_differ() {
        let a = ContentHash::from_bytes(b"hello");
        let b = ContentHash::from_bytes(b"world");
        assert_ne!(a, b);
    }

    #[test]
    fn display_format() {
        let h = ContentHash::from_bytes(b"test");
        let s = format!("{h}");
        assert_eq!(s.len(), 32, "Display should be 32 hex chars");
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn debug_abbreviated() {
        let h = ContentHash::from_bytes(b"test");
        let s = format!("{h:?}");
        assert!(s.starts_with("ContentHash("));
        assert!(s.ends_with(")"));
    }

    #[test]
    fn serde_roundtrip() {
        let h = ContentHash::from_bytes(b"serde test");
        let json = serde_json::to_string(&h).unwrap();
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }
}
