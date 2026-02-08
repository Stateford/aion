//! Packed vectors of 4-state logic values for efficient signal representation.

use crate::logic::Logic;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{BitAnd, BitOr, BitXor, Not};

/// A vector of 4-state [`Logic`] values packed for efficient storage.
///
/// Each logic value occupies 2 bits (encoding 4 states), with 32 values packed
/// per `u64` word. This representation is used for signal values in simulation
/// and synthesis.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogicVec {
    width: u32,
    /// Packed storage: 2 bits per logic value, 32 values per u64.
    data: Vec<u64>,
}

/// Number of logic values packed per u64 word.
const VALUES_PER_WORD: u32 = 32;

impl LogicVec {
    /// Creates a new `LogicVec` of the given width, initialized to all `Zero`.
    pub fn new(width: u32) -> Self {
        let num_words = word_count(width);
        Self {
            width,
            data: vec![0; num_words],
        }
    }

    /// Returns the number of logic values in this vector.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Gets the logic value at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index >= self.width()`.
    pub fn get(&self, index: u32) -> Logic {
        assert!(
            index < self.width,
            "index {index} out of bounds for width {}",
            self.width
        );
        let word_idx = (index / VALUES_PER_WORD) as usize;
        let bit_offset = (index % VALUES_PER_WORD) * 2;
        let bits = (self.data[word_idx] >> bit_offset) & 0b11;
        match bits {
            0 => Logic::Zero,
            1 => Logic::One,
            2 => Logic::X,
            3 => Logic::Z,
            _ => unreachable!(),
        }
    }

    /// Sets the logic value at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index >= self.width()`.
    pub fn set(&mut self, index: u32, value: Logic) {
        assert!(
            index < self.width,
            "index {index} out of bounds for width {}",
            self.width
        );
        let word_idx = (index / VALUES_PER_WORD) as usize;
        let bit_offset = (index % VALUES_PER_WORD) * 2;
        let mask = !(0b11u64 << bit_offset);
        self.data[word_idx] = (self.data[word_idx] & mask) | ((value as u64) << bit_offset);
    }

    /// Creates a `LogicVec` with all bits set to `Zero`.
    pub fn all_zero(width: u32) -> Self {
        Self::new(width)
    }

    /// Creates a `LogicVec` with all bits set to `One`.
    pub fn all_one(width: u32) -> Self {
        let mut v = Self::new(width);
        for i in 0..width {
            v.set(i, Logic::One);
        }
        v
    }

    /// Creates a single-bit `LogicVec` from a boolean value.
    pub fn from_bool(value: bool) -> Self {
        let mut v = Self::new(1);
        if value {
            v.set(0, Logic::One);
        }
        v
    }

    /// Creates a `LogicVec` from a `u64` value with the given width.
    ///
    /// Bits beyond the given width are ignored.
    pub fn from_u64(value: u64, width: u32) -> Self {
        let mut v = Self::new(width);
        for i in 0..width.min(64) {
            if (value >> i) & 1 != 0 {
                v.set(i, Logic::One);
            }
        }
        v
    }

    /// Converts the `LogicVec` to a `u64`, if all bits are definite (0 or 1).
    ///
    /// Returns `None` if the vector contains X or Z values, or if the width
    /// exceeds 64 bits.
    pub fn to_u64(&self) -> Option<u64> {
        if self.width > 64 {
            return None;
        }
        let mut result = 0u64;
        for i in 0..self.width {
            match self.get(i) {
                Logic::Zero => {}
                Logic::One => result |= 1 << i,
                Logic::X | Logic::Z => return None,
            }
        }
        Some(result)
    }

    /// Returns true if all bits are `Logic::Zero`.
    pub fn is_all_zero(&self) -> bool {
        (0..self.width).all(|i| self.get(i) == Logic::Zero)
    }

    /// Returns true if all bits are `Logic::One`.
    pub fn is_all_one(&self) -> bool {
        (0..self.width).all(|i| self.get(i) == Logic::One)
    }

    /// Parses a binary string like `"10XZ"` into a `LogicVec`.
    ///
    /// The leftmost character is the most significant bit (highest index).
    /// Returns `None` if the string contains invalid characters.
    pub fn from_binary_str(s: &str) -> Option<Self> {
        let width = s.len() as u32;
        let mut v = Self::new(width);
        for (i, c) in s.chars().rev().enumerate() {
            let val = Logic::from_char(c)?;
            v.set(i as u32, val);
        }
        Some(v)
    }

    /// Parses a hex string into a `LogicVec`.
    ///
    /// Each hex digit represents 4 bits. Returns `None` if the string
    /// contains invalid hex characters.
    pub fn from_hex_str(s: &str) -> Option<Self> {
        let width = (s.len() as u32) * 4;
        let mut v = Self::new(width);
        for (hex_idx, c) in s.chars().rev().enumerate() {
            let nibble = c.to_digit(16)? as u8;
            for bit in 0..4 {
                let val = if nibble & (1 << bit) != 0 {
                    Logic::One
                } else {
                    Logic::Zero
                };
                v.set((hex_idx as u32) * 4 + bit, val);
            }
        }
        Some(v)
    }
}

impl fmt::Display for LogicVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for i in (0..self.width).rev() {
            write!(f, "{}", self.get(i))?;
        }
        Ok(())
    }
}

impl fmt::Debug for LogicVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LogicVec({self})")
    }
}

impl BitAnd for &LogicVec {
    type Output = LogicVec;

