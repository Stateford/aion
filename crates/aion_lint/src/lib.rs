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
//!
//! # Running the linter
//!
//! Rules see the elaborated design plus a [`LintContext`] carrying the
//! session's interner (to print real signal, port and module names), the
//! source database (for checks on comments and original spelling), and the
//! optional target [`Architecture`](aion_arch::Architecture):
//!
//! ```
//! use aion_common::Interner;
//! use aion_diagnostics::DiagnosticSink;
//! use aion_lint::{LintContext, LintEngine};
//! use aion_source::SourceDb;
//!
//! # fn lint(design: &aion_ir::Design, interner: &Interner, sources: &SourceDb) {
//! let ctx = LintContext::new(interner, sources);
//! let sink = DiagnosticSink::new();
//! LintEngine::with_defaults().run(design, &ctx, &sink);
//! # }
//! ```

#![warn(missing_docs)]

mod context;
mod engine;
mod helpers;
mod rules;

pub use context::{LintContext, UNKNOWN_NAME};
pub use engine::LintEngine;
pub use helpers::{
    check_cell_port_match, collect_expr_signals, collect_read_signals, collect_signal_ref_signals,
    collect_written_signals, count_drivers, expr_width, has_assign, is_signal_driven_in_module,
    is_signal_read_in_module, range_width, signal_ref_width, stmt_has_full_else_coverage,
    type_width, PortMatchIssue,
};
pub use rules::register_builtin_rules;
pub use rules::{
    is_camel_case, is_pascal_case, is_snake_case, is_upper_snake_case, DeadLogic,
    IncompleteSensitivity, InconsistentStyle, LatchInferred, MagicNumber, MissingDoc, MissingReset,
    MultipleDrivers, NamingViolation, NonSynthesizable, PortMismatch, Truncation, UndrivenSignal,
    UnusedSignal, WidthMismatch,
};

use aion_diagnostics::{Category, DiagnosticCode, DiagnosticSink, Severity};
use aion_ir::{Design, Module};

/// Diagnostic code `E199`, emitted by [`LintEngine::run`] when a lint rule
/// panics while checking a module.
///
/// A panic inside a rule is a bug in Aion, not in the user's design. The
/// engine catches it, reports this internal error (including the rule name,
/// module, and panic message), and keeps running the remaining rules so one
/// faulty rule cannot abort the whole lint pass.
pub const LINT_INTERNAL_ERROR: DiagnosticCode = DiagnosticCode {
    category: Category::Error,
    number: 199,
};

/// A single lint rule that checks a module for design issues.
///
/// Each rule has a unique diagnostic code, a human-readable name, a description,
/// and a default severity. The `check_module` method is called for each module
/// in the design and should emit diagnostics via the provided sink. The
/// [`LintContext`] gives access to names, source text and the target device.
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
    ///
    /// `ctx` resolves the module's interned names (messages must name the
    /// offending signal, port or module, never print raw IDs), exposes the
    /// source text, and carries the target architecture when one is known.
    fn check_module(
        &self,
        module: &Module,
        design: &Design,
        ctx: &LintContext<'_>,
        sink: &DiagnosticSink,
    );
}
