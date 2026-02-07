//! Interned identifiers for cheap cloning and O(1) equality comparison.

use lasso::ThreadedRodeo;
use serde::{Deserialize, Serialize};

/// A unique identifier for any named entity in the design.
///
/// Identifiers are interned strings represented as a `u32` index into a global
/// string interner. This provides O(1) equality comparison and O(1) cloning.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Ident(u32);

impl Ident {
    /// Creates an `Ident` from a raw `u32` index.
    ///
    /// This is primarily intended for deserialization and testing.
    /// In normal use, identifiers should be created through [`Interner::get_or_intern`].
    pub fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Returns the raw `u32` index of this identifier.
    pub fn as_raw(self) -> u32 {
        self.0
    }
}

// SAFETY: `Ident` wraps a `u32` which is always a valid `usize` on 32-bit and
// 64-bit platforms. `try_from_usize` rejects values that don't fit in `u32`.
unsafe impl lasso::Key for Ident {
    fn into_usize(self) -> usize {
        self.0 as usize
    }

    fn try_from_usize(int: usize) -> Option<Self> {
        u32::try_from(int).ok().map(Ident)
    }
}

/// Thread-safe global string interner backed by [`lasso::ThreadedRodeo`].
///
/// All identifiers, module names, signal names, and file paths are interned
/// to provide O(1) equality, O(1) cloning, and string deduplication across
/// the compilation session.
pub struct Interner {
    rodeo: ThreadedRodeo<Ident>,
}

impl Interner {
    /// Creates a new empty interner.
    pub fn new() -> Self {
        Self {
            rodeo: ThreadedRodeo::new(),
        }
    }

    /// Interns a string, returning its [`Ident`]. If the string was already
    /// interned, returns the existing identifier without allocating.
    pub fn get_or_intern(&self, s: &str) -> Ident {
        self.rodeo.get_or_intern(s)
    }

    /// Resolves an [`Ident`] back to its string value.
    ///
    /// # Panics
    ///
    /// Panics if the `Ident` was not created by this interner.
    pub fn resolve(&self, ident: Ident) -> &str {
        self.rodeo.resolve(&ident)
    }
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_resolve_roundtrip() {
        let interner = Interner::new();
        let id = interner.get_or_intern("hello");
        assert_eq!(interner.resolve(id), "hello");
    }

    #[test]
    fn same_string_same_ident() {
        let interner = Interner::new();
        let a = interner.get_or_intern("world");
        let b = interner.get_or_intern("world");
        assert_eq!(a, b);
    }

    #[test]
    fn different_strings_different_idents() {
        let interner = Interner::new();
        let a = interner.get_or_intern("foo");
        let b = interner.get_or_intern("bar");
        assert_ne!(a, b);
    }

    #[test]
    fn serde_roundtrip() {
        let id = Ident(42);
        let json = serde_json::to_string(&id).unwrap();
        let back: Ident = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
