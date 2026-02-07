//! Structured diagnostic messages with severity, codes, labels, and fixes.

use crate::code::DiagnosticCode;
use crate::label::Label;
use crate::severity::Severity;
use crate::suggested_fix::SuggestedFix;
use aion_source::Span;
use serde::{Deserialize, Serialize};

/// A structured diagnostic message with source locations, labels, and optional fixes.
///
/// Diagnostics are the primary mechanism for reporting errors, warnings, and
/// suggestions to the user. Each diagnostic includes:
/// - A severity level and unique error code
/// - A primary message and source span
/// - Optional secondary labels, notes, help text, and auto-applicable fixes
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The severity level of this diagnostic.
    pub severity: Severity,
    /// The unique error code identifying the type of diagnostic.
    pub code: DiagnosticCode,
    /// The main diagnostic message.
    pub message: String,
    /// The primary source span where the issue was detected.
    pub primary_span: Span,
    /// Additional annotated source spans providing context.
    pub labels: Vec<Label>,
    /// Explanatory footnotes (e.g., "note: ...").
    pub notes: Vec<String>,
    /// Actionable suggestions (e.g., "help: ...").
    pub help: Vec<String>,
    /// An auto-applicable fix, if available.
    pub fix: Option<SuggestedFix>,
}

impl Diagnostic {
    /// Creates a new error diagnostic with the given code, message, and span.
    pub fn error(code: DiagnosticCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            primary_span: span,
            labels: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            fix: None,
        }
    }

    /// Creates a new warning diagnostic with the given code, message, and span.
    pub fn warning(code: DiagnosticCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            message: message.into(),
            primary_span: span,
            labels: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            fix: None,
        }
    }

    /// Adds a label to this diagnostic.
    pub fn with_label(mut self, label: Label) -> Self {
        self.labels.push(label);
        self
    }

    /// Adds a note to this diagnostic.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Adds a help message to this diagnostic.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    /// Sets the suggested fix for this diagnostic.
    pub fn with_fix(mut self, fix: SuggestedFix) -> Self {
        self.fix = Some(fix);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::Category;

    #[test]
    fn create_error() {
        let code = DiagnosticCode::new(Category::Error, 101);
        let diag = Diagnostic::error(code, "unexpected token", Span::DUMMY);
        assert_eq!(diag.severity, Severity::Error);
        assert_eq!(diag.message, "unexpected token");
        assert_eq!(format!("{}", diag.code), "E101");
    }

    #[test]
    fn create_warning() {
        let code = DiagnosticCode::new(Category::Warning, 201);
        let diag = Diagnostic::warning(code, "unused signal", Span::DUMMY);
        assert_eq!(diag.severity, Severity::Warning);
        assert_eq!(diag.message, "unused signal");
    }

    #[test]
    fn builder_methods() {
        let code = DiagnosticCode::new(Category::Error, 101);
        let diag = Diagnostic::error(code, "type mismatch", Span::DUMMY)
            .with_label(Label::primary(Span::DUMMY, "expected std_logic"))
            .with_note("types must match in assignments")
            .with_help("consider using to_std_logic()");
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.notes.len(), 1);
        assert_eq!(diag.help.len(), 1);
        assert!(diag.fix.is_none());
    }

    #[test]
    fn with_fix_sets_fix() {
        use crate::suggested_fix::{Replacement, SuggestedFix};

        let code = DiagnosticCode::new(Category::Error, 102);
        let fix = SuggestedFix {
            message: "add missing semicolon".to_string(),
            replacements: vec![Replacement {
                span: Span::DUMMY,
                new_text: ";".to_string(),
            }],
        };
        let diag = Diagnostic::error(code, "expected ';'", Span::DUMMY).with_fix(fix);
        assert!(diag.fix.is_some());
        assert_eq!(diag.fix.unwrap().message, "add missing semicolon");
    }
}
