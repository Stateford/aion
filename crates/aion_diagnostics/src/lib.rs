//! Diagnostic creation, severity management, and multi-format rendering.
//!
//! This crate provides structured [`Diagnostic`] messages with severity levels,
//! error codes, source labels, and suggested fixes. The thread-safe [`DiagnosticSink`]
//! accumulates diagnostics during compilation, and [`DiagnosticRenderer`] implementations
//! format them for terminal, JSON, or SARIF output.

#![warn(missing_docs)]

pub mod code;
pub mod diagnostic;
pub mod label;
pub mod renderer;
pub mod severity;
pub mod sink;
pub mod suggested_fix;

pub use code::{Category, DiagnosticCode};
pub use diagnostic::Diagnostic;
pub use label::{Label, LabelStyle};
pub use renderer::{DiagnosticRenderer, TerminalRenderer};
pub use severity::Severity;
pub use sink::DiagnosticSink;
pub use suggested_fix::{Replacement, SuggestedFix};
