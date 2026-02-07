//! Type system for the IR, including interned types and a central type database.
//!
//! All types in the design are interned into a [`TypeDb`], which assigns each
//! unique type a [`TypeId`] for cheap comparison and storage.

use crate::ids::TypeId;
use aion_common::Ident;
use serde::{Deserialize, Serialize};

/// A hardware type in the design.
///
/// Types are language-independent — VHDL `std_logic_vector`, Verilog `wire`,
/// and SystemVerilog `logic` all map to [`Type::BitVec`] after elaboration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Type {
    /// A single bit (`std_logic`, `wire`, `logic`).
    Bit,
    /// A bit vector of known width, optionally signed.
    BitVec {
        /// The number of bits.
        width: u32,
        /// Whether the vector is signed (for arithmetic operations).
        signed: bool,
    },
    /// An integer type (for parameters and constants).
    Integer,
    /// A real/floating-point type (for parameters and simulation).
    Real,
    /// A boolean type.
    Bool,
    /// A string type (for parameters and simulation).
    Str,
    /// An array type (for memories and multi-dimensional signals).
    Array {
        /// The type of each element.
        element: TypeId,
        /// The number of elements.
        size: u32,
    },
    /// An enumeration type (for FSMs and state machines).
    Enum {
        /// The enum type name.
        name: Ident,
        /// The variant names.
        variants: Vec<Ident>,
    },
    /// A record/struct type (from VHDL records or SystemVerilog structs).
    Record {
        /// The record type name.
        name: Ident,
        /// Named fields with their types.
        fields: Vec<(Ident, TypeId)>,
    },
    /// A placeholder for types that failed resolution.
    Error,
}

/// Central type database — interned types for cheap comparison.
///
/// Each unique [`Type`] is stored once and referenced by [`TypeId`].
/// This makes type equality checks O(1) via ID comparison.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypeDb {
    types: Vec<Type>,
}

impl TypeDb {
    /// Creates a new, empty type database.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns a type, returning its [`TypeId`].
    ///
    /// If an identical type already exists, returns the existing ID.
    /// Otherwise, allocates a new entry.
    pub fn intern(&mut self, ty: Type) -> TypeId {
        // Check for existing identical type
        for (i, existing) in self.types.iter().enumerate() {
            if existing == &ty {
                return TypeId::from_raw(i as u32);
            }
        }
        let id = TypeId::from_raw(self.types.len() as u32);
        self.types.push(ty);
        id
    }

    /// Returns a reference to the type with the given ID.
    ///
    /// # Panics
    ///
    /// Panics if the ID is out of bounds.
    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id.as_raw() as usize]
    }

    /// Returns the bit width of a type, if it has a fixed width.
    ///
    /// Returns `None` for types without a fixed bit width (e.g., `Integer`, `Str`).
    pub fn bit_width(&self, id: TypeId) -> Option<u32> {
        match self.get(id) {
            Type::Bit => Some(1),
            Type::BitVec { width, .. } => Some(*width),
            Type::Bool => Some(1),
            Type::Array { element, size } => self.bit_width(*element).map(|w| w * size),
            _ => None,
        }
    }

    /// Returns the number of interned types.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Returns `true` if no types have been interned.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_bit() {
        let mut db = TypeDb::new();
        let id = db.intern(Type::Bit);
        assert_eq!(*db.get(id), Type::Bit);
    }

    #[test]
    fn intern_deduplicates() {
        let mut db = TypeDb::new();
        let a = db.intern(Type::Bit);
        let b = db.intern(Type::Bit);
        assert_eq!(a, b);
        assert_eq!(db.len(), 1);
    }

    #[test]
    fn intern_different_types() {
        let mut db = TypeDb::new();
        let bit = db.intern(Type::Bit);
        let vec8 = db.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        assert_ne!(bit, vec8);
        assert_eq!(db.len(), 2);
    }

    #[test]
    fn bit_width_bit() {
        let mut db = TypeDb::new();
        let id = db.intern(Type::Bit);
        assert_eq!(db.bit_width(id), Some(1));
    }

    #[test]
    fn bit_width_bitvec() {
        let mut db = TypeDb::new();
        let id = db.intern(Type::BitVec {
            width: 32,
            signed: true,
        });
        assert_eq!(db.bit_width(id), Some(32));
    }

    #[test]
    fn bit_width_array() {
        let mut db = TypeDb::new();
        let elem = db.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let arr = db.intern(Type::Array {
            element: elem,
            size: 4,
        });
        assert_eq!(db.bit_width(arr), Some(32));
    }

    #[test]
    fn bit_width_integer_is_none() {
        let mut db = TypeDb::new();
        let id = db.intern(Type::Integer);
        assert_eq!(db.bit_width(id), None);
    }

    #[test]
    fn bit_width_bool() {
        let mut db = TypeDb::new();
        let id = db.intern(Type::Bool);
        assert_eq!(db.bit_width(id), Some(1));
    }

    #[test]
    fn empty_db() {
        let db = TypeDb::new();
        assert!(db.is_empty());
        assert_eq!(db.len(), 0);
    }

    #[test]
    fn serde_roundtrip() {
        let mut db = TypeDb::new();
        db.intern(Type::Bit);
        db.intern(Type::BitVec {
            width: 16,
            signed: false,
        });
        let json = serde_json::to_string(&db).unwrap();
        let restored: TypeDb = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.len(), 2);
    }

    #[test]
    fn error_type() {
        let mut db = TypeDb::new();
        let id = db.intern(Type::Error);
        assert_eq!(db.bit_width(id), None);
        assert_eq!(*db.get(id), Type::Error);
    }
}
