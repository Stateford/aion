//! Diagnostic codes with category prefixes for structured error identification.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The category of a diagnostic code, determining its prefix letter.
///
/// Each category maps to a single-character prefix used in diagnostic code
/// display (e.g., `E101` for an error, `W203` for a warning).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Category {
    /// Error diagnostics, prefixed with `E`.
    Error,
    /// Warning diagnostics, prefixed with `W`.
    Warning,
    /// Convention diagnostics, prefixed with `C`.
    Convention,
    /// Timing diagnostics, prefixed with `T`.
    Timing,
    /// Vendor-specific diagnostics, prefixed with `S`.
    Vendor,
}

impl Category {
    /// Returns the single-character prefix for this category.
    pub fn prefix(self) -> char {
        match self {
            Category::Error => 'E',
            Category::Warning => 'W',
            Category::Convention => 'C',
            Category::Timing => 'T',
            Category::Vendor => 'S',
        }
    }
}

/// A structured diagnostic code combining a category prefix and a numeric identifier.
///
/// Displayed as the category prefix followed by a zero-padded 3-digit number,
/// e.g., `E101`, `W203`, `T305`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DiagnosticCode {
    /// The category of this diagnostic.
    pub category: Category,
    /// The numeric identifier within the category.
    pub number: u16,
}

impl DiagnosticCode {
    /// Creates a new diagnostic code.
    pub fn new(category: Category, number: u16) -> Self {
        Self { category, number }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{:03}", self.category.prefix(), self.number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_prefixes() {
        assert_eq!(Category::Error.prefix(), 'E');
        assert_eq!(Category::Warning.prefix(), 'W');
        assert_eq!(Category::Convention.prefix(), 'C');
        assert_eq!(Category::Timing.prefix(), 'T');
        assert_eq!(Category::Vendor.prefix(), 'S');
    }

    #[test]
    fn display_format() {
        let code = DiagnosticCode::new(Category::Error, 101);
        assert_eq!(format!("{code}"), "E101");

        let code = DiagnosticCode::new(Category::Warning, 3);
        assert_eq!(format!("{code}"), "W003");

        let code = DiagnosticCode::new(Category::Timing, 42);
        assert_eq!(format!("{code}"), "T042");
    }

    #[test]
    fn serde_roundtrip() {
        let code = DiagnosticCode::new(Category::Error, 101);
        let json = serde_json::to_string(&code).unwrap();
        let back: DiagnosticCode = serde_json::from_str(&json).unwrap();
        assert_eq!(code, back);
    }
}
