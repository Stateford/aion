//! Auto-applicable fix suggestions for diagnostics.

use aion_source::Span;
use serde::{Deserialize, Serialize};

/// A text replacement to apply to source code as part of a suggested fix.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Replacement {
    /// The source span to replace.
    pub span: Span,
    /// The new text to insert in place of the span.
    pub new_text: String,
}

/// A suggested fix that can be automatically applied to source code.
///
/// A fix consists of a human-readable message describing the change and
/// one or more [`Replacement`]s that together implement the fix.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuggestedFix {
    /// A description of what this fix does.
    pub message: String,
    /// The set of text replacements that implement this fix.
    pub replacements: Vec<Replacement>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_fix() {
        let fix = SuggestedFix {
            message: "add missing semicolon".to_string(),
            replacements: vec![Replacement {
                span: Span::DUMMY,
                new_text: ";".to_string(),
            }],
        };
        assert_eq!(fix.message, "add missing semicolon");
        assert_eq!(fix.replacements.len(), 1);
    }

    #[test]
    fn multi_replacement_fix() {
        let fix = SuggestedFix {
            message: "rename signal".to_string(),
            replacements: vec![
                Replacement {
                    span: Span::DUMMY,
                    new_text: "new_name".to_string(),
                },
                Replacement {
                    span: Span::DUMMY,
                    new_text: "new_name".to_string(),
                },
            ],
        };
        assert_eq!(fix.replacements.len(), 2);
    }
}
