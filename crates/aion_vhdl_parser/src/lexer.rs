//! Lexical analyzer for VHDL-2008 source text.
//!
//! Converts source text into a sequence of [`Token`]s, handling case-insensitive
//! keywords, based literals, string/character/bit-string literals, delimited
//! (`/* ... */`) and line comments, and extended identifiers. Errors are reported
//! to the [`DiagnosticSink`] and produce [`VhdlToken::Error`] tokens.
//!
//! The lexer follows IEEE 1076-2008 clause 15:
//!
//! - Delimited comments end at the first `*/` and do not nest (§15.9).
//! - Based literals are validated: the base must be 2 to 16 and every digit
//!   must be less than the base (§15.5.3). `:` may replace both `#` (§15.10).
//! - Bit-string literals may carry a length and a signedness prefix, such as
//!   `8UX"FF"` or `12SB"101"` (§15.8); they are validated with
//!   [`expand_bit_string`].
//! - An apostrophe directly after a name, `)`, `all`, or a string literal is an
//!   attribute/qualification tick; elsewhere `'c'` is a character literal, so
//!   `character'(' ')` lexes as name, tick, `(`, char literal, `)`.
//! - `!` is accepted as a replacement for `|` (§15.10).
//! - Non-ASCII characters outside literals and comments are reported once per
//!   character, with a hint when they look like typographic quotes or dashes.

use crate::bit_string::expand_bit_string;
use crate::token::{lookup_keyword, Token, VhdlToken};
use aion_diagnostics::code::{Category, DiagnosticCode};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, Span};

/// Lexes the given source text into a vector of tokens.
///
/// Whitespace and comments are skipped. The returned vector always ends with
/// a [`VhdlToken::Eof`] token. Lexer errors are reported via the diagnostic
/// sink and produce [`VhdlToken::Error`] tokens in the output; every token span
/// starts and ends on a UTF-8 character boundary of `source`.
pub fn lex(source: &str, file: FileId, sink: &DiagnosticSink) -> Vec<Token> {
    let mut lexer = Lexer {
        source: source.as_bytes(),
        pos: 0,
        file,
        sink,
        last_kind: None,
    };
    lexer.lex_all()
}

struct Lexer<'a> {
    source: &'a [u8],
    pos: usize,
    file: FileId,
    sink: &'a DiagnosticSink,
    /// Kind of the most recently emitted token, used to tell an attribute
    /// tick (`x'length`) from a character literal (`'1'`).
    last_kind: Option<VhdlToken>,
}

