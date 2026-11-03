//! The read-only environment handed to every lint rule.
//!
//! Lint rules work on the elaborated [`Design`](aion_ir::Design), whose names
//! are interned [`Ident`]s and whose locations are [`Span`]s. Neither is
//! meaningful on its own: turning them into the text a user wrote needs the
//! session's [`Interner`] and [`SourceDb`]. [`LintContext`] bundles those
//! together with the optional target [`Architecture`] (for rules that only make
//! sense for a specific device) and offers non-panicking helpers to resolve
//! names and source text, so every diagnostic can name the offending signal,
//! port or module instead of printing a raw interner ID.

use std::borrow::Cow;
use std::fmt;

use aion_arch::Architecture;
use aion_common::{Ident, Interner};
use aion_ir::{Module, SignalId, SignalRef};
use aion_source::{SourceDb, Span};

/// The placeholder printed when an [`Ident`] cannot be resolved by the
/// context's interner.
///
/// This only happens when the design was built with a different interner
/// than the one handed to the lint engine, which is a caller bug; rules print
/// this instead of panicking or leaking the raw ID.
pub const UNKNOWN_NAME: &str = "<unknown>";

/// Shared, read-only state passed to [`LintRule::check_module`](crate::LintRule::check_module).
///
/// Carries the [`Interner`] that resolves identifiers in the design, the
/// [`SourceDb`] holding the original source text (for rules such as C202 that
/// look at comments), and, when a target device is known, its
/// [`Architecture`] model (for vendor-specific rules). The context is cheap
/// to copy: it only holds references.
#[derive(Clone, Copy)]
pub struct LintContext<'a> {
    /// Resolves the [`Ident`]s stored in the design.
    interner: &'a Interner,
    /// The source files the design was elaborated from.
    source_db: &'a SourceDb,
    /// The target device, if one was selected.
    arch: Option<&'a dyn Architecture>,
}

impl fmt::Debug for LintContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LintContext")
            .field("arch", &self.arch.map(|a| a.family_name()))
            .finish_non_exhaustive()
    }
}

impl<'a> LintContext<'a> {
    /// Creates a context with no target architecture.
    ///
    /// `interner` must be the interner the design was elaborated with and
    /// `source_db` the database its spans point into.
    pub fn new(interner: &'a Interner, source_db: &'a SourceDb) -> Self {
        Self {
            interner,
            source_db,
            arch: None,
        }
    }

    /// Returns this context with its target architecture set to `arch`.
    ///
    /// Passing `None` clears it, so callers that only sometimes have a target
    /// can forward their `Option` unchanged.
    pub fn with_arch(mut self, arch: Option<&'a dyn Architecture>) -> Self {
        self.arch = arch;
        self
    }

