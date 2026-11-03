//! Lexical analyzer for Verilog-2005 source text.
//!
//! Converts source text into a sequence of [`Token`]s, handling case-sensitive
//! keywords, sized/based literals (`4'b1010`), string literals with C-style escapes,
//! line and block comments, escaped identifiers, system identifiers, and compiler
//! directives. Errors are reported to the [`DiagnosticSink`] and produce
//! [`VerilogToken::Error`] tokens.

use crate::token::{lookup_keyword, Token, VerilogToken};
use aion_diagnostics::code::{Category, DiagnosticCode};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, Span};

/// Lexes the given Verilog source text into a vector of tokens.
///
/// Whitespace and comments are skipped. The returned vector always ends with
/// a [`VerilogToken::Eof`] token. Lexer errors are reported via the diagnostic
/// sink and produce [`VerilogToken::Error`] tokens in the output.
pub fn lex(source: &str, file: FileId, sink: &DiagnosticSink) -> Vec<Token> {
    let mut lexer = Lexer {
        source: source.as_bytes(),
        pos: 0,
        file,
        sink,
    };
    lexer.lex_all()
}

struct Lexer<'a> {
    source: &'a [u8],
    pos: usize,
    file: FileId,
    sink: &'a DiagnosticSink,
}

impl Lexer<'_> {
    fn lex_all(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            if self.pos >= self.source.len() {
                tokens.push(Token {
                    kind: VerilogToken::Eof,
                    span: Span::new(self.file, self.pos as u32, self.pos as u32),
                });
                break;
            }
            tokens.push(self.next_token());
        }
        debug_assert!(
            tokens.iter().all(|t| t.span.start <= t.span.end
                && self.is_char_boundary(t.span.start as usize)
                && self.is_char_boundary(t.span.end as usize)),
            "lexer produced a span past EOF or inside a UTF-8 character"
        );
        tokens
    }

    /// Returns `true` if `pos` is within the source (or exactly at its end)
    /// and does not fall inside a multi-byte UTF-8 character.
    fn is_char_boundary(&self, pos: usize) -> bool {
        pos == self.source.len() || (pos < self.source.len() && (self.source[pos] & 0xC0) != 0x80)
    }

    /// Returns the byte position just past the UTF-8 character starting at `pos`.
    fn char_end(&self, pos: usize) -> usize {
        let mut end = (pos + 1).min(self.source.len());
        while end < self.source.len() && (self.source[end] & 0xC0) == 0x80 {
            end += 1;
        }
        end
    }

    /// Returns the position of the first non-whitespace byte at or after `pos`.
    fn skip_ws_from(&self, mut pos: usize) -> usize {
        while pos < self.source.len() && self.source[pos].is_ascii_whitespace() {
            pos += 1;
        }
        pos
    }

    /// If a based-literal prefix (`'b`, `'sh`, ...) starts at `pos`, returns the
    /// lowercase base letter and the position just past it. The tick, optional
    /// `s` and base letter must be adjacent.
    fn based_prefix_at(&self, pos: usize) -> Option<(u8, usize)> {
        if self.source.get(pos) != Some(&b'\'') {
            return None;
        }
        let mut p = pos + 1;
        if matches!(self.source.get(p), Some(b's' | b'S')) {
            p += 1;
        }
        let base = self.source.get(p)?.to_ascii_lowercase();
        matches!(base, b'b' | b'o' | b'd' | b'h').then_some((base, p + 1))
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

    fn error(&self, msg: &str, span: Span) {
        self.sink.emit(Diagnostic::error(
            DiagnosticCode::new(Category::Error, 100),
            msg,
            span,
        ));
    }

    /// Reports the unrecognized character spanning `start..self.pos` with one
    /// diagnostic that names the real character and its code point.
    fn unrecognized_char(&self, start: usize) {
        let ch = std::str::from_utf8(&self.source[start..self.pos])
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or(char::REPLACEMENT_CHARACTER);
        let code_point = format!("U+{:04X}", ch as u32);
        let msg = if ch.is_whitespace() || ch.is_control() {
            format!("unrecognized character {code_point}")
        } else {
            format!("unrecognized character '{ch}' ({code_point})")
        };
        let help = match ch {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' => {
                "replace it with an ASCII apostrophe (')"
            }
            '\u{201C}' | '\u{201D}' | '\u{201F}' | '\u{2033}' => {
                "replace it with an ASCII double quote (\")"
            }
            c if c.is_whitespace() => "replace it with an ASCII space",
            c if c.is_alphanumeric() => {
                "identifiers may only contain ASCII letters, digits, '_' and '$'"
            }
            _ => "remove this character",
        };
        self.sink.emit(
            Diagnostic::error(
                DiagnosticCode::new(Category::Error, 100),
                msg,
                self.span_from(start),
            )
            .with_help(help),
        );
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace
            while self.pos < self.source.len() && self.source[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.pos >= self.source.len() {
                return;
            }
            // Line comment: //
            if self.peek() == b'/' && self.peek_at(1) == b'/' {
                self.pos += 2;
                while self.pos < self.source.len() && self.source[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            // Block comment: /* ... */ (non-nesting in Verilog)
            if self.peek() == b'/' && self.peek_at(1) == b'*' {
                let start = self.pos;
                self.pos += 2;
                loop {
                    if self.pos >= self.source.len() {
                        self.error("unterminated block comment", self.span_from(start));
                        break;
                    }
                    if self.source[self.pos] == b'*' && self.peek_at(1) == b'/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            // Compiler directive: `identifier — emit diagnostic, skip line
            if self.peek() == b'`' {
                let start = self.pos;
                self.pos += 1;
                while self.pos < self.source.len() && self.source[self.pos] != b'\n' {
                    self.pos += 1;
                }
                self.error(
                    "compiler directives are not yet supported",
                    self.span_from(start),
                );
                continue;
            }
            break;
        }
    }

    fn next_token(&mut self) -> Token {
        let start = self.pos;
        let b = self.peek();

        // Identifiers and keywords
        if is_ident_start(b) {
            return self.lex_identifier_or_keyword(start);
        }

        // Escaped identifier: \...whitespace
        if b == b'\\' {
            return self.lex_escaped_identifier(start);
        }

        // System identifier: $name
        if b == b'$' {
            return self.lex_system_identifier(start);
        }

        // Numeric literals (possibly sized: 4'b1010)
        if b.is_ascii_digit() {
            return self.lex_number(start);
        }

        // Unsized based literal: 'b1010, 'hFF etc. (tick without preceding digits)
        if b == b'\'' && self.pos + 1 < self.source.len() {
            let next = self.peek_at(1).to_ascii_lowercase();
            if matches!(next, b'b' | b'o' | b'd' | b'h' | b's') {
                return self.lex_unsized_based_literal(start);
            }
        }

        // String literal
        if b == b'"' {
            return self.lex_string(start);
        }

        // Operators and punctuation
        self.lex_operator(start)
    }

    fn lex_identifier_or_keyword(&mut self, start: usize) -> Token {
        while self.pos < self.source.len() && is_ident_char(self.source[self.pos]) {
            self.pos += 1;
        }

        let text = std::str::from_utf8(&self.source[start..self.pos]).unwrap_or("");

        // Check for sized literal: digits followed by tick-base
        // e.g., identifier "4" then we see 'b — but this is handled in lex_number
        let kind = if let Some(kw) = lookup_keyword(text) {
            kw
        } else {
            VerilogToken::Identifier
        };

        Token {
            kind,
            span: self.span_from(start),
        }
    }

    fn lex_escaped_identifier(&mut self, start: usize) -> Token {
        self.pos += 1; // skip backslash
                       // Escaped identifier extends to the next whitespace
        while self.pos < self.source.len() && !self.source[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        if self.pos == start + 1 {
            self.error("empty escaped identifier", self.span_from(start));
            return Token {
                kind: VerilogToken::Error,
                span: self.span_from(start),
            };
        }
        Token {
            kind: VerilogToken::EscapedIdentifier,
            span: self.span_from(start),
        }
    }

    fn lex_system_identifier(&mut self, start: usize) -> Token {
        self.pos += 1; // skip $
        if self.pos < self.source.len() && is_ident_start(self.source[self.pos]) {
            while self.pos < self.source.len() && is_ident_char(self.source[self.pos]) {
                self.pos += 1;
            }
            Token {
                kind: VerilogToken::SystemIdentifier,
                span: self.span_from(start),
            }
        } else {
            self.error("expected identifier after '$'", self.span_from(start));
            Token {
                kind: VerilogToken::Error,
                span: self.span_from(start),
            }
        }
    }

    fn lex_number(&mut self, start: usize) -> Token {
        // Consume digits (and underscores)
        self.eat_decimal_digits();

        // Check for sized literal: size ' [s] base value. IEEE 1364-2005 §3.5.1
        // allows whitespace between the size and the tick; it is only consumed
        // when a tick plus a valid base letter follows.
        if let Some((base, base_end)) = self.based_prefix_at(self.skip_ws_from(self.pos)) {
            self.pos = base_end;
            self.lex_based_value(start, base);
            return Token {
                kind: VerilogToken::SizedLiteral,
                span: self.span_from(start),
            };
        }

        // Check for real literal: digits.digits
        if self.pos < self.source.len()
            && self.source[self.pos] == b'.'
            && self.pos + 1 < self.source.len()
            && self.source[self.pos + 1].is_ascii_digit()
        {
            self.pos += 1; // skip .
            self.eat_decimal_digits();
            self.eat_exponent();
            return Token {
                kind: VerilogToken::RealLiteral,
                span: self.span_from(start),
            };
        }

        // Optional exponent for integer (1e3 is real in Verilog)
        if self.pos < self.source.len()
            && (self.source[self.pos] == b'e' || self.source[self.pos] == b'E')
        {
            self.eat_exponent();
            return Token {
                kind: VerilogToken::RealLiteral,
                span: self.span_from(start),
            };
        }

        Token {
            kind: VerilogToken::IntLiteral,
            span: self.span_from(start),
        }
    }

    /// Lex an unsized based literal starting with tick: `'b1010`, `'hFF`, `'sb1010`
    fn lex_unsized_based_literal(&mut self, start: usize) -> Token {
        if let Some((base, base_end)) = self.based_prefix_at(start) {
            self.pos = base_end;
            self.lex_based_value(start, base);
        } else {
            // `'s` not followed by a base letter
            self.pos = start + 2;
            self.error(
                "expected base specifier ('b', 'o', 'd' or 'h') after signed prefix 's'",
                self.span_from(start),
            );
        }
        Token {
            kind: VerilogToken::SizedLiteral,
            span: self.span_from(start),
        }
    }

    /// Lexes the value of a based literal; `self.pos` is just past the base letter.
    ///
    /// Whitespace between the base and the value is allowed (§3.5.1). Reports
    /// an error when no digits follow the base, or when the value contains a
    /// digit invalid for the base; in the latter case the whole alphanumeric
    /// run is consumed so the literal stays a single token.
    fn lex_based_value(&mut self, start: usize, base: u8) {
        let base_end = self.pos;
        self.pos = self.skip_ws_from(self.pos);
        let digits_start = self.pos;
        let has_digit = self.eat_based_digits(base);
        if self.pos == digits_start && digits_start > base_end {
            // Only whitespace followed the base: the value is missing, and
            // whatever comes next is not part of this literal.
            self.pos = base_end;
            self.missing_based_digits(start, base);
        } else if self.pos < self.source.len() && is_ident_char(self.source[self.pos]) {
            let bad_pos = self.pos;
            let bad = self.source[bad_pos] as char; // ASCII: is_ident_char only accepts ASCII
            while self.pos < self.source.len() && is_ident_char(self.source[self.pos]) {
                self.pos += 1;
            }
            self.error(
                &format!("invalid digit '{bad}' for {} literal", base_name(base)),
                Span::new(self.file, bad_pos as u32, bad_pos as u32 + 1),
            );
        } else if !has_digit {
            self.missing_based_digits(start, base);
        }
    }

    /// Reports a based literal whose base specifier is not followed by any digits.
    fn missing_based_digits(&self, start: usize, base: u8) {
        self.error(
            &format!(
                "missing digits after {} base specifier in based literal",
                base_name(base)
            ),
            self.span_from(start),
        );
    }

    fn eat_decimal_digits(&mut self) {
        while self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch.is_ascii_digit() || ch == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Consumes the digits valid for `base` and returns `true` if at least one
    /// non-underscore digit was consumed. A decimal value may instead be a
    /// single `x`, `z` or `?` digit (§3.5.1).
    fn eat_based_digits(&mut self, base: u8) -> bool {
        if base == b'd' && matches!(self.peek().to_ascii_lowercase(), b'x' | b'z' | b'?') {
            self.pos += 1;
            while self.peek() == b'_' {
                self.pos += 1;
            }
            return true;
        }
        let mut has_digit = false;
        while self.pos < self.source.len() {
            let ch = self.source[self.pos].to_ascii_lowercase();
            let valid = match base {
                b'b' => matches!(ch, b'0' | b'1' | b'x' | b'z' | b'?' | b'_'),
                b'o' => matches!(ch, b'0'..=b'7' | b'x' | b'z' | b'?' | b'_'),
                b'd' => ch.is_ascii_digit() || ch == b'_',
                b'h' => ch.is_ascii_hexdigit() || matches!(ch, b'x' | b'z' | b'?' | b'_'),
                _ => false,
            };
            if valid {
                has_digit |= ch != b'_';
                self.pos += 1;
            } else {
                break;
            }
        }
        has_digit
    }

    fn eat_exponent(&mut self) {
        if self.pos < self.source.len()
            && (self.source[self.pos] == b'e' || self.source[self.pos] == b'E')
        {
            let exp_start = self.pos;
            self.pos += 1;
            if self.pos < self.source.len()
                && (self.source[self.pos] == b'+' || self.source[self.pos] == b'-')
            {
                self.pos += 1;
            }
            if !self.peek().is_ascii_digit() {
                self.error(
                    "missing digits in real literal exponent",
                    self.span_from(exp_start),
                );
                return;
            }
            self.eat_decimal_digits();
        }
    }

    fn lex_string(&mut self, start: usize) -> Token {
        self.pos += 1; // skip opening "
        loop {
            if self.pos >= self.source.len() || self.source[self.pos] == b'\n' {
                self.error("unterminated string literal", self.span_from(start));
                return Token {
                    kind: VerilogToken::Error,
                    span: self.span_from(start),
                };
            }
            if self.source[self.pos] == b'\\' {
                // C-style escape: skip the backslash and the whole next
                // character, never running past EOF (a trailing `\` at EOF
                // falls through to the unterminated-string error).
                self.pos += 1;
                if self.pos < self.source.len() {
                    self.pos = self.char_end(self.pos);
                }
                continue;
            }
            if self.source[self.pos] == b'"' {
                self.pos += 1; // skip closing "
                return Token {
                    kind: VerilogToken::StringLiteral,
                    span: self.span_from(start),
                };
            }
            self.pos += 1;
        }
    }

    fn lex_operator(&mut self, start: usize) -> Token {
        let b = self.advance();
        let kind = match b {
            b'(' => VerilogToken::LeftParen,
            b')' => VerilogToken::RightParen,
            b'[' => VerilogToken::LeftBracket,
            b']' => VerilogToken::RightBracket,
            b'{' => VerilogToken::LeftBrace,
            b'}' => VerilogToken::RightBrace,
            b',' => VerilogToken::Comma,
            b';' => VerilogToken::Semicolon,
            b':' => VerilogToken::Colon,
            b'.' => VerilogToken::Dot,
            b'#' => VerilogToken::Hash,
            b'@' => VerilogToken::At,
            b'?' => VerilogToken::Question,
            b'=' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    if self.peek() == b'=' {
                        self.pos += 1;
                        VerilogToken::TripleEquals
                    } else {
                        VerilogToken::DoubleEquals
                    }
                } else {
                    VerilogToken::Equals
                }
            }
            b'!' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    if self.peek() == b'=' {
                        self.pos += 1;
                        VerilogToken::BangDoubleEquals
                    } else {
                        VerilogToken::BangEquals
                    }
                } else {
                    VerilogToken::Bang
                }
            }
            b'<' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    VerilogToken::LessEquals
                } else if self.peek() == b'<' {
                    self.pos += 1;
                    if self.peek() == b'<' {
                        self.pos += 1;
                        VerilogToken::TripleLess
                    } else {
                        VerilogToken::DoubleLess
                    }
                } else {
                    VerilogToken::LessThan
                }
            }
            b'>' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    VerilogToken::GreaterEquals
                } else if self.peek() == b'>' {
                    self.pos += 1;
                    if self.peek() == b'>' {
                        self.pos += 1;
                        VerilogToken::TripleGreater
                    } else {
                        VerilogToken::DoubleGreater
                    }
                } else {
                    VerilogToken::GreaterThan
                }
            }
            b'+' => VerilogToken::Plus,
            b'-' => VerilogToken::Minus,
            b'*' => {
                if self.peek() == b'*' {
                    self.pos += 1;
                    VerilogToken::DoubleStar
                } else {
                    VerilogToken::Star
                }
            }
            b'/' => VerilogToken::Slash,
            b'%' => VerilogToken::Percent,
            b'&' => {
                if self.peek() == b'&' {
                    self.pos += 1;
                    VerilogToken::DoubleAmpersand
                } else {
                    VerilogToken::Ampersand
                }
            }
            b'|' => {
                if self.peek() == b'|' {
                    self.pos += 1;
                    VerilogToken::DoublePipe
                } else {
                    VerilogToken::Pipe
                }
            }
            b'^' => {
                if self.peek() == b'~' {
                    self.pos += 1;
                    VerilogToken::TildeCaret
                } else {
                    VerilogToken::Caret
                }
            }
            b'~' => {
                if self.peek() == b'^' {
                    self.pos += 1;
                    VerilogToken::TildeCaret
                } else if self.peek() == b'&' {
                    self.pos += 1;
                    VerilogToken::TildeAmpersand
                } else if self.peek() == b'|' {
                    self.pos += 1;
                    VerilogToken::TildePipe
                } else {
                    VerilogToken::Tilde
                }
            }
            _ => {
                // `b` may be the lead byte of a multi-byte UTF-8 character:
                // consume the whole character and report it once.
                self.pos = self.char_end(start);
                self.unrecognized_char(start);
                VerilogToken::Error
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
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Returns the human-readable name of a lowercase base letter (`b`, `o`, `d`, `h`).
fn base_name(base: u8) -> &'static str {
    match base {
        b'b' => "binary",
        b'o' => "octal",
        b'd' => "decimal",
        _ => "hexadecimal",
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

    fn kinds(tokens: &[Token]) -> Vec<VerilogToken> {
        tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn empty_input() {
        let tokens = lex_tokens("");
        assert_eq!(kinds(&tokens), vec![VerilogToken::Eof]);
    }

    #[test]
    fn whitespace_only() {
        let tokens = lex_tokens("  \t\n  ");
        assert_eq!(kinds(&tokens), vec![VerilogToken::Eof]);
    }

    #[test]
    fn keywords_case_sensitive() {
        let tokens = lex_tokens("module Module MODULE");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::Module,
                VerilogToken::Identifier,
                VerilogToken::Identifier,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn all_keyword_categories() {
        let tokens = lex_tokens(
            "always assign begin case default else end endcase endmodule for function generate genvar if initial input integer localparam module output parameter posedge reg repeat signed task while wire",
        );
        let k = kinds(&tokens);
        assert_eq!(k[0], VerilogToken::Always);
        assert_eq!(k[1], VerilogToken::Assign);
        assert_eq!(k[2], VerilogToken::Begin);
        assert!(k.contains(&VerilogToken::Wire));
        assert_eq!(*k.last().unwrap(), VerilogToken::Eof);
    }

    #[test]
    fn identifiers() {
        let tokens = lex_tokens("my_signal clk data_in_0");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::Identifier,
                VerilogToken::Identifier,
                VerilogToken::Identifier,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn escaped_identifier() {
        let tokens = lex_tokens("\\my+signal ");
        assert_eq!(
            kinds(&tokens),
            vec![VerilogToken::EscapedIdentifier, VerilogToken::Eof]
        );
    }

    #[test]
    fn system_identifiers() {
        let tokens = lex_tokens("$display $clog2 $finish");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::SystemIdentifier,
                VerilogToken::SystemIdentifier,
                VerilogToken::SystemIdentifier,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn integer_literals() {
        let tokens = lex_tokens("0 42 1_000_000");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::IntLiteral,
                VerilogToken::IntLiteral,
                VerilogToken::IntLiteral,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn sized_binary_literal() {
        let tokens = lex_tokens("4'b1010");
        assert_eq!(
            kinds(&tokens),
            vec![VerilogToken::SizedLiteral, VerilogToken::Eof]
        );
    }

    #[test]
    fn sized_hex_literal() {
        let tokens = lex_tokens("16'hFF 8'hAB");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::SizedLiteral,
                VerilogToken::SizedLiteral,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn sized_octal_literal() {
        let tokens = lex_tokens("8'o77");
        assert_eq!(
            kinds(&tokens),
            vec![VerilogToken::SizedLiteral, VerilogToken::Eof]
        );
    }

    #[test]
    fn sized_decimal_literal() {
        let tokens = lex_tokens("32'd255");
        assert_eq!(
            kinds(&tokens),
            vec![VerilogToken::SizedLiteral, VerilogToken::Eof]
        );
    }

    #[test]
    fn sized_literal_with_xz() {
        let tokens = lex_tokens("4'bxx0z 8'hxF");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::SizedLiteral,
                VerilogToken::SizedLiteral,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn unsized_based_literal() {
        let tokens = lex_tokens("'b1 'hFF 'd10");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::SizedLiteral,
                VerilogToken::SizedLiteral,
                VerilogToken::SizedLiteral,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn signed_sized_literal() {
        let tokens = lex_tokens("8'sb10101010");
        assert_eq!(
            kinds(&tokens),
            vec![VerilogToken::SizedLiteral, VerilogToken::Eof]
        );
    }

    #[test]
    fn real_literals() {
        let tokens = lex_tokens("1.5 0.0 1.0e3 2.5E-2");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::RealLiteral,
                VerilogToken::RealLiteral,
                VerilogToken::RealLiteral,
                VerilogToken::RealLiteral,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn string_literal() {
        let tokens = lex_tokens("\"hello\" \"world\"");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::StringLiteral,
                VerilogToken::StringLiteral,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn string_literal_with_escapes() {
        let tokens = lex_tokens("\"say \\\"hi\\\"\"");
        assert_eq!(
            kinds(&tokens),
            vec![VerilogToken::StringLiteral, VerilogToken::Eof]
        );
    }

    #[test]
    fn operators_and_punctuation() {
        let tokens = lex_tokens("( ) [ ] { } , ; : . # @ = == != === !== < <= > >= + - * ** / % & && | || ^ ~^ ~ ~& ~| ! << >> <<< >>> ?");
        let k = kinds(&tokens);
        assert_eq!(k[0], VerilogToken::LeftParen);
        assert_eq!(k[1], VerilogToken::RightParen);
        assert_eq!(k[2], VerilogToken::LeftBracket);
        assert_eq!(k[3], VerilogToken::RightBracket);
        assert_eq!(k[4], VerilogToken::LeftBrace);
        assert_eq!(k[5], VerilogToken::RightBrace);
        assert_eq!(k[6], VerilogToken::Comma);
        assert_eq!(k[7], VerilogToken::Semicolon);
        assert_eq!(k[8], VerilogToken::Colon);
        assert_eq!(k[9], VerilogToken::Dot);
        assert_eq!(k[10], VerilogToken::Hash);
        assert_eq!(k[11], VerilogToken::At);
        assert_eq!(k[12], VerilogToken::Equals);
        assert_eq!(k[13], VerilogToken::DoubleEquals);
        assert_eq!(k[14], VerilogToken::BangEquals);
        assert_eq!(k[15], VerilogToken::TripleEquals);
        assert_eq!(k[16], VerilogToken::BangDoubleEquals);
        assert_eq!(k[17], VerilogToken::LessThan);
        assert_eq!(k[18], VerilogToken::LessEquals);
        assert_eq!(k[19], VerilogToken::GreaterThan);
        assert_eq!(k[20], VerilogToken::GreaterEquals);
        assert_eq!(k[21], VerilogToken::Plus);
        assert_eq!(k[22], VerilogToken::Minus);
        assert_eq!(k[23], VerilogToken::Star);
        assert_eq!(k[24], VerilogToken::DoubleStar);
        assert_eq!(k[25], VerilogToken::Slash);
        assert_eq!(k[26], VerilogToken::Percent);
        assert_eq!(k[27], VerilogToken::Ampersand);
        assert_eq!(k[28], VerilogToken::DoubleAmpersand);
        assert_eq!(k[29], VerilogToken::Pipe);
        assert_eq!(k[30], VerilogToken::DoublePipe);
        assert_eq!(k[31], VerilogToken::Caret);
        assert_eq!(k[32], VerilogToken::TildeCaret);
        assert_eq!(k[33], VerilogToken::Tilde);
        assert_eq!(k[34], VerilogToken::TildeAmpersand);
        assert_eq!(k[35], VerilogToken::TildePipe);
        assert_eq!(k[36], VerilogToken::Bang);
        assert_eq!(k[37], VerilogToken::DoubleLess);
        assert_eq!(k[38], VerilogToken::DoubleGreater);
        assert_eq!(k[39], VerilogToken::TripleLess);
        assert_eq!(k[40], VerilogToken::TripleGreater);
        assert_eq!(k[41], VerilogToken::Question);
        assert_eq!(k[42], VerilogToken::Eof);
    }

    #[test]
    fn line_comment() {
        let tokens = lex_tokens("wire // this is a comment\nclk");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::Wire,
                VerilogToken::Identifier,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn block_comment() {
        let tokens = lex_tokens("wire /* block\ncomment */ clk");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::Wire,
                VerilogToken::Identifier,
                VerilogToken::Eof,
            ]
        );
    }

    #[test]
    fn compiler_directive_skipped() {
        let (tokens, errors) = lex_tokens_with_errors("`timescale 1ns/1ps\nmodule top;");
        let k = kinds(&tokens);
        assert_eq!(k[0], VerilogToken::Module);
        assert!(!errors.is_empty());
    }

    #[test]
    fn unterminated_string_error() {
        let (tokens, errors) = lex_tokens_with_errors("\"unterminated\n");
        assert!(tokens.iter().any(|t| t.kind == VerilogToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn unterminated_block_comment_error() {
        let (tokens, errors) = lex_tokens_with_errors("/* unterminated");
        // Should still produce Eof
        assert_eq!(tokens.last().unwrap().kind, VerilogToken::Eof);
        assert!(!errors.is_empty());
    }

    #[test]
    fn unrecognized_char_error() {
        let (tokens, errors) = lex_tokens_with_errors("§");
        assert!(tokens.iter().any(|t| t.kind == VerilogToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn spans_are_correct() {
        let tokens = lex_tokens("module top");
        // "module" is bytes 0..6, "top" is bytes 7..10
        assert_eq!(tokens[0].span.start, 0);
        assert_eq!(tokens[0].span.end, 6);
        assert_eq!(tokens[1].span.start, 7);
        assert_eq!(tokens[1].span.end, 10);
    }

    #[test]
    fn eof_always_present() {
        let tokens = lex_tokens("module");
        assert_eq!(tokens.last().unwrap().kind, VerilogToken::Eof);
    }

    #[test]
    fn empty_escaped_identifier_error() {
        let (tokens, errors) = lex_tokens_with_errors("\\ ");
        assert!(tokens.iter().any(|t| t.kind == VerilogToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn dollar_without_ident_error() {
        let (tokens, errors) = lex_tokens_with_errors("$ ;");
        assert!(tokens.iter().any(|t| t.kind == VerilogToken::Error));
        assert!(!errors.is_empty());
    }

    /// Asserts every span lies within `source` on UTF-8 character boundaries.
    fn assert_spans_valid(source: &str, tokens: &[Token], errors: &[Diagnostic]) {
        let spans = tokens
            .iter()
            .map(|t| t.span)
            .chain(errors.iter().map(|d| d.primary_span));
        for span in spans {
            assert!(span.start <= span.end, "inverted span {span:?}");
            assert!(source.get(span.start as usize..span.end as usize).is_some());
        }
    }

    #[test]
    fn multibyte_char_single_error() {
        let src = "wire a\u{2019};";
        let (tokens, errors) = lex_tokens_with_errors(src);
        assert_spans_valid(src, &tokens, &errors);
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::Wire,
                VerilogToken::Identifier,
                VerilogToken::Error,
                VerilogToken::Semicolon,
                VerilogToken::Eof,
            ]
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].message,
            "unrecognized character '\u{2019}' (U+2019)"
        );
        assert!(errors[0].help[0].contains("apostrophe"));
        assert_eq!(
            &src[tokens[2].span.start as usize..tokens[2].span.end as usize],
            "\u{2019}"
        );
    }

    #[test]
    fn non_ascii_letter_in_identifier() {
        let src = "wire caf\u{e9};";
        let (tokens, errors) = lex_tokens_with_errors(src);
        assert_spans_valid(src, &tokens, &errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].message,
            "unrecognized character '\u{e9}' (U+00E9)"
        );
        assert_eq!(tokens[3].kind, VerilogToken::Semicolon);
    }

    #[test]
    fn nbsp_reported_by_code_point() {
        let src = "wire\u{a0}a;";
        let (tokens, errors) = lex_tokens_with_errors(src);
        assert_spans_valid(src, &tokens, &errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "unrecognized character U+00A0");
        assert!(errors[0].help[0].contains("space"));
    }

    #[test]
    fn four_byte_char_single_error() {
        let src = "a \u{1F600} b";
        let (tokens, errors) = lex_tokens_with_errors(src);
        assert_spans_valid(src, &tokens, &errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(tokens.len(), 4);
    }

    #[test]
    fn string_trailing_backslash_at_eof() {
        let src = "module m; initial $display(\"abc\\";
        let (tokens, errors) = lex_tokens_with_errors(src);
        assert_spans_valid(src, &tokens, &errors);
        assert!(errors
            .iter()
            .any(|d| d.message == "unterminated string literal"));
        let eof = tokens.last().unwrap();
        assert_eq!(eof.kind, VerilogToken::Eof);
        assert_eq!(eof.span.end as usize, src.len());
    }

    #[test]
    fn string_escaped_multibyte_char() {
        let src = "\"a\\\u{e9}b\"";
        let tokens = lex_tokens(src);
        assert_eq!(
            kinds(&tokens),
            vec![VerilogToken::StringLiteral, VerilogToken::Eof]
        );
        assert_eq!(tokens[0].span.end as usize, src.len());
    }

    #[test]
    fn sized_literal_whitespace_before_tick_and_after_base() {
        let tokens = lex_tokens("8 'hFF 8'h FF 4 'sb 1010 16'd\n255");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::SizedLiteral,
                VerilogToken::SizedLiteral,
                VerilogToken::SizedLiteral,
                VerilogToken::SizedLiteral,
                VerilogToken::Eof,
            ]
        );
        assert_eq!(tokens[0].span.start, 0);
        assert_eq!(tokens[0].span.end, 6);
    }

    #[test]
    fn int_followed_by_tick_without_base_is_not_sized() {
        // `8 '(` is not a based literal: the whitespace is not consumed.
        let (tokens, _) = lex_tokens_with_errors("8 '(x)");
        assert_eq!(tokens[0].kind, VerilogToken::IntLiteral);
        assert_eq!(tokens[0].span.end, 1);
    }

    #[test]
    fn tick_and_base_must_be_adjacent() {
        let (tokens, _) = lex_tokens_with_errors("8' hFF");
        assert_eq!(tokens[0].kind, VerilogToken::IntLiteral);
        assert_ne!(tokens[1].kind, VerilogToken::SizedLiteral);
    }

    #[test]
    fn based_literal_missing_digits() {
        let (tokens, errors) = lex_tokens_with_errors("4'b;");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("missing digits after binary"));
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::SizedLiteral,
                VerilogToken::Semicolon,
                VerilogToken::Eof
            ]
        );
        assert_eq!(tokens[0].span.end, 3);
    }

    #[test]
    fn based_literal_missing_digits_after_whitespace() {
        let (tokens, errors) = lex_tokens_with_errors("'h ;");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("hexadecimal"));
        assert_eq!(tokens[0].span.end, 2);
        assert_eq!(tokens[1].kind, VerilogToken::Semicolon);
    }

    #[test]
    fn based_literal_invalid_digit() {
        let (tokens, errors) = lex_tokens_with_errors("4'b102;");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "invalid digit '2' for binary literal");
        assert_eq!(errors[0].primary_span.start, 5);
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::SizedLiteral,
                VerilogToken::Semicolon,
                VerilogToken::Eof
            ]
        );

        let (_, errors) = lex_tokens_with_errors("8'o78 8'hFG");
        assert_eq!(errors.len(), 2);
        assert!(errors[0].message.contains("octal"));
        assert!(errors[1].message.contains("'G' for hexadecimal"));
    }

    #[test]
    fn decimal_literal_single_xz_digit() {
        let tokens = lex_tokens("8'dx 8'dZ 'd? 4'sdx_");
        assert_eq!(tokens.len(), 5);
        assert!(tokens[..4]
            .iter()
            .all(|t| t.kind == VerilogToken::SizedLiteral));
        let (_, errors) = lex_tokens_with_errors("8'dx1");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn signed_prefix_without_base_error() {
        let (_, errors) = lex_tokens_with_errors("'s ;");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("signed prefix"));
    }

    #[test]
    fn real_exponent_missing_digits() {
        for src in ["1e", "1e+;", "2.5E-"] {
            let (tokens, errors) = lex_tokens_with_errors(src);
            assert_eq!(errors.len(), 1, "{src}");
            assert_eq!(errors[0].message, "missing digits in real literal exponent");
            assert_eq!(tokens[0].kind, VerilogToken::RealLiteral);
        }
        let tokens = lex_tokens("1e3 1E+3 1.5e-2");
        assert_eq!(tokens.len(), 4);
    }

    #[test]
    fn dollar_inside_identifier() {
        let tokens = lex_tokens("a$b $c$d");
        assert_eq!(
            kinds(&tokens),
            vec![
                VerilogToken::Identifier,
                VerilogToken::SystemIdentifier,
                VerilogToken::Eof
            ]
        );
        assert_eq!(tokens[0].span.end, 3);
        // `$` is not an identifier start character.
        assert_eq!(lex_tokens("$x")[0].kind, VerilogToken::SystemIdentifier);
    }
}
