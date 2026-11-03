//! The directive and text-macro engine.
//!
//! The engine scans a stack of input *frames* (the top-level file, included
//! files, macro expansions and macro arguments being pre-expanded) and copies
//! active text into a [`MappedText`] output, recording the origin of every
//! byte. Directives are interpreted and removed; macro usages are replaced by
//! their expansion, which is pushed as a new frame and rescanned.

use crate::mapped::{MappedText, Origin};
use crate::options::PreprocessOptions;
use crate::output::{DirectiveValue, Preprocessed};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, SourceDb, Span};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Maximum number of simultaneously active frames (nested macro expansions,
/// arguments and includes).
pub(crate) const MAX_EXPANSION_DEPTH: usize = 128;
/// Maximum number of macro expansions in one preprocessor run.
pub(crate) const MAX_EXPANSIONS: usize = 1_000_000;
/// Maximum number of bytes the preprocessor will emit for one run.
pub(crate) const MAX_OUTPUT_BYTES: usize = 256 << 20;

/// Names the engine treats as compiler directives rather than macros.
const DIRECTIVES: &[&str] = &[
    "define",
    "undef",
    "undefineall",
    "ifdef",
    "ifndef",
    "elsif",
    "else",
    "endif",
    "include",
    "timescale",
    "default_nettype",
    "resetall",
    "celldefine",
    "endcelldefine",
    "line",
    "pragma",
    "begin_keywords",
    "end_keywords",
    "unconnected_drive",
    "nounconnected_drive",
    "__FILE__",
    "__LINE__",
];

/// Legal arguments of `` `default_nettype ``.
const NET_TYPES: &[&str] = &[
    "wire", "tri", "tri0", "tri1", "wand", "triand", "wor", "trior", "trireg", "uwire", "none",
];

/// Access to the source database: mutable access allows loading includes.
pub(crate) enum DbAccess<'a> {
    /// Includes are loaded into the database and get their own `FileId`.
    Mut(&'a mut SourceDb),
    /// Read-only: `` `include `` is reported as unsupported.
    Ref(&'a SourceDb),
}

impl DbAccess<'_> {
    fn get(&self) -> &SourceDb {
        match self {
            DbAccess::Mut(db) => db,
            DbAccess::Ref(db) => db,
        }
    }
}

/// One formal parameter of a function-like macro.
struct Param {
    name: String,
    default: Option<MappedText>,
}

/// A text macro definition.
struct Macro {
    /// `None` for an object-like macro, `Some` (possibly empty) for a
    /// function-like one.
    params: Option<Vec<Param>>,
    body: MappedText,
}

enum FrameKind {
    /// A source file (the top-level file, an include, or the predefines file).
    File {
        path: PathBuf,
        canonical: Option<PathBuf>,
    },
    /// The expansion of macro `name`, invoked (outermost) at `site`.
    Macro { name: String, site: Span },
    /// Free-standing text: a macro argument being pre-expanded, or
    /// synthetic predefines.
    Text { site: Option<Span> },
}

struct Frame {
    text: Rc<MappedText>,
    pos: usize,
    run_start: usize,
    kind: FrameKind,
}

impl Frame {
    fn new(text: MappedText, kind: FrameKind) -> Self {
        Self {
            text: Rc::new(text),
            pos: 0,
            run_start: 0,
            kind,
        }
    }

    /// The outermost invocation site for text produced by this frame.
    fn site(&self) -> Option<Span> {
        match &self.kind {
            FrameKind::File { .. } => None,
            FrameKind::Macro { site, .. } => Some(*site),
            FrameKind::Text { site } => *site,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CondState {
    /// The current branch is being emitted.
    Active,
    /// No branch has been taken yet; a later `elsif`/`else` may activate.
    Seeking,
    /// A branch was already taken, or the enclosing region is inactive.
    Done,
}

struct Cond {
    state: CondState,
    seen_else: bool,
    span: Span,
}

/// Outcome of scanning a macro invocation's argument list.
enum ArgScan {
    /// No `(` follows the macro name.
    NoParen,
    /// The argument list runs off the end of the frame.
    Unterminated,
    /// Argument ranges (untrimmed) and the position after the closing `)`.
    Ok(Vec<std::ops::Range<usize>>, usize),
}

/// The preprocessor state for one top-level file.
pub(crate) struct Engine<'a> {
    db: DbAccess<'a>,
    options: &'a PreprocessOptions,
    sink: &'a DiagnosticSink,
    top_file: FileId,
    macros: HashMap<String, Rc<Macro>>,
    frames: Vec<Frame>,
    conds: Vec<Cond>,
    out: MappedText,
    emitted: usize,
    expansions: usize,
    limit_reported: bool,
    include_cache: HashMap<PathBuf, FileId>,
    included: Vec<FileId>,
    timescales: Vec<DirectiveValue>,
    default_nettypes: Vec<DirectiveValue>,
}

impl<'a> Engine<'a> {
    pub(crate) fn new(
        db: DbAccess<'a>,
        top_file: FileId,
        options: &'a PreprocessOptions,
        sink: &'a DiagnosticSink,
    ) -> Self {
        Self {
            db,
            options,
            sink,
            top_file,
            macros: HashMap::new(),
            frames: Vec::new(),
            conds: Vec::new(),
            out: MappedText::default(),
            emitted: 0,
            expansions: 0,
            limit_reported: false,
            include_cache: HashMap::new(),
            included: Vec::new(),
            timescales: Vec::new(),
            default_nettypes: Vec::new(),
        }
    }

    /// Preprocesses the top-level file and returns the mapped output.
    pub(crate) fn run(mut self) -> Preprocessed {
        self.load_predefines();
        let (content, path) = {
            let f = self.db.get().get_file(self.top_file);
            (f.content.clone(), f.path.clone())
        };
        let top_len = content.len() as u32;
        let canonical = std::fs::canonicalize(&path).ok();
        self.frames.push(Frame::new(
            MappedText::from_file(self.top_file, &content),
            FrameKind::File { path, canonical },
        ));
        self.run_until(0);
        for cond in std::mem::take(&mut self.conds) {
            self.sink.emit(
                Diagnostic::error(
                    crate::diagnostic_code(),
                    "missing `endif for this conditional",
                    cond.span,
                )
                .with_help("add a matching `endif"),
            );
        }
        Preprocessed {
            map: self.out,
            top_file: self.top_file,
            top_len,
            timescales: self.timescales,
            default_nettypes: self.default_nettypes,
            included_files: self.included,
        }
    }

    /// Processes the configured predefined macros as `` `define `` lines.
    fn load_predefines(&mut self) {
        if self.options.defines.is_empty() {
            return;
        }
        let mut text = String::new();
        for (name, body) in &self.options.defines {
            text.push_str("`define ");
            text.push_str(&name.replace('\n', " "));
            text.push(' ');
            text.push_str(&body.replace('\n', " "));
            text.push('\n');
        }
        let frame = match &mut self.db {
            DbAccess::Mut(db) => {
                let path = PathBuf::from("<predefined macros>");
                let id = db.add_source(path.clone(), text.clone());
                Frame::new(
                    MappedText::from_file(id, &text),
                    FrameKind::File {
                        path,
                        canonical: None,
                    },
                )
            }
            DbAccess::Ref(_) => Frame::new(
                MappedText::synthetic(&text, None),
                FrameKind::Text { site: None },
            ),
        };
        self.frames.push(frame);
        self.run_until(0);
        self.out = MappedText::default();
    }

    /// Runs the main scanning loop until the frame stack shrinks to `depth`.
    fn run_until(&mut self, depth: usize) {
        while self.frames.len() > depth {
            let fi = self.frames.len() - 1;
            if self.frames[fi].pos >= self.frames[fi].text.len() {
                self.flush(fi);
                self.pop_frame();
                continue;
            }
            if self.active() {
                self.step_active(fi);
            } else {
                self.step_inactive(fi);
            }
        }
    }

    fn active(&self) -> bool {
        self.conds
            .last()
            .is_none_or(|c| c.state == CondState::Active)
    }

    /// Copies the pending run of frame `fi` to the output.
    fn flush(&mut self, fi: usize) {
        let frame = &self.frames[fi];
        let pos = frame.pos.min(frame.text.len());
        let start = frame.run_start;
        let site = frame.site();
        let text = Rc::clone(&frame.text);
        if start < pos {
            self.out.push_slice(&text, start..pos, site);
            self.emitted += pos - start;
        }
        self.frames[fi].run_start = pos;
    }

    fn pop_frame(&mut self) {
        if let Some(f) = self.frames.pop() {
            if matches!(f.kind, FrameKind::File { .. }) && !self.frames.is_empty() {
                // Keep an included file's last line (e.g. a `//` comment
                // without a trailing newline) from swallowing the rest of
                // the including line.
                self.out.push("\n", Origin::Synthetic, None);
            }
        }
    }

    fn set_pos(&mut self, fi: usize, pos: usize) {
        let f = &mut self.frames[fi];
        f.pos = pos.min(f.text.len());
        f.run_start = f.pos;
    }

    fn step_active(&mut self, fi: usize) {
        let text = Rc::clone(&self.frames[fi].text);
        let bytes = text.text.as_bytes();
        let p = self.frames[fi].pos;
        let next = match bytes[p] {
            b'`' => {
                self.frames[fi].pos = p;
                self.flush(fi);
                self.directive(fi, &text, p);
                return;
            }
            b'"' => skip_string(bytes, p),
            b'/' if at(bytes, p + 1) == b'/' => line_end(bytes, p),
            b'/' if at(bytes, p + 1) == b'*' => skip_block_comment(bytes, p),
            b'\\' => skip_escaped(bytes, p),
            _ => skip_plain(bytes, p + 1),
        };
        self.frames[fi].pos = next;
    }

    fn step_inactive(&mut self, fi: usize) {
        let text = Rc::clone(&self.frames[fi].text);
        let bytes = text.text.as_bytes();
        let p = self.frames[fi].pos;
        let next = match bytes[p] {
            b'`' => {
                let (name, end) = read_ident(&text.text, p + 1);
                if matches!(name, "ifdef" | "ifndef" | "elsif" | "else" | "endif") {
                    self.directive(fi, &text, p);
                    return;
                }
                end.max(p + 1)
            }
            b'"' => skip_string(bytes, p),
            b'/' if at(bytes, p + 1) == b'/' => line_end(bytes, p),
            b'/' if at(bytes, p + 1) == b'*' => skip_block_comment(bytes, p),
            b'\\' => skip_escaped(bytes, p),
            _ => skip_plain(bytes, p + 1),
        };
        self.set_pos(fi, next);
    }

    // ------------------------------------------------------------------
    // Diagnostics helpers
    // ------------------------------------------------------------------

    /// Real span for `[a, b)` of frame `fi`: exact if possible, else the
    /// frame's invocation site, else the start of the top-level file.
    fn span_in(&self, fi: usize, a: usize, b: usize) -> Span {
        let f = &self.frames[fi];
        f.text
            .exact_span(a, b)
            .or_else(|| f.site())
            .or_else(|| f.text.site_at(a))
            .unwrap_or_else(|| Span::new(self.top_file, 0, 0))
    }

    /// Emits an error, adding an "in expansion of" note for macro frames.
    fn error(&self, fi: usize, span: Span, msg: impl Into<String>, help: Option<String>) {
        let mut d = Diagnostic::error(crate::diagnostic_code(), msg, span);
        for f in self.frames[..=fi.min(self.frames.len().saturating_sub(1))]
            .iter()
            .rev()
        {
            if let FrameKind::Macro { name, site } = &f.kind {
                let at = self.db.get().resolve_span(*site);
                d = d.with_note(format!("in expansion of macro `{name}` at {at}"));
                break;
            }
        }
        if let Some(h) = help {
            d = d.with_help(h);
        }
        self.sink.emit(d);
    }

    // ------------------------------------------------------------------
    // Directives
    // ------------------------------------------------------------------

    /// Handles the backtick at `p` in frame `fi`.
    fn directive(&mut self, fi: usize, text: &Rc<MappedText>, p: usize) {
        let (name, name_end) = read_ident(&text.text, p + 1);
        if name.is_empty() {
            if self.active() {
                let span = self.span_in(fi, p, p + 1);
                self.error(
                    fi,
                    span,
                    "stray '`': expected a compiler directive or macro name",
                    None,
                );
            }
            self.set_pos(fi, p + 1);
            return;
        }
        match name {
            "define" => self.do_define(fi, text, p, name_end),
            "undef" => {
                let (id, end) = read_ident(&text.text, skip_hspace(text.text.as_bytes(), name_end));
                if id.is_empty() {
                    let span = self.span_in(fi, p, name_end);
                    self.error(fi, span, "expected a macro name after `undef", None);
                    self.set_pos(fi, name_end);
                } else {
                    self.macros.remove(id);
                    self.set_pos(fi, end);
                }
            }
            "undefineall" => {
                self.macros.clear();
                self.set_pos(fi, name_end);
            }
            "ifdef" | "ifndef" => self.do_ifdef(fi, text, p, name_end, name == "ifndef"),
            "elsif" => self.do_elsif(fi, text, p, name_end),
            "else" => self.do_else(fi, p, name_end),
            "endif" => {
                if self.conds.pop().is_none() {
                    let span = self.span_in(fi, p, name_end);
                    self.error(
                        fi,
                        span,
                        "`endif without a matching `ifdef or `ifndef",
                        None,
                    );
                }
                self.set_pos(fi, name_end);
            }
            "include" => self.do_include(fi, text, p, name_end),
            "timescale" => self.do_timescale(fi, text, p, name_end),
            "default_nettype" => self.do_default_nettype(fi, text, p, name_end),
            "unconnected_drive" => {
                let (id, end) = read_ident(&text.text, skip_hspace(text.text.as_bytes(), name_end));
                if !matches!(id, "pull0" | "pull1") {
                    let span = self.span_in(fi, p, end.max(name_end));
                    self.error(
                        fi,
                        span,
                        "expected `pull0` or `pull1` after `unconnected_drive",
                        None,
                    );
                }
                self.set_pos(fi, end.max(name_end));
            }
            "line" | "pragma" => {
                let end = line_end(text.text.as_bytes(), name_end);
                self.set_pos(fi, end);
            }
            "begin_keywords" => {
                let bytes = text.text.as_bytes();
                let q = skip_hspace(bytes, name_end);
                if at(bytes, q) == b'"' {
                    self.set_pos(fi, skip_string(bytes, q));
                } else {
                    let span = self.span_in(fi, p, name_end);
                    self.error(
                        fi,
                        span,
                        "expected a version string such as \"1800-2017\" after `begin_keywords",
                        None,
                    );
                    self.set_pos(fi, name_end);
                }
            }
            "resetall"
            | "celldefine"
            | "endcelldefine"
            | "nounconnected_drive"
            | "end_keywords" => self.set_pos(fi, name_end),
            "__FILE__" | "__LINE__" => {
                let span = self.span_in(fi, p, name_end);
                let site = self.frames[fi].site().unwrap_or(span);
                let value = if name == "__FILE__" {
                    let path = self.current_path(fi).unwrap_or_default();
                    let path = path.display().to_string();
                    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
                } else {
                    self.db.get().resolve_span(site).start_line.to_string()
                };
                self.out.push(&value, Origin::Synthetic, Some(site));
                self.set_pos(fi, name_end);
            }
            _ => self.do_macro_usage(fi, text, p, name, name_end),
        }
    }

    fn do_ifdef(
        &mut self,
        fi: usize,
        text: &Rc<MappedText>,
        start: usize,
        name_end: usize,
        negate: bool,
    ) {
        let (id, end) = read_ident(&text.text, skip_hspace(text.text.as_bytes(), name_end));
        let end = end.max(name_end);
        let span = self.span_in(fi, start, end);
        let parent_active = self.active();
        if id.is_empty() && parent_active {
            let kw = if negate { "ifndef" } else { "ifdef" };
            self.error(fi, span, format!("expected a macro name after `{kw}"), None);
        }
        let state = if !parent_active {
            CondState::Done
        } else if (!id.is_empty() && self.macros.contains_key(id)) != negate {
            CondState::Active
        } else {
            CondState::Seeking
        };
        self.conds.push(Cond {
            state,
            seen_else: false,
            span,
        });
        self.set_pos(fi, end);
    }

    fn do_elsif(&mut self, fi: usize, text: &Rc<MappedText>, start: usize, name_end: usize) {
        let (id, end) = read_ident(&text.text, skip_hspace(text.text.as_bytes(), name_end));
        let end = end.max(name_end);
        let span = self.span_in(fi, start, end);
        let defined = !id.is_empty() && self.macros.contains_key(id);
        match self.conds.last_mut() {
            None => self.error(
                fi,
                span,
                "`elsif without a matching `ifdef or `ifndef",
                None,
            ),
            Some(c) => {
                let after_else = c.seen_else;
                c.state = match c.state {
                    CondState::Active | CondState::Done => CondState::Done,
                    CondState::Seeking if defined => CondState::Active,
                    CondState::Seeking => CondState::Seeking,
                };
                if after_else {
                    self.error(fi, span, "`elsif after `else", None);
                } else if id.is_empty() {
                    self.error(fi, span, "expected a macro name after `elsif", None);
                }
            }
        }
        self.set_pos(fi, end);
    }

    fn do_else(&mut self, fi: usize, start: usize, name_end: usize) {
        let span = self.span_in(fi, start, name_end);
        match self.conds.last_mut() {
            None => self.error(fi, span, "`else without a matching `ifdef or `ifndef", None),
            Some(c) => {
                let duplicate = c.seen_else;
                c.seen_else = true;
                c.state = match c.state {
                    CondState::Seeking if !duplicate => CondState::Active,
                    _ => CondState::Done,
                };
                if duplicate {
                    self.error(fi, span, "duplicate `else in the same conditional", None);
                }
            }
        }
        self.set_pos(fi, name_end);
    }

    fn do_timescale(&mut self, fi: usize, text: &Rc<MappedText>, start: usize, name_end: usize) {
        let bytes = text.text.as_bytes();
        let mut end = line_end(bytes, name_end);
        if let Some(c) = text.text[name_end..end].find("//") {
            end = name_end + c;
        }
        if let Some(c) = text.text[name_end..end].find("/*") {
            end = name_end + c;
        }
        let r = text.trim_range(name_end..end);
        let value = text.text[r.clone()].to_string();
        let span = self.span_in(fi, start, r.end.max(name_end));
        if value.is_empty() || !value.contains('/') {
            self.error(
                fi,
                span,
                "expected `timescale <time unit> / <time precision>",
                Some("for example: `timescale 1ns / 1ps".into()),
            );
        } else {
            self.timescales.push(DirectiveValue { value, span });
        }
        self.set_pos(fi, r.end.max(name_end));
    }

    fn do_default_nettype(
        &mut self,
        fi: usize,
        text: &Rc<MappedText>,
        start: usize,
        name_end: usize,
    ) {
        let (id, end) = read_ident(&text.text, skip_hspace(text.text.as_bytes(), name_end));
        let end = end.max(name_end);
        let span = self.span_in(fi, start, end);
        if NET_TYPES.contains(&id) {
            self.default_nettypes.push(DirectiveValue {
                value: id.to_string(),
                span,
            });
        } else {
            self.error(
                fi,
                span,
                format!("expected a net type or `none` after `default_nettype, found `{id}`"),
                Some(format!("valid values are: {}", NET_TYPES.join(", "))),
            );
        }
        self.set_pos(fi, end);
    }

    // ------------------------------------------------------------------
    // `define
    // ------------------------------------------------------------------

    fn do_define(&mut self, fi: usize, text: &Rc<MappedText>, start: usize, name_end: usize) {
        let bytes = text.text.as_bytes();
        let p = skip_hspace(bytes, name_end);
        let (name, mend) = read_ident(&text.text, p);
        if name.is_empty() {
            let span = self.span_in(fi, start, name_end);
            self.error(fi, span, "expected a macro name after `define", None);
            self.set_pos(fi, logical_line_end(bytes, p));
            return;
        }
        if DIRECTIVES.contains(&name) {
            let span = self.span_in(fi, p, mend);
            self.error(
                fi,
                span,
                format!("`{name}` is a compiler directive and cannot be defined as a macro"),
                None,
            );
            self.set_pos(fi, logical_line_end(bytes, p));
            return;
        }
        let mut p = mend;
        let params = if at(bytes, p) == b'(' {
            match self.parse_params(fi, text, p) {
                Some((params, np)) => {
                    p = np;
                    Some(params)
                }
                None => {
                    self.set_pos(fi, logical_line_end(bytes, p));
                    return;
                }
            }
        } else {
            None
        };
        let (body, end) = read_body(text, p);
        self.macros
            .insert(name.to_string(), Rc::new(Macro { params, body }));
        self.set_pos(fi, end);
    }

    /// Parses `(a, b = default, ...)` starting at the `(` at `p`.
    fn parse_params(
        &mut self,
        fi: usize,
        text: &Rc<MappedText>,
        p: usize,
    ) -> Option<(Vec<Param>, usize)> {
        let bytes = text.text.as_bytes();
        let mut p = p + 1;
        let mut params: Vec<Param> = Vec::new();
        loop {
            p = skip_define_space(bytes, p);
            if at(bytes, p) == b')' && params.is_empty() {
                return Some((params, p + 1));
            }
            let (pname, pend) = read_ident(&text.text, p);
            if pname.is_empty() {
                let span = self.span_in(fi, p, (p + 1).min(text.len()));
                self.error(
                    fi,
                    span,
                    "expected a parameter name in the macro parameter list",
                    None,
                );
                return None;
            }
            if params.iter().any(|q| q.name == pname) {
                let span = self.span_in(fi, p, pend);
                self.error(
                    fi,
                    span,
                    format!("duplicate macro parameter `{pname}`"),
                    None,
                );
            }
            p = skip_define_space(bytes, pend);
            let mut default = None;
            if at(bytes, p) == b'=' {
                let dend = scan_default(bytes, p + 1);
                default = Some(text.slice(text.trim_range(p + 1..dend)));
                p = dend;
            }
            params.push(Param {
                name: pname.to_string(),
                default,
            });
            match at(bytes, p) {
                b',' => p += 1,
                b')' => return Some((params, p + 1)),
                _ => {
                    let span = self.span_in(fi, p, (p + 1).min(text.len()));
                    self.error(
                        fi,
                        span,
                        "expected `,` or `)` in the macro parameter list",
                        None,
                    );
                    return None;
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // `include
    // ------------------------------------------------------------------

    fn current_path(&self, fi: usize) -> Option<PathBuf> {
        self.frames[..=fi].iter().rev().find_map(|f| match &f.kind {
            FrameKind::File { path, .. } => Some(path.clone()),
            _ => None,
        })
    }

    fn do_include(&mut self, fi: usize, text: &Rc<MappedText>, start: usize, name_end: usize) {
        let bytes = text.text.as_bytes();
        let p = skip_hspace(bytes, name_end);
        let eol = line_end(bytes, p);
        let (fname, angle, end) = match at(bytes, p) {
            q @ (b'"' | b'<') => {
                let close = if q == b'"' { b'"' } else { b'>' };
                match bytes[p + 1..eol].iter().position(|&b| b == close) {
                    Some(off) => (
                        text.text[p + 1..p + 1 + off].to_string(),
                        q == b'<',
                        p + off + 2,
                    ),
                    None => {
                        let span = self.span_in(fi, start, eol);
                        self.error(fi, span, "unterminated file name in `include", None);
                        self.set_pos(fi, eol);
                        return;
                    }
                }
            }
            b'`' => {
                let (m, e) = read_ident(&text.text, p + 1);
                let body = self
                    .macros
                    .get(m)
                    .filter(|mac| mac.params.is_none())
                    .map(|mac| mac.body.text.trim().to_string());
                match body {
                    Some(b) if b.len() >= 2 && b.starts_with('"') && b.ends_with('"') => {
                        (b[1..b.len() - 1].to_string(), false, e)
                    }
                    _ => {
                        let span = self.span_in(fi, start, e);
                        self.error(
                            fi,
                            span,
                            format!("macro `{m}` does not expand to a quoted file name"),
                            None,
                        );
                        self.set_pos(fi, e.max(p + 1));
                        return;
                    }
                }
            }
            _ => {
                let span = self.span_in(fi, start, name_end);
                self.error(
                    fi,
                    span,
                    "expected a quoted file name after `include",
                    Some("write `include \"file.vh\"".into()),
                );
                self.set_pos(fi, name_end);
                return;
            }
        };
        let span = self.span_in(fi, start, end);
        self.set_pos(fi, end);
        if fname.trim().is_empty() {
            self.error(fi, span, "empty file name in `include", None);
            return;
        }
        if matches!(self.db, DbAccess::Ref(_)) {
            self.error(
                fi,
                span,
                format!("cannot include `{fname}`: this entry point has a read-only SourceDb"),
                Some(
                    "use `parse_file_with_options` with a mutable SourceDb to enable `include"
                        .into(),
                ),
            );
            return;
        }

        let rel = Path::new(&fname);
        let mut candidates = Vec::new();
        if rel.is_absolute() {
            candidates.push(rel.to_path_buf());
        } else {
            if !angle {
                let dir = self
                    .current_path(fi)
                    .and_then(|p| p.parent().map(Path::to_path_buf))
                    .unwrap_or_default();
                candidates.push(dir.join(rel));
            }
            for d in &self.options.include_dirs {
                candidates.push(d.join(rel));
            }
        }
        let Some(found) = candidates.iter().find(|c| c.is_file()).cloned() else {
            let searched: Vec<String> = candidates
                .iter()
                .map(|c| c.parent().unwrap_or(Path::new("")).display().to_string())
                .map(|s| if s.is_empty() { ".".into() } else { s })
                .collect();
            let mut d = Diagnostic::error(
                crate::diagnostic_code(),
                format!("cannot find include file `{fname}`"),
                span,
            )
            .with_help("add the directory containing it to the include search path");
            if !searched.is_empty() {
                d = d.with_note(format!("searched: {}", searched.join(", ")));
            }
            self.sink.emit(d);
            return;
        };
        let canonical = std::fs::canonicalize(&found).ok();
        if let Some(c) = &canonical {
            let on_stack = self
                .frames
                .iter()
                .any(|f| matches!(&f.kind, FrameKind::File { canonical: Some(x), .. } if x == c));
            if on_stack {
                let mut chain: Vec<String> = self
                    .frames
                    .iter()
                    .filter_map(|f| match &f.kind {
                        FrameKind::File { path, .. } => Some(path.display().to_string()),
                        _ => None,
                    })
                    .collect();
                chain.push(found.display().to_string());
                self.sink.emit(
                    Diagnostic::error(
                        crate::diagnostic_code(),
                        format!("include cycle: `{fname}` is already being included"),
                        span,
                    )
                    .with_note(format!("include chain: {}", chain.join(" -> "))),
                );
                return;
            }
        }
        let depth = self
            .frames
            .iter()
            .filter(|f| matches!(f.kind, FrameKind::File { .. }))
            .count();
        if depth > self.options.max_include_depth || self.frames.len() >= MAX_EXPANSION_DEPTH {
            self.error(
                fi,
                span,
                format!(
                    "`include nesting exceeds the limit of {} levels",
                    self.options.max_include_depth
                ),
                None,
            );
            return;
        }
        let cached = canonical
            .as_ref()
            .and_then(|c| self.include_cache.get(c).copied());
        let file_id = match cached {
            Some(id) => id,
            None => {
                let DbAccess::Mut(db) = &mut self.db else {
                    return;
                };
                match db.load_file(&found) {
                    Ok(id) => {
                        if let Some(c) = &canonical {
                            self.include_cache.insert(c.clone(), id);
                        }
                        id
                    }
                    Err(e) => {
                        self.error(
                            fi,
                            span,
                            format!("cannot read include file `{}`: {e}", found.display()),
                            None,
                        );
                        return;
                    }
                }
            }
        };
        if !self.included.contains(&file_id) {
            self.included.push(file_id);
        }
        let content = self.db.get().get_file(file_id).content.clone();
        self.frames.push(Frame::new(
            MappedText::from_file(file_id, &content),
            FrameKind::File {
                path: found,
                canonical,
            },
        ));
    }

    // ------------------------------------------------------------------
    // Macro usage
    // ------------------------------------------------------------------

    fn do_macro_usage(
        &mut self,
        fi: usize,
        text: &Rc<MappedText>,
        start: usize,
        name: &str,
        name_end: usize,
    ) {
        let Some(mac) = self.macros.get(name).cloned() else {
            let span = self.span_in(fi, start, name_end);
            self.error(
                fi,
                span,
                format!("undefined macro or unknown compiler directive `{name}"),
                Some(format!(
                    "define it with `define {name} ..., or as a predefined macro (like +define+{name})"
                )),
            );
            self.set_pos(fi, name_end);
            return;
        };
        let (arg_ranges, end) = match &mac.params {
            None => (Vec::new(), name_end),
            Some(_) => match scan_args(text.text.as_bytes(), name_end) {
                ArgScan::Ok(ranges, end) => (ranges, end),
                ArgScan::NoParen => {
                    let span = self.span_in(fi, start, name_end);
                    self.error(
                        fi,
                        span,
                        format!("macro `{name}` takes arguments, but no argument list follows"),
                        None,
                    );
                    self.set_pos(fi, name_end);
                    return;
                }
                ArgScan::Unterminated => {
                    let span = self.span_in(fi, start, name_end);
                    self.error(
                        fi,
                        span,
                        format!("unterminated argument list for macro `{name}`"),
                        None,
                    );
                    self.set_pos(fi, text.len());
                    return;
                }
            },
        };
        let inv_span = self.span_in(fi, start, end);
        self.set_pos(fi, end);

        let recursive = self
            .frames
            .iter()
            .any(|f| matches!(&f.kind, FrameKind::Macro { name: n, .. } if n == name));
        if recursive {
            self.error(
                fi,
                inv_span,
                format!("recursive expansion of macro `{name}`"),
                Some("a macro must not expand to an invocation of itself".into()),
            );
            return;
        }
        if self.frames.len() >= MAX_EXPANSION_DEPTH {
            self.error(
                fi,
                inv_span,
                format!("macro expansion nested more than {MAX_EXPANSION_DEPTH} levels deep"),
                None,
            );
            return;
        }
        if self.expansions >= MAX_EXPANSIONS || self.emitted >= MAX_OUTPUT_BYTES {
            if !self.limit_reported {
                self.limit_reported = true;
                self.error(
                    fi,
                    inv_span,
                    "macro expansion limit exceeded; further macro usages are ignored",
                    None,
                );
            }
            return;
        }
        self.expansions += 1;

        let site = self.frames[fi].site().unwrap_or(inv_span);
        let mut actuals = Vec::with_capacity(arg_ranges.len());
        for r in arg_ranges {
            let arg = text.slice(text.trim_range(r));
            actuals.push(self.expand_text(arg, site));
        }
        let body = self.substitute(fi, &mac, name, actuals, inv_span, site);
        self.frames.push(Frame::new(
            body,
            FrameKind::Macro {
                name: name.to_string(),
                site,
            },
        ));
    }

    /// Fully macro-expands `text` (a macro argument) in the current context.
    fn expand_text(&mut self, text: MappedText, site: Span) -> MappedText {
        if !text.text.contains('`') {
            return text;
        }
        let saved = std::mem::take(&mut self.out);
        let depth = self.frames.len();
        self.frames
            .push(Frame::new(text, FrameKind::Text { site: Some(site) }));
        self.run_until(depth);
        std::mem::replace(&mut self.out, saved)
    }

    /// Builds the expansion text of `mac`: parameters replaced by actual
    /// arguments (or defaults), `` `` `` pasting, `` `" `` and `` `\`" ``.
    fn substitute(
        &mut self,
        fi: usize,
        mac: &Macro,
        name: &str,
        mut actuals: Vec<MappedText>,
        inv_span: Span,
        site: Span,
    ) -> MappedText {
        let params: &[Param] = mac.params.as_deref().unwrap_or(&[]);
        if params.is_empty() && actuals.len() == 1 && actuals[0].text.trim().is_empty() {
            actuals.clear();
        }
        if actuals.len() > params.len() {
            self.error(
                fi,
                inv_span,
                format!(
                    "macro `{name}` takes {} argument(s) but {} were given",
                    params.len(),
                    actuals.len()
                ),
                None,
            );
            actuals.truncate(params.len());
        }
        let given = actuals.len();
        let mut values: Vec<MappedText> = Vec::with_capacity(params.len());
        let mut actuals = actuals.into_iter();
        for (i, p) in params.iter().enumerate() {
            let actual = actuals.next().filter(|a| !a.text.trim().is_empty());
            match (actual, &p.default) {
                (Some(a), _) => values.push(a),
                (None, Some(d)) => values.push(d.clone()),
                (None, None) => {
                    if i >= given {
                        self.error(
                            fi,
                            inv_span,
                            format!(
                                "missing argument for parameter `{}` of macro `{name}`",
                                p.name
                            ),
                            None,
                        );
                    }
                    values.push(MappedText::default());
                }
            }
        }

        let body = &mac.body;
        let bytes = body.text.as_bytes();
        let len = bytes.len();
        let mut out = MappedText::default();
        let mut run = 0;
        let mut p = 0;
        while p < len {
            let b = bytes[p];
            if b == b'`' {
                let special: Option<(usize, &str)> = match (at(bytes, p + 1), at(bytes, p + 2)) {
                    (b'`', _) => Some((2, "")),
                    (b'"', _) => Some((2, "\"")),
                    (b'\\', b'`') if at(bytes, p + 3) == b'"' => Some((4, "\\\"")),
                    _ => None,
                };
                if let Some((skip, repl)) = special {
                    out.push_slice(body, run..p, None);
                    out.push(repl, Origin::Synthetic, Some(site));
                    p += skip;
                    run = p;
                } else {
                    let (_, e) = read_ident(&body.text, p + 1);
                    p = e.max(p + 1);
                }
                continue;
            }
            if b == b'"' {
                p = skip_string(bytes, p);
                continue;
            }
            if b == b'\\' {
                p = skip_escaped(bytes, p);
                continue;
            }
            if b == b'/' && at(bytes, p + 1) == b'*' {
                p = skip_block_comment(bytes, p);
                continue;
            }
            if is_ident_start(b) {
                let (w, e) = read_ident(&body.text, p);
                if let Some(i) = params.iter().position(|q| q.name == w) {
                    out.push_slice(body, run..p, None);
                    let v = &values[i];
                    out.push_slice(v, 0..v.len(), None);
                    run = e;
                }
                p = e;
                continue;
            }
            if b.is_ascii_digit() {
                p += 1;
                while p < len && is_ident_char(bytes[p]) {
                    p += 1;
                }
                continue;
            }
            p += 1;
        }
        out.push_slice(body, run..len, None);
        out
    }
}

// ----------------------------------------------------------------------
// Byte-level scanning helpers. Every returned position is either the text
// length or the index of an ASCII byte / UTF-8 char start, so slicing at
// these positions never splits a character.
// ----------------------------------------------------------------------

fn at(bytes: &[u8], i: usize) -> u8 {
    bytes.get(i).copied().unwrap_or(0)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Advances past one character starting at `i` (handles multi-byte UTF-8).
fn next_char(bytes: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < bytes.len() && (bytes[j] & 0xC0) == 0x80 {
        j += 1;
    }
    j.min(bytes.len())
}

/// Reads an identifier starting at `p`; returns `("", p)` if there is none.
fn read_ident(text: &str, p: usize) -> (&str, usize) {
    let bytes = text.as_bytes();
    if p >= bytes.len() || !is_ident_start(bytes[p]) {
        return ("", p.min(bytes.len()));
    }
    let mut e = p + 1;
    while e < bytes.len() && is_ident_char(bytes[e]) {
        e += 1;
    }
    (&text[p..e], e)
}

/// Skips ordinary bytes up to the next byte the scanner must look at.
fn skip_plain(bytes: &[u8], mut p: usize) -> usize {
    while p < bytes.len() && !matches!(bytes[p], b'`' | b'"' | b'/' | b'\\') {
        p += 1;
    }
    p
}

/// Skips a string literal starting at the `"` at `p`. Stops after the
/// closing quote, or at an unescaped newline / end of text.
fn skip_string(bytes: &[u8], p: usize) -> usize {
    let mut i = p + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i = next_char(bytes, i + 1).max(i + 1),
            b'"' => return i + 1,
            b'\n' => return i,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Returns the position of the next `\n` at or after `p` (or the end).
fn line_end(bytes: &[u8], p: usize) -> usize {
    let p = p.min(bytes.len());
    bytes[p..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |o| p + o)
}

/// Like [`line_end`], but follows `\`-newline continuations.
fn logical_line_end(bytes: &[u8], mut p: usize) -> usize {
    loop {
        let e = line_end(bytes, p);
        if e < bytes.len() && ends_with_backslash(bytes, e) {
            p = e + 1;
        } else {
            return e;
        }
    }
}

/// `true` if the line ending at the `\n` at `nl` ends with a backslash.
fn ends_with_backslash(bytes: &[u8], nl: usize) -> bool {
    match nl.checked_sub(1).map(|i| bytes[i]) {
        Some(b'\\') => true,
        Some(b'\r') => nl >= 2 && bytes[nl - 2] == b'\\',
        _ => false,
    }
}

fn skip_block_comment(bytes: &[u8], p: usize) -> usize {
    let mut i = p + 2;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 2;
        }
        i += 1;
    }
    bytes.len()
}

/// Skips an escaped identifier (`\name` up to whitespace).
fn skip_escaped(bytes: &[u8], p: usize) -> usize {
    let mut i = p + 1;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Skips spaces, tabs and carriage returns (not newlines).
fn skip_hspace(bytes: &[u8], mut p: usize) -> usize {
    while p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\r') {
        p += 1;
    }
    p
}

/// If a `\`-newline continuation starts at `p`, returns the index of its `\n`.
fn continuation_newline(bytes: &[u8], p: usize) -> Option<usize> {
    if at(bytes, p) != b'\\' {
        return None;
    }
    match at(bytes, p + 1) {
        b'\n' => Some(p + 1),
        b'\r' if at(bytes, p + 2) == b'\n' => Some(p + 2),
        _ => None,
    }
}

/// Skips horizontal whitespace and line continuations inside a `define line.
fn skip_define_space(bytes: &[u8], mut p: usize) -> usize {
    loop {
        p = skip_hspace(bytes, p);
        match continuation_newline(bytes, p) {
            Some(nl) => p = nl + 1,
            None => return p,
        }
    }
}

/// Scans a parameter default value up to a top-level `,` or `)`.
fn scan_default(bytes: &[u8], mut p: usize) -> usize {
    let mut depth = 0usize;
    while p < bytes.len() {
        match bytes[p] {
            b'\n' => return p,
            b'"' => {
                p = skip_string(bytes, p);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth > 0 => depth -= 1,
            b',' | b')' if depth == 0 => return p,
            _ => {}
        }
        p += 1;
    }
    p
}

/// Reads a macro body starting at `p`, up to the end of the logical line.
///
/// `\`-newline continuations become plain newlines and `//` comments are
/// dropped. Returns the trimmed body and the position of the terminating
/// newline (which is left in the input).
fn read_body(text: &MappedText, p: usize) -> (MappedText, usize) {
    let bytes = text.text.as_bytes();
    let mut p = skip_hspace(bytes, p);
    let mut body = MappedText::default();
    let mut run = p;
    loop {
        if p >= bytes.len() {
            body.push_slice(text, run..p, None);
            break;
        }
        if let Some(nl) = continuation_newline(bytes, p) {
            body.push_slice(text, run..p, None);
            body.push_slice(text, nl..nl + 1, None);
            p = nl + 1;
            run = p;
            continue;
        }
        match bytes[p] {
            b'\n' => {
                body.push_slice(text, run..p, None);
                break;
            }
            b'/' if at(bytes, p + 1) == b'/' => {
                body.push_slice(text, run..p, None);
                let e = line_end(bytes, p);
                if e < bytes.len() && ends_with_backslash(bytes, e) {
                    body.push_slice(text, e..e + 1, None);
                    p = e + 1;
                    run = p;
                } else {
                    p = e;
                    run = p;
                    break;
                }
            }
            b'/' if at(bytes, p + 1) == b'*' => p = skip_block_comment(bytes, p),
            b'"' => p = skip_string(bytes, p),
            b'\\' => p = skip_escaped(bytes, p),
            _ => p = next_char(bytes, p),
        }
    }
    let _ = run;
    (body.trimmed(), p)
}

/// Scans `( arg, arg, ... )` after a macro name ending at `p`.
fn scan_args(bytes: &[u8], p: usize) -> ArgScan {
    let mut i = p;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if at(bytes, i) != b'(' {
        return ArgScan::NoParen;
    }
    i += 1;
    let mut depth = 0usize;
    let mut arg_start = i;
    let mut ranges = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = skip_string(bytes, i);
                continue;
            }
            b'/' if at(bytes, i + 1) == b'/' => {
                i = line_end(bytes, i);
                continue;
            }
            b'/' if at(bytes, i + 1) == b'*' => {
                i = skip_block_comment(bytes, i);
                continue;
            }
            b'\\' => {
                i = skip_escaped(bytes, i);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' if depth == 0 => {
                ranges.push(arg_start..i);
                return ArgScan::Ok(ranges, i + 1);
            }
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                ranges.push(arg_start..i);
                arg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    ArgScan::Unterminated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_ident_variants() {
        assert_eq!(read_ident("abc$1 x", 0), ("abc$1", 5));
        assert_eq!(read_ident("9abc", 0), ("", 0));
        assert_eq!(read_ident("ab", 5), ("", 2));
    }

    #[test]
    fn skip_string_handles_escapes_and_eol() {
        let b = br#""a\"b" x"#;
        assert_eq!(skip_string(b, 0), 6);
        let b = b"\"abc\nd";
        assert_eq!(skip_string(b, 0), 4);
        let b = "\"\\é\"".as_bytes();
        assert_eq!(skip_string(b, 0), b.len());
    }

    #[test]
    fn logical_line_end_follows_continuations() {
        let b = b"a \\\nb \\\r\nc\nd";
        assert_eq!(logical_line_end(b, 0), 10);
        assert_eq!(logical_line_end(b"abc", 0), 3);
    }

    #[test]
    fn scan_args_splits_top_level_commas() {
        let b = b"(a, f(b, c), {d, e}, \"x,y\") rest";
        match scan_args(b, 0) {
            ArgScan::Ok(r, end) => {
                assert_eq!(r.len(), 4);
                assert_eq!(&b[end..], b" rest");
            }
            _ => panic!("expected args"),
        }
        assert!(matches!(scan_args(b"  x", 0), ArgScan::NoParen));
        assert!(matches!(scan_args(b"(a, (b", 0), ArgScan::Unterminated));
    }

    #[test]
    fn read_body_handles_continuation_and_comments() {
        let m = MappedText::from_file(FileId::from_raw(0), "  a + \\\n  b // c\nnext");
        let (body, end) = read_body(&m, 0);
        assert_eq!(body.text, "a + \n  b");
        assert_eq!(&m.text[end..], "\nnext");
    }

    #[test]
    fn scan_default_respects_nesting() {
        let b = b"f(1,2), y)";
        assert_eq!(scan_default(b, 0), 6);
        assert_eq!(scan_default(b"abc\n", 0), 3);
    }
}