    /// Returns the interner that resolves the design's identifiers.
    pub fn interner(&self) -> &'a Interner {
        self.interner
    }

    /// Returns the source database the design's spans point into.
    pub fn source_db(&self) -> &'a SourceDb {
        self.source_db
    }

    /// Returns the target device model, or `None` when linting without a
    /// target (vendor-specific rules then stay silent).
    pub fn arch(&self) -> Option<&'a dyn Architecture> {
        self.arch
    }

    /// Resolves `ident` to its string, or `None` if this context's interner
    /// did not create it.
    pub fn try_name(&self, ident: Ident) -> Option<&'a str> {
        self.interner.try_resolve(ident)
    }

    /// Resolves `ident` to its string for use in a diagnostic message.
    ///
    /// Falls back to [`UNKNOWN_NAME`] instead of panicking or printing the
    /// raw ID when the identifier is foreign to this context's interner.
    pub fn name(&self, ident: Ident) -> &'a str {
        self.try_name(ident).unwrap_or(UNKNOWN_NAME)
    }

    /// Returns the name of signal `id` in `module`, or [`UNKNOWN_NAME`] if the
    /// ID is out of range or its name cannot be resolved.
    pub fn signal_name(&self, module: &Module, id: SignalId) -> &'a str {
        if (id.as_raw() as usize) < module.signals.len() {
            self.name(module.signals.get(id).name)
        } else {
            UNKNOWN_NAME
        }
    }

    /// Renders an assignment target or connection (`SignalRef`) the way it
    /// would be written in Verilog: `q`, `q[7:4]`, `q[3]` or `{a, b}`.
    ///
    /// Returns `None` for a constant, or a concatenation made only of
    /// constants, since there is no signal to name.
    pub fn signal_ref_name(&self, module: &Module, sref: &SignalRef) -> Option<String> {
        match sref {
            SignalRef::Signal(id) => Some(self.signal_name(module, *id).to_string()),
            SignalRef::Slice { signal, high, low } => {
                let name = self.signal_name(module, *signal);
                Some(if high == low {
                    format!("{name}[{high}]")
                } else {
                    format!("{name}[{high}:{low}]")
                })
            }
            SignalRef::Concat(parts) => {
                let names: Vec<String> = parts
                    .iter()
                    .filter_map(|p| self.signal_ref_name(module, p))
                    .collect();
                if names.is_empty() {
                    None
                } else {
                    Some(format!("{{{}}}", names.join(", ")))
                }
            }
            SignalRef::Const(_) => None,
        }
    }

    /// Returns the source text covered by `span`, or `None` if the span is a
    /// dummy, points to an unknown file, or is out of range.
    pub fn source_text(&self, span: Span) -> Option<&'a str> {
        if span.is_dummy() {
            return None;
        }
        self.source_db.try_snippet(span)
    }

    /// Returns the text of `span`'s file from the start of the file up to
    /// (not including) the span's first byte, or `None` if the span is a
    /// dummy, its file is unknown, or its start lies outside the file.
    ///
    /// Used by rules that need context around a node, such as the comment
    /// lines preceding a module declaration.
    pub fn file_text_before(&self, span: Span) -> Option<&'a str> {
        if span.is_dummy() {
            return None;
        }
        let file = self.source_db.try_get_file(span.file)?;
        file.content.get(..span.start as usize)
    }

    /// Returns true if `span` lies in a VHDL source file (extension `.vhd` or
    /// `.vhdl`, in any letter case).
    ///
    /// VHDL identifiers are case-insensitive while Verilog and SystemVerilog
    /// identifiers are case-sensitive, so rules that compare names read from
    /// source text must know which language the text is in. Returns `false`
    /// for a dummy span or an unknown file.
    pub fn is_vhdl_source(&self, span: Span) -> bool {
        if span.is_dummy() {
            return false;
        }
        self.source_db
            .try_get_file(span.file)
            .and_then(|f| f.path.extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("vhd") || e.eq_ignore_ascii_case("vhdl"))
    }

    /// Returns `ident` as the user spelled it in the source text of `span`.
    ///
    /// VHDL folds basic identifiers to lowercase when interning them, so the
    /// interned string of `DATA_WIDTH` is `data_width`. Checks that depend on
    /// letter case (naming conventions) must look at the original spelling:
    /// this finds the first whole-word, case-insensitive occurrence of the
    /// interned name inside the span's text. When the text is unavailable or
    /// does not contain the name, the interned string is returned.
    pub fn source_spelling(&self, ident: Ident, span: Span) -> Cow<'a, str> {
        let Some(interned) = self.try_name(ident) else {
            return Cow::Borrowed(UNKNOWN_NAME);
        };
        // Scoped names (`proc.var`) are matched by their last segment.
        let wanted = interned.rsplit('.').next().unwrap_or(interned);
        if wanted.is_empty() {
            return Cow::Borrowed(interned);
        }
        let Some(text) = self.source_text(span) else {
            return Cow::Borrowed(wanted);
        };
        match find_word_ignore_ascii_case(text, wanted) {
            Some(start) => Cow::Borrowed(&text[start..start + wanted.len()]),
            None => Cow::Borrowed(wanted),
        }
    }
}

/// Returns `true` if `b` can be part of an HDL identifier.
pub(crate) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Finds the byte offset of the first occurrence of `word` in `text` that is
/// not part of a longer identifier, comparing ASCII letters case-insensitively.
fn find_word_ignore_ascii_case(text: &str, word: &str) -> Option<usize> {
    let hay = text.as_bytes();
    let needle = word.as_bytes();
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| {
        hay[i..i + needle.len()].eq_ignore_ascii_case(needle)
            && (i == 0 || !is_ident_byte(hay[i - 1]))
            && hay.get(i + needle.len()).is_none_or(|&b| !is_ident_byte(b))
            && text.is_char_boundary(i)
            && text.is_char_boundary(i + needle.len())
    })
}

