//! Labels that annotate source spans within a diagnostic.

use aion_source::Span;
use serde::{Deserialize, Serialize};

/// The visual style of a diagnostic label.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LabelStyle {
    /// The primary label highlighting the main error location (e.g., `^^^^`).
    Primary,
    /// A secondary label providing additional context (e.g., `----`).
    Secondary,
}

/// An annotated source span within a diagnostic, pointing to a specific location
/// in source code with an explanatory message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Label {
    /// The source span this label annotates.
    pub span: Span,
    /// The message displayed next to the underline.
    pub message: String,
    /// Whether this is a primary or secondary label.
    pub style: LabelStyle,
}

impl Label {
    /// Creates a primary label (the main error location).
    pub fn primary(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            style: LabelStyle::Primary,
        }
    }

    /// Creates a secondary label (additional context).
    pub fn secondary(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            style: LabelStyle::Secondary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_label() {
        let label = Label::primary(Span::DUMMY, "expected type");
        assert_eq!(label.style, LabelStyle::Primary);
        assert_eq!(label.message, "expected type");
    }

    #[test]
    fn secondary_label() {
        let label = Label::secondary(Span::DUMMY, "defined here");
        assert_eq!(label.style, LabelStyle::Secondary);
        assert_eq!(label.message, "defined here");
    }
}
