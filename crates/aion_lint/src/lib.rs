//! Lint rules and engine for static analysis of HDL designs.
//!
//! This crate implements warning, error, and convention lint rules that operate
//! on the AionIR to detect common design mistakes, enforce coding standards,
//! and identify potential synthesis issues.
//!
//! # Rule Categories
//!
//! - **W-series (warnings):** Unused signals, width mismatches, missing resets, latches
//! - **E-series (errors):** Non-synthesizable constructs, multiple drivers, port mismatches
//! - **C-series (conventions):** Naming violations, magic numbers, inconsistent coding style

#![warn(missing_docs)]

mod engine;
mod helpers;
mod rules;

pub use engine::LintEngine;
pub use helpers::{
    check_cell_port_match, collect_expr_signals, collect_read_signals, collect_signal_ref_signals,
    collect_written_signals, count_drivers, has_assign, is_signal_driven_in_module,
    is_signal_read_in_module, stmt_has_full_else_coverage, PortMatchIssue,
};
pub use rules::register_builtin_rules;
pub use rules::{
    is_camel_case, is_pascal_case, is_snake_case, is_upper_snake_case, DeadLogic,
    IncompleteSensitivity, InconsistentStyle, LatchInferred, MagicNumber, MissingDoc, MissingReset,
    MultipleDrivers, NamingViolation, NonSynthesizable, PortMismatch, Truncation, UndrivenSignal,
    UnusedSignal, WidthMismatch,
};

use aion_diagnostics::{DiagnosticCode, DiagnosticSink, Severity};
use aion_ir::{Design, Module};

/// A single lint rule that checks a module for design issues.
///
/// Each rule has a unique diagnostic code, a human-readable name, a description,
/// and a default severity. The `check_module` method is called for each module
/// in the design and should emit diagnostics via the provided sink.
pub trait LintRule: Send + Sync {
    /// Returns the diagnostic code for this rule (e.g., W101, E104).
    fn code(&self) -> DiagnosticCode;

    /// Returns the short kebab-case name of this rule (e.g., "unused-signal").
    fn name(&self) -> &str;

    /// Returns a human-readable description of what this rule checks.
    fn description(&self) -> &str;

    /// Returns the default severity for diagnostics emitted by this rule.
    fn default_severity(&self) -> Severity;

    /// Checks a single module for issues and emits diagnostics to the sink.
    fn check_module(&self, module: &Module, design: &Design, sink: &DiagnosticSink);
}