#[cfg(test)]
pub(crate) mod test_util {
    //! Shared fixtures for rule unit tests.

    use std::sync::OnceLock;

    use super::*;

    /// Returns a context backed by an empty interner and source database.
    ///
    /// Rule tests that build designs from `Ident::from_raw` use this; names
    /// then render as [`UNKNOWN_NAME`].
    pub(crate) fn empty_ctx() -> LintContext<'static> {
        static INTERNER: OnceLock<Interner> = OnceLock::new();
        static SOURCES: OnceLock<SourceDb> = OnceLock::new();
        LintContext::new(
            INTERNER.get_or_init(Interner::new),
            SOURCES.get_or_init(SourceDb::new),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, LogicVec};
    use aion_ir::*;

    fn mk_module(interner: &Interner, names: &[&str]) -> Module {
        let mut signals = Arena::new();
        for (i, n) in names.iter().enumerate() {
            signals.alloc(Signal {
                id: SignalId::from_raw(i as u32),
                name: interner.get_or_intern(n),
                ty: TypeId::from_raw(0),
                kind: SignalKind::Wire,
                init: None,
                clock_domain: None,
                span: Span::DUMMY,
            });
        }
        Module {
            id: ModuleId::from_raw(0),
            name: interner.get_or_intern("top"),
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals,
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(&[]),
        }
    }

    #[test]
    fn name_resolves_known_and_falls_back_for_foreign() {
        let interner = Interner::new();
        let db = SourceDb::new();
        let ctx = LintContext::new(&interner, &db);
        let clk = interner.get_or_intern("clk");
        assert_eq!(ctx.name(clk), "clk");
        assert_eq!(ctx.try_name(clk), Some("clk"));
        assert_eq!(ctx.name(Ident::from_raw(99)), UNKNOWN_NAME);
        assert_eq!(ctx.try_name(Ident::from_raw(99)), None);
    }

    #[test]
    fn arch_defaults_to_none() {
        let interner = Interner::new();
        let db = SourceDb::new();
        let ctx = LintContext::new(&interner, &db);
        assert!(ctx.arch().is_none());
        assert!(ctx.with_arch(None).arch().is_none());
        assert!(format!("{ctx:?}").contains("LintContext"));
    }

    #[test]
    fn signal_name_in_and_out_of_range() {
        let interner = Interner::new();
        let db = SourceDb::new();
        let ctx = LintContext::new(&interner, &db);
        let m = mk_module(&interner, &["a", "b"]);
        assert_eq!(ctx.signal_name(&m, SignalId::from_raw(1)), "b");
        assert_eq!(ctx.signal_name(&m, SignalId::from_raw(9)), UNKNOWN_NAME);
    }

    #[test]
    fn signal_ref_name_forms() {
        let interner = Interner::new();
        let db = SourceDb::new();
        let ctx = LintContext::new(&interner, &db);
        let m = mk_module(&interner, &["a", "b"]);
        let a = SignalId::from_raw(0);
        let b = SignalId::from_raw(1);
        assert_eq!(
            ctx.signal_ref_name(&m, &SignalRef::Signal(a)).as_deref(),
            Some("a")
        );
        assert_eq!(
            ctx.signal_ref_name(
                &m,
                &SignalRef::Slice {
                    signal: b,
                    high: 7,
                    low: 4
                }
            )
            .as_deref(),
            Some("b[7:4]")
        );
        assert_eq!(
            ctx.signal_ref_name(
                &m,
                &SignalRef::Slice {
                    signal: b,
                    high: 3,
                    low: 3
                }
            )
            .as_deref(),
            Some("b[3]")
        );
        let cat = SignalRef::Concat(vec![
            SignalRef::Signal(a),
            SignalRef::Const(LogicVec::all_zero(1)),
            SignalRef::Signal(b),
        ]);
        assert_eq!(ctx.signal_ref_name(&m, &cat).as_deref(), Some("{a, b}"));
        assert_eq!(
            ctx.signal_ref_name(&m, &SignalRef::Const(LogicVec::all_zero(2))),
            None
        );
        let const_cat = SignalRef::Concat(vec![SignalRef::Const(LogicVec::all_zero(1))]);
        assert_eq!(ctx.signal_ref_name(&m, &const_cat), None);
    }