impl Lexer<'_> {
    fn lex_all(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            if self.pos >= self.source.len() {
                tokens.push(Token {
                    kind: VhdlToken::Eof,
                    span: Span::new(self.file, self.pos as u32, self.pos as u32),
                });
                break;
            }
            let token = self.next_token();
            self.last_kind = Some(token.kind);
            tokens.push(token);
        }
        tokens
    }

    fn peek(&self) -> u8 {
        if self.pos < self.source.len() {
            self.source[self.pos]
        } else {
            0
        }
    }

    fn peek_at(&self, offset: usize) -> u8 {
        let idx = self.pos + offset;
        if idx < self.source.len() {
            self.source[idx]
        } else {
            0
        }
    }

    fn advance(&mut self) -> u8 {
        let b = self.source[self.pos];
        self.pos += 1;
        b
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(self.file, start as u32, self.pos as u32)
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.file, start as u32, end as u32)
    }

    fn error(&self, msg: &str, span: Span) {
        self.sink.emit(Diagnostic::error(
            DiagnosticCode::new(Category::Error, 100),
            msg,
            span,
        ));
    }

    fn error_with_help(&self, msg: &str, span: Span, help: &str) {
        self.sink.emit(
            Diagnostic::error(DiagnosticCode::new(Category::Error, 100), msg, span).with_help(help),
        );
    }

    /// Decodes the UTF-8 character starting at byte `pos`.
    ///
    /// Returns the character and its length in bytes. Never panics: if the
    /// bytes at `pos` are not a valid UTF-8 sequence (which cannot happen for
    /// input that came from a `&str`, but is handled anyway), returns U+FFFD
    /// and a length that skips to the next character boundary.
    fn char_at(&self, pos: usize) -> (char, usize) {
        let b = self.source[pos];
        let len = match b {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        let end = (pos + len).min(self.source.len());
        if let Ok(s) = std::str::from_utf8(&self.source[pos..end]) {
            if let Some(c) = s.chars().next() {
                return (c, c.len_utf8());
            }
        }
        let mut skip = 1;
        while pos + skip < self.source.len() && (0x80..=0xBF).contains(&self.source[pos + skip]) {
            skip += 1;
        }
        ('\u{FFFD}', skip)
    }

    /// Returns the length in bytes of a separator (whitespace) at the current
    /// position, or 0 if there is none. Besides ASCII whitespace this accepts
    /// vertical tab and the non-breaking space (U+00A0), which VHDL treats as a
    /// space character (§15.3).
    fn separator_len(&self) -> usize {
        match self.peek() {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => 1,
            0xC2 if self.peek_at(1) == 0xA0 => 2,
            _ => 0,
        }
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip separators
            while self.pos < self.source.len() {
                let len = self.separator_len();
                if len == 0 {
                    break;
                }
                self.pos += len;
            }
            if self.pos >= self.source.len() {
                return;
            }
            // Line comment: --
            if self.peek() == b'-' && self.peek_at(1) == b'-' {
                self.pos += 2;
                while self.pos < self.source.len() && self.source[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            // Delimited comment: /* ... */ (VHDL-2008). Per LRM §15.9 it ends at
            // the first `*/`; delimited comments do not nest.
            if self.peek() == b'/' && self.peek_at(1) == b'*' {
                let start = self.pos;
                self.pos += 2;
                loop {
                    if self.pos + 1 >= self.source.len() {
                        self.pos = self.source.len();
                        self.error("unterminated block comment", self.span_from(start));
                        break;
                    }
                    if self.source[self.pos] == b'*' && self.source[self.pos + 1] == b'/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn next_token(&mut self) -> Token {
        let start = self.pos;
        let b = self.peek();

        // Apostrophe: attribute/qualification tick or character literal.
        if b == b'\'' {
            return self.lex_tick_or_char(start);
        }

        // Identifiers and keywords
        if is_ident_start(b) {
            return self.lex_identifier_or_keyword(start);
        }

        // Extended identifier: \...\
        if b == b'\\' {
            return self.lex_extended_identifier(start);
        }

        // Numeric literals (including sized bit-string literals like 8UX"FF")
        if b.is_ascii_digit() {
            return self.lex_number(start);
        }

        // String literal
        if b == b'"' {
            return self.lex_string(start);
        }

        // Non-ASCII character outside a literal or comment
        if b >= 0x80 {
            return self.lex_non_ascii(start);
        }

        // Operators and punctuation
        self.lex_operator(start)
    }

    /// Lexes an apostrophe at `start`.
    ///
    /// After a token that can end a name or prefix (identifier, `)`, `all`, a
    /// string literal used as an operator symbol, or the `subtype` attribute
    /// designator), the apostrophe is a
    /// [`VhdlToken::Tick`]. Otherwise, if it is followed by exactly one graphic
    /// character and another apostrophe, it is a character literal.
    fn lex_tick_or_char(&mut self, start: usize) -> Token {
        let after_prefix = matches!(
            self.last_kind,
            Some(
                VhdlToken::Identifier
                    | VhdlToken::ExtendedIdentifier
                    | VhdlToken::RightParen
                    | VhdlToken::All
                    | VhdlToken::Subtype
                    | VhdlToken::StringLiteral
            )
        );
        if !after_prefix && start + 1 < self.source.len() {
            let (c, len) = self.char_at(start + 1);
            let close = start + 1 + len;
            if !c.is_control() && close < self.source.len() && self.source[close] == b'\'' {
                self.pos = close + 1;
                return Token {
                    kind: VhdlToken::CharLiteral,
                    span: self.span_from(start),
                };
            }
        }
        self.pos += 1;
        Token {
            kind: VhdlToken::Tick,
            span: self.span_from(start),
        }
    }

    fn lex_identifier_or_keyword(&mut self, start: usize) -> Token {
        while self.pos < self.source.len() && is_ident_char(self.source[self.pos]) {
            self.pos += 1;
        }

        let text = &self.source[start..self.pos];

        // Check for bit string literal prefix: B"...", O"...", X"...", UX"...", D"...", etc.
        if self.pos < self.source.len() && self.source[self.pos] == b'"' && text.len() <= 2 {
            let lower: Vec<u8> = text.iter().map(|b| b.to_ascii_lowercase()).collect();
            let is_bit_prefix = matches!(
                lower.as_slice(),
                b"b" | b"o" | b"x" | b"ub" | b"uo" | b"ux" | b"sb" | b"so" | b"sx" | b"d"
            );
            if is_bit_prefix {
                return self.lex_bit_string(start);
            }
        }

        // Lowercase for keyword lookup
        let mut lower_buf = [0u8; 64];
        let len = text.len().min(64);
        for (i, &ch) in text[..len].iter().enumerate() {
            lower_buf[i] = ch.to_ascii_lowercase();
        }
        let lower = std::str::from_utf8(&lower_buf[..len]).unwrap_or("");

        let kind = if let Some(kw) = lookup_keyword(lower) {
            kw
        } else {
            VhdlToken::Identifier
        };

        Token {
            kind,
            span: self.span_from(start),
        }
    }

    /// Lexes a bit-string literal whose length/base-specifier prefix starts at
    /// `start`; the current position is at the opening `"`.
    ///
    /// An unterminated literal (newline or end of file before the closing `"`)
    /// produces an error token. A terminated literal is validated with
    /// [`expand_bit_string`]; problems are reported with precise spans but the
    /// token stays a [`VhdlToken::BitStringLiteral`] so parsing continues.
    fn lex_bit_string(&mut self, start: usize) -> Token {
        self.pos += 1; // skip opening "
        loop {
            if self.pos >= self.source.len() || self.source[self.pos] == b'\n' {
                self.error("unterminated bit string literal", self.span_from(start));
                return Token {
                    kind: VhdlToken::Error,
                    span: self.span_from(start),
                };
            }
            if self.source[self.pos] == b'"' {
                self.pos += 1;
                break;
            }
            self.pos += 1;
        }
        if let Ok(text) = std::str::from_utf8(&self.source[start..self.pos]) {
            if let Err(e) = expand_bit_string(text) {
                self.error(&e.message, self.span(start + e.start, start + e.end));
            }
        }
        Token {
            kind: VhdlToken::BitStringLiteral,
            span: self.span_from(start),
        }
    }

    fn lex_extended_identifier(&mut self, start: usize) -> Token {
        self.pos += 1; // skip opening backslash
        while self.pos < self.source.len() {
            if self.source[self.pos] == b'\\' {
                // Check for escaped backslash: \\
                if self.peek_at(1) == b'\\' {
                    self.pos += 2;
                    continue;
                }
                self.pos += 1; // skip closing backslash
                return Token {
                    kind: VhdlToken::ExtendedIdentifier,
                    span: self.span_from(start),
                };
            }
            if self.source[self.pos] == b'\n' {
                break;
            }
            self.pos += 1;
        }
        self.error("unterminated extended identifier", self.span_from(start));
        Token {
            kind: VhdlToken::Error,
            span: self.span_from(start),
        }
    }

    /// Lexes a literal starting with a digit: a decimal integer or real
    /// literal, a based literal (`16#FF#`), or a sized bit-string literal
    /// (`8UX"FF"`).
    fn lex_number(&mut self, start: usize) -> Token {
        // Consume digits (and underscores)
        self.eat_digits();
        let int_end = self.pos;

        // Based literal: base#digits# (or base:digits: per LRM §15.10)
        let delim = self.peek();
        if delim == b'#' || (delim == b':' && self.colon_based_literal_ahead()) {
            return self.lex_based(start, int_end, delim);
        }

        // Sized bit-string literal: length base_specifier "..."
        if let Some(spec_len) = self.bit_string_spec_len() {
            self.pos += spec_len;
            return self.lex_bit_string(start);
        }

        self.check_underlines(start, int_end);

        // Check for real literal: digits.digits
        if self.pos < self.source.len()
            && self.source[self.pos] == b'.'
            && self.peek_at(1).is_ascii_digit()
        {
            self.pos += 1; // skip .
            let frac_start = self.pos;
            self.eat_digits();
            self.check_underlines(frac_start, self.pos);
            self.lex_exponent(true);
            return Token {
                kind: VhdlToken::RealLiteral,
                span: self.span_from(start),
            };
        }

        // Optional exponent for integer
        self.lex_exponent(false);

        Token {
            kind: VhdlToken::IntLiteral,
            span: self.span_from(start),
        }
    }

    /// If the current position starts a bit-string base specifier immediately
    /// followed by `"` (`B"`, `UX"`, `D"`, ...), returns the specifier length.
    fn bit_string_spec_len(&self) -> Option<usize> {
        let a = self.peek().to_ascii_uppercase();
        let b = self.peek_at(1).to_ascii_uppercase();
        if matches!(a, b'B' | b'O' | b'X' | b'D') && b == b'"' {
            return Some(1);
        }
        if matches!(a, b'U' | b'S') && matches!(b, b'B' | b'O' | b'X') && self.peek_at(2) == b'"' {
            return Some(2);
        }
        None
    }

    /// Returns true if the `:` at the current position opens a based literal
    /// written with the replacement delimiter (`16:FF:`): it must be followed by
    /// at least one extended digit and then a closing `:`.
    fn colon_based_literal_ahead(&self) -> bool {
        let mut i = self.pos + 1;
        if i >= self.source.len() || !self.source[i].is_ascii_alphanumeric() {
            return false;
        }
        while i < self.source.len()
            && (self.source[i].is_ascii_alphanumeric()
                || self.source[i] == b'_'
                || self.source[i] == b'.')
        {
            i += 1;
        }
        i < self.source.len() && self.source[i] == b':'
    }

    /// Lexes and validates a based literal. `start..int_end` holds the base
    /// and the current position is at the opening delimiter `delim` (`#` or
    /// its replacement `:`).
    fn lex_based(&mut self, start: usize, int_end: usize, delim: u8) -> Token {
        let delim_char = delim as char;
        let base_text: String = self.source[start..int_end]
            .iter()
            .filter(|&&b| b != b'_')
            .map(|&b| b as char)
            .collect();
        self.check_underlines(start, int_end);
        let base = match base_text.parse::<u32>() {
            Ok(b) if (2..=16).contains(&b) => Some(b),
            _ => {
                self.error_with_help(
                    &format!("base {base_text} of based literal is not in the range 2 to 16"),
                    self.span(start, int_end),
                    "use a base between 2 and 16, e.g. 2#1010#, 8#17#, or 16#FF#",
                );
                None
            }
        };

        let open = self.pos;
        self.pos += 1; // skip opening delimiter
        let digits_start = self.pos;
        let mut dot: Option<usize> = None;
        let mut extra_dot: Option<usize> = None;
        let mut bad_digit: Option<usize> = None;
        while self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch.is_ascii_alphanumeric() {
                let value = (ch as char).to_digit(36).unwrap_or(u32::MAX);
                if let Some(b) = base {
                    if value >= b && bad_digit.is_none() {
                        bad_digit = Some(self.pos);
                    }
                }
                self.pos += 1;
            } else if ch == b'_' {
                self.pos += 1;
            } else if ch == b'.' {
                if dot.is_none() {
                    dot = Some(self.pos);
                } else if extra_dot.is_none() {
                    extra_dot = Some(self.pos);
                }
                self.pos += 1;
            } else {
                break;
            }
        }
        let digits_end = self.pos;

        // Expect closing delimiter
        if self.peek() == delim {
            self.pos += 1;
        } else {
            self.error(
                &format!("expected closing '{delim_char}' in based literal"),
                self.span_from(start),
            );
            return Token {
                kind: VhdlToken::Error,
                span: self.span_from(start),
            };
        }

        // Validate the digits.
        let int_part_end = dot.unwrap_or(digits_end);
        let has_digit = |s: usize, e: usize| self.source[s..e].iter().any(|b| *b != b'_');
        if !has_digit(digits_start, int_part_end) {
            let end = if dot.is_some() {
                int_part_end + 1
            } else {
                self.pos
            };
            self.error(
                "based literal requires at least one digit before the closing delimiter or '.'",
                self.span(open, end),
            );
        } else {
            self.check_underlines(digits_start, int_part_end);
        }
        if let Some(d) = dot {
            let frac_end = extra_dot.unwrap_or(digits_end);
            if !has_digit(d + 1, frac_end) {
                self.error(
                    "based real literal requires at least one digit after '.'",
                    self.span(d, d + 1),
                );
            } else {
                self.check_underlines(d + 1, frac_end);
            }
        }
        if let Some(e) = extra_dot {
            self.error(
                "based literal can contain at most one '.'",
                self.span(e, e + 1),
            );
        }
        if let (Some(p), Some(b)) = (bad_digit, base) {
            let c = self.source[p] as char;
            self.error_with_help(
                &format!("digit '{c}' is not valid in base {b}"),
                self.span(p, p + 1),
                &format!("every digit of a base-{b} literal must be less than {b}"),
            );
        }

        // Optional exponent
        let is_real = dot.is_some();
        self.lex_exponent(is_real);
        Token {
            kind: if is_real {
                VhdlToken::RealLiteral
            } else {
                VhdlToken::IntLiteral
            },
            span: self.span_from(start),
        }
    }

    fn eat_digits(&mut self) {
        while self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch.is_ascii_digit() || ch == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Reports an underline in `start..end` that does not sit between two
    /// digits (leading, trailing, or doubled), per LRM §15.5.
    fn check_underlines(&self, start: usize, end: usize) {
        for i in start..end {
            if self.source[i] != b'_' {
                continue;
            }
            if i == start || i + 1 == end || self.source[i + 1] == b'_' {
                self.error(
                    "underline in a numeric literal must appear between two digits",
                    self.span(i, i + 1),
                );
                return;
            }
        }
    }

    /// Lexes an optional exponent (`E[+|-]digits`). Reports an exponent with
    /// no digits, and a negative exponent on an integer literal (LRM §15.5.2).
    fn lex_exponent(&mut self, is_real: bool) {
        if !matches!(self.peek(), b'e' | b'E') {
            return;
        }
        let e_pos = self.pos;
        self.pos += 1;
        let sign = self.peek();
        if sign == b'+' || sign == b'-' {
            self.pos += 1;
        }
        if !self.peek().is_ascii_digit() {
            self.error("expected digits in exponent", self.span_from(e_pos));
            return;
        }
        let digits_start = self.pos;
        self.eat_digits();
        self.check_underlines(digits_start, self.pos);
        if !is_real && sign == b'-' {
            self.error_with_help(
                "an integer literal cannot have a negative exponent",
                self.span_from(e_pos),
                "write it as a real literal instead, e.g. 1.0E-3",
            );
        }
    }

    fn lex_string(&mut self, start: usize) -> Token {
        self.pos += 1; // skip opening "
        loop {
            if self.pos >= self.source.len() || self.source[self.pos] == b'\n' {
                self.error("unterminated string literal", self.span_from(start));
                return Token {
                    kind: VhdlToken::Error,
                    span: self.span_from(start),
                };
            }
            if self.source[self.pos] == b'"' {
                // Check for escaped quote: ""
                if self.peek_at(1) == b'"' {
                    self.pos += 2;
                    continue;
                }
                self.pos += 1; // skip closing "
                return Token {
                    kind: VhdlToken::StringLiteral,
                    span: self.span_from(start),
                };
            }
            self.pos += 1;
        }
    }

    /// Reports a non-ASCII character outside literals and comments as a single
    /// error token whose span covers the whole UTF-8 sequence.
    fn lex_non_ascii(&mut self, start: usize) -> Token {
        let (c, len) = self.char_at(start);
        self.pos += len;
        let msg = format!("unrecognized character {}", describe_char(c));
        let span = self.span_from(start);
        match typographic_replacement(c) {
            Some((ascii, what)) => self.error_with_help(
                &msg,
                span,
                &format!(
                    "this looks like a typographic {what}; use the ASCII character `{ascii}` \
                     instead"
                ),
            ),
            None => self.error(&msg, span),
        }
        Token {
            kind: VhdlToken::Error,
            span,
        }
    }

    fn lex_operator(&mut self, start: usize) -> Token {
        let b = self.advance();
        let kind = match b {
            b'(' => VhdlToken::LeftParen,
            b')' => VhdlToken::RightParen,
            b',' => VhdlToken::Comma,
            b';' => VhdlToken::Semicolon,
            b':' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    VhdlToken::ColonEquals
                } else {
                    VhdlToken::Colon
                }
            }
            b'<' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    VhdlToken::LessEquals
                } else if self.peek() == b'<' {
                    self.pos += 1;
                    VhdlToken::DoubleLess
                } else if self.peek() == b'>' {
                    // `<>` is a single compound delimiter (box); no VHDL
                    // expression has `<` directly followed by `>`.
                    self.pos += 1;
                    VhdlToken::Box
                } else {
                    VhdlToken::LessThan
                }
            }
            b'>' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    VhdlToken::GreaterEquals
                } else if self.peek() == b'>' {
                    self.pos += 1;
                    VhdlToken::DoubleGreater
                } else {
                    VhdlToken::GreaterThan
                }
            }
            b'=' => {
                if self.peek() == b'>' {
                    self.pos += 1;
                    VhdlToken::Arrow
                } else {
                    VhdlToken::Equals
                }
            }
            b'/' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    VhdlToken::SlashEquals
                } else {
                    VhdlToken::Slash
                }
            }
            b'*' => {
                if self.peek() == b'*' {
                    self.pos += 1;
                    VhdlToken::DoubleStar
                } else {
                    VhdlToken::Star
                }
            }
            b'+' => VhdlToken::Plus,
            b'-' => VhdlToken::Minus,
            b'&' => VhdlToken::Ampersand,
            // `!` is the LRM §15.10 replacement character for `|`.
            b'|' | b'!' => VhdlToken::Bar,
            b'\'' => VhdlToken::Tick,
            b'.' => VhdlToken::Dot,
            b'^' => VhdlToken::Caret,
            b'@' => VhdlToken::At,
            b'?' => match self.peek() {
                b'=' => {
                    self.pos += 1;
                    VhdlToken::MatchEquals
                }
                b'/' => {
                    if self.peek_at(1) == b'=' {
                        self.pos += 2;
                        VhdlToken::MatchSlashEquals
                    } else {
                        self.error("unexpected character after '?/'", self.span_from(start));
                        VhdlToken::Error
                    }
                }
                b'<' => {
                    self.pos += 1;
                    if self.peek() == b'=' {
                        self.pos += 1;
                        VhdlToken::MatchLessEquals
                    } else {
                        VhdlToken::MatchLess
                    }
                }
                b'>' => {
                    self.pos += 1;
                    if self.peek() == b'=' {
                        self.pos += 1;
                        VhdlToken::MatchGreaterEquals
                    } else {
                        VhdlToken::MatchGreater
                    }
                }
                b'?' => {
                    self.pos += 1;
                    VhdlToken::ConditionOp
                }
                // A lone `?` is the VHDL-2008 matching marker in
                // `select?` and `case?`; the parser decides where it is legal.
                _ => VhdlToken::Question,
            },
            _ => {
                // Only ASCII reaches here; non-ASCII is handled by lex_non_ascii.
                self.error(
                    &format!("unrecognized character {}", describe_char(b as char)),
                    self.span_from(start),
                );
                VhdlToken::Error
            }
        };
        Token {
            kind,
            span: self.span_from(start),
        }
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Formats a character for an "unrecognized character" diagnostic: printable
/// ASCII as `'~'`, control characters as `U+0007`, and everything else with
/// both the character and its code point, e.g. `'‘' (U+2018)`.
fn describe_char(c: char) -> String {
    if c.is_ascii_graphic() {
        format!("'{c}'")
    } else if c.is_control() {
        format!("U+{:04X}", c as u32)
    } else {
        format!("'{c}' (U+{:04X})", c as u32)
    }
}

