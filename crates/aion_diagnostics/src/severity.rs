//! Diagnostic severity levels ordered from least to most severe.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The severity level of a diagnostic message.
///
/// Ordered from least severe (`Help`) to most severe (`Error`), matching the
/// derived `PartialOrd`/`Ord` implementation based on declaration order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Severity {
    /// A helpful suggestion that doesn't indicate a problem.
    Help,
    /// An informational note providing additional context.
    Note,
    /// A potential issue that should be reviewed but doesn't prevent compilation.
    Warning,
    /// A definite problem that prevents successful compilation.
    Error,
}

impl Severity {
    /// Returns `true` if this severity is [`Error`](Severity::Error).
    pub fn is_error(self) -> bool {
        self == Severity::Error
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Help => write!(f, "help"),
            Severity::Note => write!(f, "note"),
            Severity::Warning => write!(f, "warning"),
            Severity::Error => write!(f, "error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        assert!(Severity::Help < Severity::Note);
        assert!(Severity::Note < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
    }

    #[test]
    fn is_error() {
        assert!(Severity::Error.is_error());
        assert!(!Severity::Warning.is_error());
        assert!(!Severity::Note.is_error());
        assert!(!Severity::Help.is_error());
    }

    #[test]
    fn display() {
        assert_eq!(format!("{}", Severity::Error), "error");
        assert_eq!(format!("{}", Severity::Warning), "warning");
        assert_eq!(format!("{}", Severity::Note), "note");
        assert_eq!(format!("{}", Severity::Help), "help");
    }
}
