//! The result of a preprocessor run and the span remapping that goes with it.

use crate::mapped::{MappedText, Origin};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, SourceDb, Span};

/// A value recorded from a compiler directive such as `` `timescale `` or
/// `` `default_nettype ``, together with where the directive appeared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectiveValue {
    /// The directive's argument text, trimmed (e.g. `1ns/1ps` or `none`).
    pub value: String,
    /// The span of the whole directive in its source file.
    pub span: Span,
}

/// The output of preprocessing one top-level file.
///
/// [`text`](Self::text) is the fully expanded source: directives removed,
/// inactive conditional regions dropped, includes inlined and macros
/// expanded. It is meant to be lexed as if it were the file's content, with
/// spans in "preprocessed coordinates" (byte offsets into `text`, tagged with
/// the top-level [`FileId`]). [`lex_mapped`] then remaps those spans back to
/// real locations in the [`SourceDb`].
pub struct Preprocessed {
    pub(crate) map: MappedText,
    pub(crate) top_file: FileId,
    pub(crate) top_len: u32,
    /// Every `` `timescale `` directive seen in active code, in order.
    pub timescales: Vec<DirectiveValue>,
    /// Every `` `default_nettype `` directive seen in active code, in order.
    pub default_nettypes: Vec<DirectiveValue>,
    /// Files pulled in through `` `include ``, in first-inclusion order.
    pub included_files: Vec<FileId>,
}

impl Preprocessed {
    /// Returns the fully preprocessed text.
    pub fn text(&self) -> &str {
        &self.map.text
    }

    /// Returns the top-level file this output was produced from.
    pub fn top_file(&self) -> FileId {
        self.top_file
    }

    /// Returns the real span for `start..end` in preprocessed coordinates
    /// when those bytes are an exact copy of one region of one source file.
    pub fn exact_span(&self, start: u32, end: u32) -> Option<Span> {
        self.map.exact_span(start as usize, end as usize)
    }

    /// Maps a span in preprocessed coordinates to a real, in-bounds span.
    ///
    /// Text copied verbatim from a file (including macro bodies and macro
    /// arguments) maps to its exact location. Text that exists in no file
    /// (token pasting, stringification, a token straddling a macro boundary)
    /// maps to the outermost macro invocation that produced it. Spans whose
    /// file is not the top-level file are returned unchanged.
    pub fn map_span(&self, span: Span) -> Span {
        if span.file != self.top_file {
            return span;
        }
        let (s, e) = (span.start as usize, span.end as usize);
        if s >= self.map.len() {
            return Span::new(self.top_file, self.top_len, self.top_len);
        }
        let e = e.min(self.map.len());
        self.map
            .exact_span(s, e)
            .unwrap_or_else(|| self.fallback_span(s, e))
    }

    /// Best real location for inexact text: the macro invocation site, or the
    /// exactly-mapped prefix of the first run, or the start of the top file.
    fn fallback_span(&self, s: usize, e: usize) -> Span {
        if let Some(idx) = self.map.seg_index(s) {
            let seg = self.map.segs[idx];
            if let Some(site) = seg.site {
                return site;
            }
            if let Origin::Real { .. } = seg.origin {
                if let Some(sp) = self.map.exact_span(s, e.min(seg.end() as usize)) {
                    return sp;
                }
            }
        }
        Span::new(self.top_file, 0, 0)
    }
}

/// Remaps spans from preprocessed coordinates, registering a
/// `<preprocessed>` view of the output in the `SourceDb` on first need.
struct Mapper<'a> {
    pp: &'a Preprocessed,
    db: Option<&'a mut SourceDb>,
    view: Option<FileId>,
    reported: bool,
    sink: &'a DiagnosticSink,
}

impl Mapper<'_> {
    fn map(&mut self, span: Span) -> Span {
        if span.file != self.pp.top_file {
            return span;
        }
        let len = self.pp.map.len();
        let (s, e) = (span.start as usize, (span.end as usize).min(len));
        if s >= len {
            return self.pp.map_span(span);
        }
        if let Some(exact) = self.pp.map.exact_span(s, e) {
            return exact;
        }
        if let Some(db) = self.db.as_deref_mut() {
            let pp = self.pp;
            let view = *self.view.get_or_insert_with(|| {
                let name = format!("{} <preprocessed>", db.get_file(pp.top_file).path.display());
                db.add_source(name, pp.map.text.clone())
            });
            return Span::new(view, s as u32, e.max(s) as u32);
        }
        let fallback = self.pp.fallback_span(s, e);
        if !self.reported {
            self.reported = true;
            let snippet = self.pp.map.text.get(s..e).unwrap_or("");
            self.sink.emit(
                Diagnostic::error(
                    crate::diagnostic_code(),
                    format!(
                        "macro expansion produced the text `{snippet}`, which does not appear verbatim in any source file"
                    ),
                    fallback,
                )
                .with_help(
                    "parse this file with a mutable SourceDb (`parse_file_with_options`) so expanded text gets its own source view",
                ),
            );
        }
        fallback
    }

    fn map_diagnostic(&mut self, mut d: Diagnostic) -> Diagnostic {
        d.primary_span = self.map(d.primary_span);
        for label in &mut d.labels {
            label.span = self.map(label.span);
        }
        if let Some(fix) = &mut d.fix {
            for r in &mut fix.replacements {
                r.span = self.map(r.span);
            }
        }
        d
    }
}

/// Lexes preprocessed text and remaps every token span and lexer diagnostic
/// back into the `SourceDb`.
///
/// `lex` is the front end's lexer; it is handed the preprocessed text and a
/// private sink, and must produce spans tagged with [`Preprocessed::top_file`].
/// `span_of` gives mutable access to a token's span.
///
/// Returns the tokens with remapped spans plus, for each token, its byte range
/// in [`Preprocessed::text`], which is where the parser must read token text
/// from (a remapped span may point at a macro definition, not at the text).
///
/// Span policy: tokens copied verbatim from a file, macro body or macro
/// argument get their exact original span; tokens whose text exists in no
/// file (token pasting, stringification, a literal straddling a macro
/// boundary such as `` `W'b0 ``) get a span in a `<preprocessed>` view file
/// added to `source_db` on first need, so their span text still equals the
/// token text. Without a `source_db` such tokens get the macro invocation
/// site and one error is reported.
pub fn lex_mapped<T>(
    pp: &Preprocessed,
    source_db: Option<&mut SourceDb>,
    sink: &DiagnosticSink,
    lex: impl FnOnce(&str, &DiagnosticSink) -> Vec<T>,
    span_of: impl Fn(&mut T) -> &mut Span,
) -> (Vec<T>, Vec<(u32, u32)>) {
    let lex_sink = DiagnosticSink::new();
    let mut tokens = lex(pp.text(), &lex_sink);
    let mut mapper = Mapper {
        pp,
        db: source_db,
        view: None,
        reported: false,
        sink,
    };
    for d in lex_sink.take_all() {
        let d = mapper.map_diagnostic(d);
        sink.emit(d);
    }
    let mut ranges = Vec::with_capacity(tokens.len());
    for t in &mut tokens {
        let sp = span_of(t);
        ranges.push((sp.start, sp.end));
        *sp = mapper.map(*sp);
    }
    (tokens, ranges)
}
