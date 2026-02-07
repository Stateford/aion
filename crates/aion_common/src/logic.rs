//! IEEE 1164 four-state logic values with truth-table-based operators.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{BitAnd, BitOr, BitXor, Not};

/// A single 4-state logic value following the IEEE 1164 standard.
///
/// The four states represent:
/// - `Zero` — logic low (driven 0)
/// - `One` — logic high (driven 1)
/// - `X` — unknown or uninitialized value
/// - `Z` — high-impedance (tri-state, not driven)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum Logic {
    /// Logic low (0).
    Zero = 0,
    /// Logic high (1).
    One = 1,
    /// Unknown or uninitialized.
    X = 2,
    /// High-impedance (tri-state).
    Z = 3,
}

impl Logic {
    /// Converts a character to a [`Logic`] value.
    ///
    /// Accepts '0', '1', 'x'/'X', and 'z'/'Z'.
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            '0' => Some(Logic::Zero),
            '1' => Some(Logic::One),
            'x' | 'X' => Some(Logic::X),
            'z' | 'Z' => Some(Logic::Z),
            _ => None,
        }
    }
}

impl fmt::Display for Logic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Logic::Zero => write!(f, "0"),
            Logic::One => write!(f, "1"),
            Logic::X => write!(f, "X"),
            Logic::Z => write!(f, "Z"),
        }
    }
}

/// IEEE 1164 AND truth table:
/// ```text
///     0  1  X  Z
/// 0 | 0  0  0  0
/// 1 | 0  1  X  X
/// X | 0  X  X  X
/// Z | 0  X  X  X
/// ```
impl BitAnd for Logic {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        use Logic::*;
        match (self, rhs) {
            (Zero, _) | (_, Zero) => Zero,
            (One, One) => One,
            _ => X,
        }
    }
}

/// IEEE 1164 OR truth table:
/// ```text
///     0  1  X  Z
/// 0 | 0  1  X  X
/// 1 | 1  1  1  1
/// X | X  1  X  X
/// Z | X  1  X  X
/// ```
impl BitOr for Logic {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        use Logic::*;
        match (self, rhs) {
            (One, _) | (_, One) => One,
            (Zero, Zero) => Zero,
            _ => X,
        }
    }
}

/// IEEE 1164 XOR truth table:
/// ```text
///     0  1  X  Z
/// 0 | 0  1  X  X
/// 1 | 1  0  X  X
/// X | X  X  X  X
/// Z | X  X  X  X
/// ```
impl BitXor for Logic {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self {
        use Logic::*;
        match (self, rhs) {
            (Zero, Zero) | (One, One) => Zero,
            (Zero, One) | (One, Zero) => One,
            _ => X,
        }
    }
}

/// IEEE 1164 NOT:
/// - `!0 = 1`, `!1 = 0`, `!X = X`, `!Z = X`
impl Not for Logic {
    type Output = Self;

    fn not(self) -> Self {
        use Logic::*;
        match self {
            Zero => One,
            One => Zero,
            X | Z => X,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Logic::*;

    #[test]
    fn and_truth_table() {
        // Zero dominates
        assert_eq!(Zero & Zero, Zero);
        assert_eq!(Zero & One, Zero);
        assert_eq!(Zero & X, Zero);
        assert_eq!(Zero & Z, Zero);
        assert_eq!(One & Zero, Zero);
        assert_eq!(X & Zero, Zero);
        assert_eq!(Z & Zero, Zero);
        // One & One
        assert_eq!(One & One, One);
        // Unknown cases
        assert_eq!(One & X, X);
        assert_eq!(One & Z, X);
        assert_eq!(X & X, X);
        assert_eq!(X & Z, X);
        assert_eq!(Z & Z, X);
    }

    #[test]
    fn or_truth_table() {
        // One dominates
        assert_eq!(One | Zero, One);
        assert_eq!(One | One, One);
        assert_eq!(One | X, One);
        assert_eq!(One | Z, One);
        assert_eq!(Zero | One, One);
        assert_eq!(X | One, One);
        assert_eq!(Z | One, One);
        // Zero | Zero
        assert_eq!(Zero | Zero, Zero);
        // Unknown cases
        assert_eq!(Zero | X, X);
        assert_eq!(Zero | Z, X);
        assert_eq!(X | X, X);
        assert_eq!(X | Z, X);
        assert_eq!(Z | Z, X);
    }

    #[test]
    fn xor_truth_table() {
        assert_eq!(Zero ^ Zero, Zero);
        assert_eq!(Zero ^ One, One);
        assert_eq!(One ^ Zero, One);
        assert_eq!(One ^ One, Zero);
        assert_eq!(Zero ^ X, X);
        assert_eq!(One ^ X, X);
        assert_eq!(X ^ Zero, X);
        assert_eq!(X ^ One, X);
        assert_eq!(X ^ X, X);
        assert_eq!(Z ^ Zero, X);
        assert_eq!(Z ^ One, X);
        assert_eq!(Z ^ Z, X);
    }

    #[test]
    fn not_values() {
        assert_eq!(!Zero, One);
        assert_eq!(!One, Zero);
        assert_eq!(!X, X);
        assert_eq!(!Z, X);
    }

    #[test]
    fn display() {
        assert_eq!(format!("{Zero}"), "0");
        assert_eq!(format!("{One}"), "1");
        assert_eq!(format!("{X}"), "X");
        assert_eq!(format!("{Z}"), "Z");
    }

    #[test]
    fn from_char_valid() {
        use super::Logic;
        assert_eq!(Logic::from_char('0'), Some(Zero));
        assert_eq!(Logic::from_char('1'), Some(One));
        assert_eq!(Logic::from_char('x'), Some(X));
        assert_eq!(Logic::from_char('X'), Some(X));
        assert_eq!(Logic::from_char('z'), Some(Z));
        assert_eq!(Logic::from_char('Z'), Some(Z));
    }

    #[test]
    fn from_char_invalid() {
        use super::Logic;
        assert_eq!(Logic::from_char('a'), None);
        assert_eq!(Logic::from_char('2'), None);
    }
}
