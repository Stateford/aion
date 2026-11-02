#![warn(missing_docs)]
//! Shared Verilog-2005 / SystemVerilog-2017 preprocessor.
//!
//! Both HDL front ends (`aion_verilog_parser` and `aion_sv_parser`) run this
//! crate over a file before lexing it. It implements:
//!
//! - text macros: `` `define `` with and without parameters (including SV
//!   default argument values), `\` line continuation, `` `undef ``,
//!   `` `undefineall ``, nested invocation, macros expanding to macros,
//!   `` `` `` token pasting, `` `" `` stringification, `` `__FILE__ `` and
//!   `` `__LINE__ ``, with a recursion guard and expansion limits;
//! - conditional compilation: `` `ifdef `` / `` `ifndef `` / `` `elsif `` /
//!   `` `else `` / `` `endif ``, properly nested;
//! - `` `include "file" `` (and `<file>`), resolved relative to the including
//!   file and then the configured include directories, with cycle and depth
//!   checks. Included files are loaded into the [`SourceDb`] and keep their
//!   own [`FileId`];
//! - the remaining standard directives (`` `timescale ``,
//!   `` `default_nettype ``, `` `resetall ``, `` `celldefine ``, `` `line ``,
//!   `` `pragma ``, `` `begin_keywords ``, `` `unconnected_drive `` ...),
//!   which are accepted and removed; `timescale` and `default_nettype`
//!   values are recorded in [`Preprocessed`].
//!
//! # Design
//!
//! Verilog macros are *text* macros (IEEE 1800-2017 §22.5): an expansion can
//! form part of a token (`` `WIDTH'b0 ``, `` a``b ``). The preprocessor
//! therefore works on text, not tokens, and is independent of either front
//! end's token type. Its output is a single expanded string plus a source
//! map recording, for every byte, the file and offset it was copied from.
//! [`lex_mapped`] lexes the expanded text with the front end's lexer and
//! remaps each token span through that map.
//!
//! # Span policy
//!
//! - Text that does not come from a macro expansion keeps its exact original
//!   span (in the top-level file or in the included file).
//! - Tokens copied verbatim from a macro body get the span of that text in
//!   the `` `define `` line; tokens from a macro argument get their span at
//!   the invocation site. So the `SourceDb` text of a span always equals the
//!   token text, which downstream stages rely on to read literal values.
//! - Tokens whose text exists in no file (token pasting, stringification, a
//!   literal straddling a macro boundary) point into a `<preprocessed>` view
//!   file registered in the `SourceDb` on first need.
//! - Preprocessor diagnostics point at the directive or macro usage. Inside a
//!   macro body they point at the body text, with a note naming the
//!   invocation site.

mod engine;
mod mapped;
mod options;
mod output;

pub use options::{PreprocessOptions, DEFAULT_MAX_INCLUDE_DEPTH};
pub use output::{lex_mapped, DirectiveValue, Preprocessed};

use aion_diagnostics::{Category, DiagnosticCode, DiagnosticSink};
use aion_source::{FileId, SourceDb};
use engine::{DbAccess, Engine};

/// The diagnostic code used for all preprocessor errors (`E106`).
pub fn diagnostic_code() -> DiagnosticCode {
    DiagnosticCode::new(Category::Error, 106)
}

/// Preprocesses `file` with full support for `` `include ``.
///
/// Included files and, when [`PreprocessOptions::defines`] is non-empty, a
/// `<predefined macros>` file are added to `source_db`. Errors are reported
/// to `sink`; preprocessing never fails outright and always returns output
/// for the parser.
pub fn preprocess(
    file: FileId,
    source_db: &mut SourceDb,
    options: &PreprocessOptions,
    sink: &DiagnosticSink,
) -> Preprocessed {
    Engine::new(DbAccess::Mut(source_db), file, options, sink).run()
}

/// Preprocesses `file` without modifying `source_db`.
///
/// Everything except `` `include `` works as in [`preprocess`]; an include
/// is reported as an error because the included file could not get a
/// [`FileId`]. Expanded text that exists in no source file (token pasting,
/// stringification) is mapped to its invocation site and reported once by
/// [`lex_mapped`] when it is given no `SourceDb`.
pub fn preprocess_readonly(
    file: FileId,
    source_db: &SourceDb,
    options: &PreprocessOptions,
    sink: &DiagnosticSink,
) -> Preprocessed {
    Engine::new(DbAccess::Ref(source_db), file, options, sink).run()
}

#[cfg(test)]
mod tests;