    #[test]
    fn source_text_valid_and_invalid_spans() {
        let interner = Interner::new();
        let mut db = SourceDb::new();
        let f = db.add_source("t.v", "module top; endmodule".to_string());
        let ctx = LintContext::new(&interner, &db);
        assert_eq!(ctx.source_text(Span::new(f, 7, 10)), Some("top"));
        assert_eq!(ctx.source_text(Span::DUMMY), None);
        assert_eq!(ctx.source_text(Span::new(f, 0, 500)), None);
        assert_eq!(ctx.file_text_before(Span::new(f, 7, 10)), Some("module "));
        assert_eq!(ctx.file_text_before(Span::DUMMY), None);
        assert_eq!(ctx.file_text_before(Span::new(f, 900, 901)), None);
    }

    #[test]
    fn is_vhdl_source_by_extension() {
        let interner = Interner::new();
        let mut db = SourceDb::new();
        let vhd = db.add_source("a.vhd", "x".to_string());
        let vhdl = db.add_source("b.VHDL", "x".to_string());
        let v = db.add_source("c.v", "x".to_string());
        let none = db.add_source("noext", "x".to_string());
        let ctx = LintContext::new(&interner, &db);
        assert!(ctx.is_vhdl_source(Span::new(vhd, 0, 1)));
        assert!(ctx.is_vhdl_source(Span::new(vhdl, 0, 1)));
        assert!(!ctx.is_vhdl_source(Span::new(v, 0, 1)));
        assert!(!ctx.is_vhdl_source(Span::new(none, 0, 1)));
        assert!(!ctx.is_vhdl_source(Span::DUMMY));
        assert!(!ctx.is_vhdl_source(Span::new(aion_source::FileId::from_raw(99), 0, 1)));
    }

    #[test]
    fn source_spelling_recovers_vhdl_case() {
        let interner = Interner::new();
        let mut db = SourceDb::new();
        let text = "constant DATA_WIDTH : integer := 8;";
        let f = db.add_source("t.vhd", text.to_string());
        let ctx = LintContext::new(&interner, &db);
        let id = interner.get_or_intern("data_width");
        let span = Span::new(f, 0, text.len() as u32);
        assert_eq!(ctx.source_spelling(id, span), "DATA_WIDTH");
        // Scoped names match on their last segment.
        let scoped = interner.get_or_intern("p.data_width");
        assert_eq!(ctx.source_spelling(scoped, span), "DATA_WIDTH");
    }

    #[test]
    fn source_spelling_requires_whole_word_and_falls_back() {
        let interner = Interner::new();
        let mut db = SourceDb::new();
        let text = "signal ab_x, AB : bit;";
        let f = db.add_source("t.vhd", text.to_string());
        let ctx = LintContext::new(&interner, &db);
        let span = Span::new(f, 0, text.len() as u32);
        // `ab` inside `ab_x` is not a whole word; the standalone `AB` is.
        assert_eq!(
            ctx.source_spelling(interner.get_or_intern("ab"), span),
            "AB"
        );
        // Not present: falls back to the interned spelling.
        assert_eq!(
            ctx.source_spelling(interner.get_or_intern("zz"), span),
            "zz"
        );
        // No source text: falls back too.
        assert_eq!(
            ctx.source_spelling(interner.get_or_intern("q"), Span::DUMMY),
            "q"
        );
        // Foreign ident.
        assert_eq!(
            ctx.source_spelling(Ident::from_raw(1000), span),
            UNKNOWN_NAME
        );
    }

    #[test]
    fn find_word_edge_cases() {
        assert_eq!(find_word_ignore_ascii_case("abc", ""), None);
        assert_eq!(find_word_ignore_ascii_case("ab", "abc"), None);
        assert_eq!(find_word_ignore_ascii_case("x abc", "ABC"), Some(2));
        assert_eq!(find_word_ignore_ascii_case("xabc abc", "abc"), Some(5));
        assert_eq!(find_word_ignore_ascii_case("\u{e9}abc", "abc"), Some(2));
    }
}