    fn bitand(self, rhs: Self) -> LogicVec {
        assert_eq!(self.width, rhs.width, "LogicVec width mismatch in AND");
        let mut result = LogicVec::new(self.width);
        for i in 0..self.width {
            result.set(i, self.get(i) & rhs.get(i));
        }
        result
    }
}

impl BitOr for &LogicVec {
    type Output = LogicVec;

    fn bitor(self, rhs: Self) -> LogicVec {
        assert_eq!(self.width, rhs.width, "LogicVec width mismatch in OR");
        let mut result = LogicVec::new(self.width);
        for i in 0..self.width {
            result.set(i, self.get(i) | rhs.get(i));
        }
        result
    }
}

impl BitXor for &LogicVec {
    type Output = LogicVec;

    fn bitxor(self, rhs: Self) -> LogicVec {
        assert_eq!(self.width, rhs.width, "LogicVec width mismatch in XOR");
        let mut result = LogicVec::new(self.width);
        for i in 0..self.width {
            result.set(i, self.get(i) ^ rhs.get(i));
        }
        result
    }
}

impl Not for &LogicVec {
    type Output = LogicVec;

    fn not(self) -> LogicVec {
        let mut result = LogicVec::new(self.width);
        for i in 0..self.width {
            result.set(i, !self.get(i));
        }
        result
    }
}

/// Returns the number of u64 words needed to store `width` logic values.
fn word_count(width: u32) -> usize {
    width.div_ceil(VALUES_PER_WORD) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_width() {
        let v = LogicVec::new(8);
        assert_eq!(v.width(), 8);
    }

    #[test]
    fn set_get_roundtrip() {
        let mut v = LogicVec::new(4);
        v.set(0, Logic::Zero);
        v.set(1, Logic::One);
        v.set(2, Logic::X);
        v.set(3, Logic::Z);
        assert_eq!(v.get(0), Logic::Zero);
        assert_eq!(v.get(1), Logic::One);
        assert_eq!(v.get(2), Logic::X);
        assert_eq!(v.get(3), Logic::Z);
    }

    #[test]
    fn new_initializes_to_zero() {
        let v = LogicVec::new(64);
        for i in 0..64 {
            assert_eq!(v.get(i), Logic::Zero);
        }
    }

    #[test]
    fn from_binary_str() {
        let v = LogicVec::from_binary_str("10XZ").unwrap();
        assert_eq!(v.width(), 4);
        assert_eq!(v.get(3), Logic::One); // MSB
        assert_eq!(v.get(2), Logic::Zero);
        assert_eq!(v.get(1), Logic::X);
        assert_eq!(v.get(0), Logic::Z); // LSB
    }

    #[test]
    fn from_binary_str_invalid() {
        assert!(LogicVec::from_binary_str("10A1").is_none());
    }

    #[test]
    fn from_hex_str() {
        let v = LogicVec::from_hex_str("FF").unwrap();
        assert_eq!(v.width(), 8);
        for i in 0..8 {
            assert_eq!(v.get(i), Logic::One);
        }
    }

    #[test]
    fn from_hex_str_a5() {
        let v = LogicVec::from_hex_str("A5").unwrap();
        assert_eq!(v.width(), 8);
        // 0xA5 = 1010_0101
        assert_eq!(format!("{v}"), "10100101");
    }

    #[test]
    fn from_hex_str_invalid() {
        assert!(LogicVec::from_hex_str("GG").is_none());
    }

    #[test]
    fn bitwise_and() {
        let a = LogicVec::from_binary_str("1100").unwrap();
        let b = LogicVec::from_binary_str("1010").unwrap();
        let r = &a & &b;
        assert_eq!(format!("{r}"), "1000");
    }

    #[test]
    fn bitwise_or() {
        let a = LogicVec::from_binary_str("1100").unwrap();
        let b = LogicVec::from_binary_str("1010").unwrap();
        let r = &a | &b;
        assert_eq!(format!("{r}"), "1110");
    }

    #[test]
    fn bitwise_xor() {
        let a = LogicVec::from_binary_str("1100").unwrap();
        let b = LogicVec::from_binary_str("1010").unwrap();
        let r = &a ^ &b;
        assert_eq!(format!("{r}"), "0110");
    }

    #[test]
    fn bitwise_not() {
        let a = LogicVec::from_binary_str("10XZ").unwrap();
        let r = !&a;
        assert_eq!(format!("{r}"), "01XX");
    }

    #[test]
    fn display() {
        let v = LogicVec::from_binary_str("10XZ").unwrap();
        assert_eq!(format!("{v}"), "10XZ");
    }

    #[test]
    fn all_zero_all_one() {
        let z = LogicVec::all_zero(4);
        assert_eq!(format!("{z}"), "0000");
        let o = LogicVec::all_one(4);
        assert_eq!(format!("{o}"), "1111");
    }

    #[test]
    fn large_width_spanning_words() {
        let mut v = LogicVec::new(100);
        v.set(0, Logic::One);
        v.set(50, Logic::X);
        v.set(99, Logic::Z);
        assert_eq!(v.get(0), Logic::One);
        assert_eq!(v.get(50), Logic::X);
        assert_eq!(v.get(99), Logic::Z);
        assert_eq!(v.get(1), Logic::Zero);
    }

    #[test]
    fn serde_roundtrip() {
        let v = LogicVec::from_binary_str("10XZ1010").unwrap();
        let json = serde_json::to_string(&v).unwrap();
        let back: LogicVec = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }
}