/// Maps typographic look-alikes (smart quotes, primes, dashes) to the ASCII
/// character the author most likely meant, with a short description.
fn typographic_replacement(c: char) -> Option<(char, &'static str)> {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => {
            Some(('\'', "single quote (smart quote)"))
        }
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => {
            Some(('"', "double quote (smart quote)"))
        }
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => {
            Some(('-', "dash"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_tokens(source: &str) -> Vec<Token> {
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lex(source, file, &sink);
        assert!(
            !sink.has_errors(),
            "unexpected errors: {:?}",
            sink.diagnostics()
        );
        tokens
    }

    fn lex_tokens_with_errors(source: &str) -> (Vec<Token>, Vec<Diagnostic>) {
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lex(source, file, &sink);
        (tokens, sink.take_all())
    }

    fn kinds(tokens: &[Token]) -> Vec<VhdlToken> {
        tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn empty_input() {
        let tokens = lex_tokens("");
        assert_eq!(kinds(&tokens), vec![VhdlToken::Eof]);
    }

    #[test]
    fn whitespace_only() {
        let tokens = lex_tokens("  \t\n  ");
        assert_eq!(kinds(&tokens), vec![VhdlToken::Eof]);
    }

    #[test]
    fn keywords_case_insensitive() {
        let tokens = lex_tokens("ENTITY entity Entity eNtItY");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Entity,
                VhdlToken::Entity,
                VhdlToken::Entity,
                VhdlToken::Entity,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn all_keyword_categories() {
        let tokens = lex_tokens(
            "abs and architecture array begin block case component constant else end entity for function generic if in is library loop not null of or out package port process range record return signal subtype then to type use variable wait when while with",
        );
        let k = kinds(&tokens);
        assert_eq!(k[0], VhdlToken::Abs);
        assert_eq!(k[1], VhdlToken::And);
        assert_eq!(k[2], VhdlToken::Architecture);
        assert_eq!(k[3], VhdlToken::Array);
        assert_eq!(k[4], VhdlToken::Begin);
        assert!(k.contains(&VhdlToken::With));
        assert_eq!(*k.last().unwrap(), VhdlToken::Eof);
    }

    #[test]
    fn identifiers() {
        let tokens = lex_tokens("my_signal CLK data_in_0");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Identifier,
                VhdlToken::Identifier,
                VhdlToken::Identifier,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn extended_identifier() {
        let tokens = lex_tokens("\\my signal\\");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::ExtendedIdentifier, VhdlToken::Eof]
        );
    }

    #[test]
    fn extended_identifier_escaped_backslash() {
        let tokens = lex_tokens("\\my\\\\sig\\");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::ExtendedIdentifier, VhdlToken::Eof]
        );
    }

    #[test]
    fn integer_literals() {
        let tokens = lex_tokens("0 42 1_000_000");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::IntLiteral,
                VhdlToken::IntLiteral,
                VhdlToken::IntLiteral,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn real_literals() {
        let tokens = lex_tokens("1.5 0.0 1.0e3 2.5E-2");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::RealLiteral,
                VhdlToken::RealLiteral,
                VhdlToken::RealLiteral,
                VhdlToken::RealLiteral,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn based_integer_literal() {
        let tokens = lex_tokens("16#FF# 2#1010_0110# 8#77#");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::IntLiteral,
                VhdlToken::IntLiteral,
                VhdlToken::IntLiteral,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn based_real_literal() {
        let tokens = lex_tokens("16#F.F#");
        assert_eq!(kinds(&tokens), vec![VhdlToken::RealLiteral, VhdlToken::Eof]);
    }

    #[test]
    fn character_literal() {
        let tokens = lex_tokens("'0' '1' 'Z'");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::CharLiteral,
                VhdlToken::CharLiteral,
                VhdlToken::CharLiteral,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn string_literal() {
        let tokens = lex_tokens("\"hello\" \"world\"");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::StringLiteral,
                VhdlToken::StringLiteral,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn string_literal_escaped_quote() {
        let tokens = lex_tokens("\"say \"\"hi\"\"\"");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::StringLiteral, VhdlToken::Eof]
        );
    }

    #[test]
    fn bit_string_literals() {
        let tokens = lex_tokens("X\"FF\" B\"1010\" O\"77\"");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::BitStringLiteral,
                VhdlToken::BitStringLiteral,
                VhdlToken::BitStringLiteral,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn operators_and_punctuation() {
        let tokens = lex_tokens("( ) , ; : := <= => & * ** + - / = /= < > >= | .");
        let k = kinds(&tokens);
        assert_eq!(k[0], VhdlToken::LeftParen);
        assert_eq!(k[1], VhdlToken::RightParen);
        assert_eq!(k[2], VhdlToken::Comma);
        assert_eq!(k[3], VhdlToken::Semicolon);
        assert_eq!(k[4], VhdlToken::Colon);
        assert_eq!(k[5], VhdlToken::ColonEquals);
        assert_eq!(k[6], VhdlToken::LessEquals);
        assert_eq!(k[7], VhdlToken::Arrow);
        assert_eq!(k[8], VhdlToken::Ampersand);
        assert_eq!(k[9], VhdlToken::Star);
        assert_eq!(k[10], VhdlToken::DoubleStar);
        assert_eq!(k[11], VhdlToken::Plus);
        assert_eq!(k[12], VhdlToken::Minus);
        assert_eq!(k[13], VhdlToken::Slash);
        assert_eq!(k[14], VhdlToken::Equals);
        assert_eq!(k[15], VhdlToken::SlashEquals);
        assert_eq!(k[16], VhdlToken::LessThan);
        assert_eq!(k[17], VhdlToken::GreaterThan);
        assert_eq!(k[18], VhdlToken::GreaterEquals);
        assert_eq!(k[19], VhdlToken::Bar);
        assert_eq!(k[20], VhdlToken::Dot);
        assert_eq!(k[21], VhdlToken::Eof);
    }

    #[test]
    fn box_is_one_token() {
        let tokens = lex_tokens("natural range <>");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Identifier,
                VhdlToken::Range,
                VhdlToken::Box,
                VhdlToken::Eof,
            ]
        );
        assert_eq!(tokens[2].span.start, 14);
        assert_eq!(tokens[2].span.end, 16);
    }

    #[test]
    fn separated_less_greater_are_not_a_box() {
        // Only an adjacent `<>` is the box; `< >` stays two operators and
        // `<=`/`<<` keep their meanings.
        let k = kinds(&lex_tokens("< > <= << <>>"));
        assert_eq!(
            k,
            vec![
                VhdlToken::LessThan,
                VhdlToken::GreaterThan,
                VhdlToken::LessEquals,
                VhdlToken::DoubleLess,
                VhdlToken::Box,
                VhdlToken::GreaterThan,
                VhdlToken::Eof,
            ]
        );
    }

    #[test]
    fn matching_operators() {
        let tokens = lex_tokens("?= ?/= ?< ?<= ?> ?>= ??");
        let k = kinds(&tokens);
        assert_eq!(k[0], VhdlToken::MatchEquals);
        assert_eq!(k[1], VhdlToken::MatchSlashEquals);
        assert_eq!(k[2], VhdlToken::MatchLess);
        assert_eq!(k[3], VhdlToken::MatchLessEquals);
        assert_eq!(k[4], VhdlToken::MatchGreater);
        assert_eq!(k[5], VhdlToken::MatchGreaterEquals);
        assert_eq!(k[6], VhdlToken::ConditionOp);
    }

    #[test]
    fn lone_question_mark() {
        // `select?` (no space) and `select ?` both lex as Select, Question.
        let tokens = lex_tokens("select? select ? ??");
        let k = kinds(&tokens);
        assert_eq!(k[0], VhdlToken::Select);
        assert_eq!(k[1], VhdlToken::Question);
        assert_eq!(k[2], VhdlToken::Select);
        assert_eq!(k[3], VhdlToken::Question);
        assert_eq!(k[4], VhdlToken::ConditionOp);
        assert_eq!(k[5], VhdlToken::Eof);
    }

    #[test]
    fn question_slash_without_equals_is_error() {
        let sink = DiagnosticSink::new();
        let tokens = lex("?/x", FileId::from_raw(0), &sink);
        assert_eq!(tokens[0].kind, VhdlToken::Error);
        assert!(sink.has_errors());
    }

    #[test]
    fn double_angle_brackets() {
        let tokens = lex_tokens("<< >>");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::DoubleLess,
                VhdlToken::DoubleGreater,
                VhdlToken::Eof
            ]
        );
    }

    #[test]
    fn line_comment() {
        let tokens = lex_tokens("signal -- this is a comment\nclk");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::Signal, VhdlToken::Identifier, VhdlToken::Eof]
        );
    }

    #[test]
    fn block_comment() {
        let tokens = lex_tokens("signal /* block\ncomment */ clk");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::Signal, VhdlToken::Identifier, VhdlToken::Eof]
        );
    }

    #[test]
    fn nested_block_comment() {
        // LRM §15.9: a delimited comment ends at the first `*/`; `/*` inside a
        // comment has no special meaning, so the text after the first `*/` is
        // ordinary source.
        let tokens = lex_tokens("signal /* outer /* inner */ clk");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::Signal, VhdlToken::Identifier, VhdlToken::Eof]
        );
        let tokens = lex_tokens("signal /* outer /* inner */ still */ clk");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Signal,
                VhdlToken::Identifier, // still
                VhdlToken::Star,
                VhdlToken::Slash,
                VhdlToken::Identifier, // clk
                VhdlToken::Eof
            ]
        );
    }

    #[test]
    fn block_comment_with_glob_is_not_nested() {
        let tokens = lex_tokens("/* see src/*.vhd */ entity e is end;");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Entity,
                VhdlToken::Identifier,
                VhdlToken::Is,
                VhdlToken::End,
                VhdlToken::Semicolon,
                VhdlToken::Eof
            ]
        );
    }

    #[test]
    fn block_comment_edge_cases() {
        // `/*/` does not close the comment it opens.
        let tokens = lex_tokens("/*/ x */ y");
        assert_eq!(kinds(&tokens), vec![VhdlToken::Identifier, VhdlToken::Eof]);
        // `/**/` is an empty comment.
        let tokens = lex_tokens("a/**/b");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::Identifier, VhdlToken::Identifier, VhdlToken::Eof]
        );
    }

    #[test]
    fn unterminated_block_comment_error() {
        let (tokens, errors) = lex_tokens_with_errors("signal /* never closed *");
        assert_eq!(kinds(&tokens), vec![VhdlToken::Signal, VhdlToken::Eof]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("unterminated block comment"));
        assert_eq!(errors[0].primary_span.start, 7);
        assert_eq!(errors[0].primary_span.end, 24);
    }

    // === UTF-8 outside literals (VHDL-5) ===

    #[test]
    fn smart_quotes_one_error_per_char() {
        let src = "x <= \u{2018}1\u{2019};";
        let (tokens, errors) = lex_tokens_with_errors(src);
        // x, <=, Error(‘), 1, Error(’), ;, Eof
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Identifier,
                VhdlToken::LessEquals,
                VhdlToken::Error,
                VhdlToken::IntLiteral,
                VhdlToken::Error,
                VhdlToken::Semicolon,
                VhdlToken::Eof
            ]
        );
        assert_eq!(errors.len(), 2);
        // The first error token covers the whole 3-byte character.
        assert_eq!((tokens[2].span.start, tokens[2].span.end), (5, 8));
        assert_eq!((tokens[4].span.start, tokens[4].span.end), (9, 12));
        assert!(
            errors[0].message.contains("U+2018"),
            "{}",
            errors[0].message
        );
        assert!(errors[0].message.contains('\u{2018}'));
        assert!(errors[0].help.iter().any(|h| h.contains("`'`")));
        assert!(errors[1].message.contains("U+2019"));
        // Every span lies on a char boundary.
        for t in &tokens {
            assert!(src.is_char_boundary(t.span.start as usize));
            assert!(src.is_char_boundary(t.span.end as usize));
        }
    }

    #[test]
    fn smart_double_quotes_hint() {
        let (_, errors) = lex_tokens_with_errors("\u{201C}abc\u{201D}");
        assert_eq!(errors.len(), 2);
        assert!(errors[0].help.iter().any(|h| h.contains("`\"`")));
        assert!(errors[1].message.contains("U+201D"));
    }

    #[test]
    fn other_non_ascii_char_error_without_hint() {
        let (tokens, errors) = lex_tokens_with_errors("a \u{1F600} b");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Identifier,
                VhdlToken::Error,
                VhdlToken::Identifier,
                VhdlToken::Eof
            ]
        );
        assert_eq!((tokens[1].span.start, tokens[1].span.end), (2, 6));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("U+1F600"));
        assert!(errors[0].help.is_empty());
    }

    #[test]
    fn non_ascii_in_strings_and_comments_is_fine() {
        let tokens = lex_tokens("-- caf\u{e9} \u{2018}x\u{2019}\n\"\u{e9}t\u{e9}\" /* \u{2014} */");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::StringLiteral, VhdlToken::Eof]
        );
    }

    #[test]
    fn non_breaking_space_is_a_separator() {
        let tokens = lex_tokens("signal\u{a0}clk");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::Signal, VhdlToken::Identifier, VhdlToken::Eof]
        );
    }

    #[test]
    fn non_ascii_character_literal() {
        let tokens = lex_tokens("c := '\u{e9}';");
        assert_eq!(tokens[2].kind, VhdlToken::CharLiteral);
        assert_eq!((tokens[2].span.start, tokens[2].span.end), (5, 9));
    }

    #[test]
    fn control_char_reported_by_code_point() {
        let (_, errors) = lex_tokens_with_errors("a \u{7} b");
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].message.contains("U+0007"),
            "{}",
            errors[0].message
        );
    }

    // === Bit-string literals (VHDL-15) ===

    #[test]
    fn sized_bit_string_literals_are_one_token() {
        let src = "8UX\"FF\" 12SB\"101\" 16D\"255\" 8x\"0f\" 4SO\"3\"";
        let tokens = lex_tokens(src);
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::BitStringLiteral,
                VhdlToken::BitStringLiteral,
                VhdlToken::BitStringLiteral,
                VhdlToken::BitStringLiteral,
                VhdlToken::BitStringLiteral,
                VhdlToken::Eof
            ]
        );
        assert_eq!((tokens[0].span.start, tokens[0].span.end), (0, 7));
        assert_eq!((tokens[2].span.start, tokens[2].span.end), (18, 26));
    }

    #[test]
    fn unsized_extended_bit_string_literals() {
        let tokens = lex_tokens("UX\"F\" SX\"F\" UB\"1\" UO\"7\" SB\"1\" SO\"7\" D\"9\"");
        assert!(tokens[..7]
            .iter()
            .all(|t| t.kind == VhdlToken::BitStringLiteral));
        assert_eq!(tokens[7].kind, VhdlToken::Eof);
    }

    #[test]
    fn bit_string_metavalues_accepted() {
        let tokens = lex_tokens("X\"Z\" B\"1-0X\" 8UX\"-\"");
        assert_eq!(tokens.len(), 4);
    }

    #[test]
    fn number_followed_by_identifier_is_not_bit_string() {
        // No quote after the base letter, so this is not a bit string.
        let tokens = lex_tokens("8 X");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::IntLiteral, VhdlToken::Identifier, VhdlToken::Eof]
        );
    }

    #[test]
    fn unterminated_bit_string_errors() {
        for src in ["X\"FF", "X\"FF\n", "8UX\"F", "8UX\"F\n"] {
            let (tokens, errors) = lex_tokens_with_errors(src);
            assert_eq!(tokens[0].kind, VhdlToken::Error, "{src:?}");
            assert_eq!(errors.len(), 1, "{src:?}");
            assert!(errors[0].message.contains("unterminated bit string"));
        }
    }

    #[test]
    fn bit_string_invalid_digit_errors() {
        let (tokens, errors) = lex_tokens_with_errors("x <= B\"0120\";");
        assert_eq!(tokens[2].kind, VhdlToken::BitStringLiteral);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("digit '2'"));
        // Span points exactly at the offending digit.
        assert_eq!(
            (errors[0].primary_span.start, errors[0].primary_span.end),
            (9, 10)
        );
        let (_, errors) = lex_tokens_with_errors("O\"78\"");
        assert_eq!(errors.len(), 1);
        let (_, errors) = lex_tokens_with_errors("D\"1F\"");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("decimal digit"));
    }

    #[test]
    fn bit_string_truncation_errors() {
        let (_, errors) = lex_tokens_with_errors("3UX\"F\"");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("cannot be truncated"));
        let (_, errors) = lex_tokens_with_errors("8D\"256\"");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("needs 9 bits"));
        // Allowed truncations stay silent.
        lex_tokens("3UX\"7\" 3SX\"F\"");
    }

    // === Tick vs. character literal (VHDL-16) ===

    #[test]
    fn qualified_expression_with_space_char() {
        let tokens = lex_tokens("x := character'(' ');");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Identifier,
                VhdlToken::ColonEquals,
                VhdlToken::Identifier,
                VhdlToken::Tick,
                VhdlToken::LeftParen,
                VhdlToken::CharLiteral,
                VhdlToken::RightParen,
                VhdlToken::Semicolon,
                VhdlToken::Eof
            ]
        );
    }

    #[test]
    fn qualified_expression_with_char() {
        let tokens = lex_tokens("t'('a')");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::Identifier,
                VhdlToken::Tick,
                VhdlToken::LeftParen,
                VhdlToken::CharLiteral,
                VhdlToken::RightParen,
                VhdlToken::Eof
            ]
        );
    }

    #[test]
    fn tick_after_names_and_parens() {
        let k = kinds(&lex_tokens("x'length f(x)'high p.all'length \\e\\'left"));
        assert_eq!(
            k,
            vec![
                VhdlToken::Identifier,
                VhdlToken::Tick,
                VhdlToken::Identifier,
                VhdlToken::Identifier,
                VhdlToken::LeftParen,
                VhdlToken::Identifier,
                VhdlToken::RightParen,
                VhdlToken::Tick,
                VhdlToken::Identifier,
                VhdlToken::Identifier,
                VhdlToken::Dot,
                VhdlToken::All,
                VhdlToken::Tick,
                VhdlToken::Identifier,
                VhdlToken::ExtendedIdentifier,
                VhdlToken::Tick,
                VhdlToken::Identifier,
                VhdlToken::Eof
            ]
        );
    }

    #[test]
    fn char_literals_after_operators_and_keywords() {
        let k = kinds(&lex_tokens(
            "a <= '1'; if b = '0' then when 'x' | ''' => c(')') range 'a' to 'z'",
        ));
        let chars = k.iter().filter(|t| **t == VhdlToken::CharLiteral).count();
        assert_eq!(chars, 7, "{k:?}");
        assert!(!k.contains(&VhdlToken::Tick));
    }

    #[test]
    fn attribute_with_char_literal_argument() {
        // x'('a') vs x'image('a')
        let k = kinds(&lex_tokens("character'image('a')"));
        assert_eq!(
            k,
            vec![
                VhdlToken::Identifier,
                VhdlToken::Tick,
                VhdlToken::Identifier,
                VhdlToken::LeftParen,
                VhdlToken::CharLiteral,
                VhdlToken::RightParen,
                VhdlToken::Eof
            ]
        );
    }

    #[test]
    fn lone_tick_at_start_is_tick() {
        let (tokens, errors) = lex_tokens_with_errors("'");
        assert_eq!(kinds(&tokens), vec![VhdlToken::Tick, VhdlToken::Eof]);
        assert!(errors.is_empty());
    }

    // === Based literal validation (VHDL-21) ===

    #[test]
    fn based_literal_valid_forms() {
        let tokens = lex_tokens("2#1010# 8#777# 16#dead_BEEF# 16#F.F#E+2 2#1#E3 36");
        assert_eq!(
            kinds(&tokens),
            vec![
                VhdlToken::IntLiteral,
                VhdlToken::IntLiteral,
                VhdlToken::IntLiteral,
                VhdlToken::RealLiteral,
                VhdlToken::IntLiteral,
                VhdlToken::IntLiteral,
                VhdlToken::Eof
            ]
        );
    }

    #[test]
    fn based_literal_digit_exceeds_base() {
        let (tokens, errors) = lex_tokens_with_errors("2#1234#");
        assert_eq!(tokens[0].kind, VhdlToken::IntLiteral);
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("digit '2' is not valid in base 2"));
        assert_eq!(
            (errors[0].primary_span.start, errors[0].primary_span.end),
            (3, 4)
        );
        let (_, errors) = lex_tokens_with_errors("16#FG#");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("'G'"));
        let (_, errors) = lex_tokens_with_errors("8#1.8#");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn based_literal_requires_digits() {
        let (_, errors) = lex_tokens_with_errors("16##");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("at least one digit"));
        assert_eq!(
            (errors[0].primary_span.start, errors[0].primary_span.end),
            (2, 4)
        );
        let (_, errors) = lex_tokens_with_errors("16#F.#");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("after '.'"));
        let (_, errors) = lex_tokens_with_errors("16#.F#");
        assert_eq!(errors.len(), 1);
        let (_, errors) = lex_tokens_with_errors("16#1.2.3#");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("at most one '.'"));
    }

    #[test]
    fn based_literal_base_out_of_range() {
        for (src, base) in [
            ("99#1#", "99"),
            ("1#0#", "1"),
            ("0#0#", "0"),
            ("17#1#", "17"),
        ] {
            let (_, errors) = lex_tokens_with_errors(src);
            assert_eq!(errors.len(), 1, "{src}");
            assert!(
                errors[0].message.contains(&format!("base {base} ")),
                "{}",
                errors[0].message
            );
            assert_eq!(errors[0].primary_span.start, 0);
            assert_eq!(errors[0].primary_span.end as usize, base.len());
        }
        let (_, errors) = lex_tokens_with_errors("99999999999999#1#");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn based_literal_unterminated() {
        let (tokens, errors) = lex_tokens_with_errors("16#FF;");
        assert_eq!(tokens[0].kind, VhdlToken::Error);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("closing '#'"));
    }

    #[test]
    fn based_literal_colon_replacement() {
        let tokens = lex_tokens("16:FF: 2:1010:");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::IntLiteral, VhdlToken::IntLiteral, VhdlToken::Eof]
        );
        // Ordinary colons after numbers are untouched.
        let tokens = lex_tokens("x(1):=2; l: for i in 0 to 3");
        assert!(kinds(&tokens).contains(&VhdlToken::ColonEquals));
        assert!(kinds(&tokens).contains(&VhdlToken::Colon));
        let (_, errors) = lex_tokens_with_errors("2:13:");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn numeric_underline_and_exponent_errors() {
        lex_tokens("1_000 1.0E-3 2#1_0#");
        let (_, errors) = lex_tokens_with_errors("1__0");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("underline"));
        let (_, errors) = lex_tokens_with_errors("10_");
        assert_eq!(errors.len(), 1);
        let (_, errors) = lex_tokens_with_errors("1E-3");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("negative exponent"));
        let (_, errors) = lex_tokens_with_errors("1E+");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("digits in exponent"));
    }

    #[test]
    fn exclamation_is_bar_replacement() {
        let tokens = lex_tokens("when 1 ! 2 =>");
        assert_eq!(tokens[2].kind, VhdlToken::Bar);
    }

    #[test]
    fn unterminated_string_error() {
        let (tokens, errors) = lex_tokens_with_errors("\"unterminated\n");
        assert!(tokens.iter().any(|t| t.kind == VhdlToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn unterminated_extended_identifier_error() {
        let (tokens, errors) = lex_tokens_with_errors("\\no_end\n");
        assert!(tokens.iter().any(|t| t.kind == VhdlToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn unrecognized_char_error() {
        let (tokens, errors) = lex_tokens_with_errors("~");
        assert!(tokens.iter().any(|t| t.kind == VhdlToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn spans_are_correct() {
        let tokens = lex_tokens("entity top");
        // "entity" is bytes 0..6, "top" is bytes 7..10
        assert_eq!(tokens[0].span.start, 0);
        assert_eq!(tokens[0].span.end, 6);
        assert_eq!(tokens[1].span.start, 7);
        assert_eq!(tokens[1].span.end, 10);
    }

    #[test]
    fn eof_always_present() {
        let tokens = lex_tokens("entity");
        assert_eq!(tokens.last().unwrap().kind, VhdlToken::Eof);
    }

    #[test]
    fn tick_as_attribute_access() {
        // When tick appears after an identifier (not as char literal)
        let tokens = lex_tokens("clk'event");
        let k = kinds(&tokens);
        assert_eq!(k[0], VhdlToken::Identifier); // clk
        assert_eq!(k[1], VhdlToken::Tick); // '
        assert_eq!(k[2], VhdlToken::Identifier); // event
    }

    #[test]
    fn integer_with_exponent() {
        let tokens = lex_tokens("1E3 2e+5");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::IntLiteral, VhdlToken::IntLiteral, VhdlToken::Eof,]
        );
    }
}
