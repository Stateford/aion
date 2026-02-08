//! Lexical analyzer for SystemVerilog-2017 source text.
//!
//! Converts source text into a sequence of [`Token`]s, handling case-sensitive
//! keywords, sized/based literals (`4'b1010`), string literals with C-style escapes,
//! line and block comments, escaped identifiers, system identifiers, and compiler
//! directives. Extends the Verilog-2005 lexer with SystemVerilog operators
//! (`++`, `--`, `+=`, `-=`, `::`, `->`, `==?`, `!=?`, etc.) and keywords.
//! Errors are reported to the [`DiagnosticSink`] and produce [`SvToken::Error`] tokens.

use crate::token::{lookup_keyword, SvToken, Token};
use aion_diagnostics::code::{Category, DiagnosticCode};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, Span};

/// Lexes the given SystemVerilog source text into a vector of tokens.
///
/// Whitespace and comments are skipped. The returned vector always ends with
/// a [`SvToken::Eof`] token. Lexer errors are reported via the diagnostic
/// sink and produce [`SvToken::Error`] tokens in the output.
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
                    kind: SvToken::Eof,
                    span: Span::new(self.file, self.pos as u32, self.pos as u32),
                });
                break;
            }
            tokens.push(self.next_token());
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

    fn error(&self, msg: &str, span: Span) {
        self.sink.emit(Diagnostic::error(
            DiagnosticCode::new(Category::Error, 100),
            msg,
            span,
        ));
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
            // Block comment: /* ... */
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
        let kind = if let Some(kw) = lookup_keyword(text) {
            kw
        } else {
            SvToken::Identifier
        };

        Token {
            kind,
            span: self.span_from(start),
        }
    }

    fn lex_escaped_identifier(&mut self, start: usize) -> Token {
        self.pos += 1; // skip backslash
        while self.pos < self.source.len() && !self.source[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        if self.pos == start + 1 {
            self.error("empty escaped identifier", self.span_from(start));
            return Token {
                kind: SvToken::Error,
                span: self.span_from(start),
            };
        }
        Token {
            kind: SvToken::EscapedIdentifier,
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
                kind: SvToken::SystemIdentifier,
                span: self.span_from(start),
            }
        } else {
            self.error("expected identifier after '$'", self.span_from(start));
            Token {
                kind: SvToken::Error,
                span: self.span_from(start),
            }
        }
    }

    fn lex_number(&mut self, start: usize) -> Token {
        self.eat_decimal_digits();

        // Check for sized literal: digits ' base digits
        if self.pos < self.source.len() && self.source[self.pos] == b'\'' {
            let next = if self.pos + 1 < self.source.len() {
                self.source[self.pos + 1].to_ascii_lowercase()
            } else {
                0
            };
            // 's is signed prefix before base
            if next == b's' {
                let base = if self.pos + 2 < self.source.len() {
                    self.source[self.pos + 2].to_ascii_lowercase()
                } else {
                    0
                };
                if matches!(base, b'b' | b'o' | b'd' | b'h') {
                    self.pos += 3;
                    self.eat_based_digits(base);
                    return Token {
                        kind: SvToken::SizedLiteral,
                        span: self.span_from(start),
                    };
                }
            }
            if matches!(next, b'b' | b'o' | b'd' | b'h') {
                self.pos += 2;
                self.eat_based_digits(next);
                return Token {
                    kind: SvToken::SizedLiteral,
                    span: self.span_from(start),
                };
            }
        }

        // Check for real literal: digits.digits
        if self.pos < self.source.len()
            && self.source[self.pos] == b'.'
            && self.pos + 1 < self.source.len()
            && self.source[self.pos + 1].is_ascii_digit()
        {
            self.pos += 1;
            self.eat_decimal_digits();
            self.eat_exponent();
            return Token {
                kind: SvToken::RealLiteral,
                span: self.span_from(start),
            };
        }

        // Optional exponent for integer (1e3 is real in Verilog)
        if self.pos < self.source.len()
            && (self.source[self.pos] == b'e' || self.source[self.pos] == b'E')
        {
            self.eat_exponent();
            return Token {
                kind: SvToken::RealLiteral,
                span: self.span_from(start),
            };
        }

        Token {
            kind: SvToken::IntLiteral,
            span: self.span_from(start),
        }
    }

    /// Lex an unsized based literal starting with tick: `'b1010`, `'hFF`, `'sb1010`
    fn lex_unsized_based_literal(&mut self, start: usize) -> Token {
        self.pos += 1; // skip '
        let next = self.source[self.pos].to_ascii_lowercase();
        if next == b's' {
            self.pos += 1; // skip s
            let base = if self.pos < self.source.len() {
                self.source[self.pos].to_ascii_lowercase()
            } else {
                0
            };
            if matches!(base, b'b' | b'o' | b'd' | b'h') {
                self.pos += 1;
                self.eat_based_digits(base);
            }
        } else {
            self.pos += 1; // skip base letter
            self.eat_based_digits(next);
        }
        Token {
            kind: SvToken::SizedLiteral,
            span: self.span_from(start),
        }
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

    fn eat_based_digits(&mut self, base: u8) {
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
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn eat_exponent(&mut self) {
        if self.pos < self.source.len()
            && (self.source[self.pos] == b'e' || self.source[self.pos] == b'E')
        {
            self.pos += 1;
            if self.pos < self.source.len()
                && (self.source[self.pos] == b'+' || self.source[self.pos] == b'-')
            {
                self.pos += 1;
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
                    kind: SvToken::Error,
                    span: self.span_from(start),
                };
            }
            if self.source[self.pos] == b'\\' {
                self.pos += 2;
                continue;
            }
            if self.source[self.pos] == b'"' {
                self.pos += 1;
                return Token {
                    kind: SvToken::StringLiteral,
                    span: self.span_from(start),
                };
            }
            self.pos += 1;
        }
    }

    fn lex_operator(&mut self, start: usize) -> Token {
        let b = self.advance();
        let kind = match b {
            b'(' => SvToken::LeftParen,
            b')' => SvToken::RightParen,
            b'[' => SvToken::LeftBracket,
            b']' => SvToken::RightBracket,
            b'{' => SvToken::LeftBrace,
            b'}' => SvToken::RightBrace,
            b',' => SvToken::Comma,
            b';' => SvToken::Semicolon,
            b'.' => SvToken::Dot,
            b'#' => SvToken::Hash,
            b'@' => SvToken::At,
            b'?' => SvToken::Question,
            b'\'' => SvToken::Tick,
            b':' => {
                if self.peek() == b':' {
                    self.pos += 1;
                    SvToken::ColonColon
                } else {
                    SvToken::Colon
                }
            }
            b'=' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    if self.peek() == b'=' {
                        self.pos += 1;
                        SvToken::TripleEquals
                    } else if self.peek() == b'?' {
                        self.pos += 1;
                        SvToken::WildcardEq
                    } else {
                        SvToken::DoubleEquals
                    }
                } else {
                    SvToken::Equals
                }
            }
            b'!' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    if self.peek() == b'=' {
                        self.pos += 1;
                        SvToken::BangDoubleEquals
                    } else if self.peek() == b'?' {
                        self.pos += 1;
                        SvToken::WildcardNeq
                    } else {
                        SvToken::BangEquals
                    }
                } else {
                    SvToken::Bang
                }
            }
            b'<' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::LessEquals
                } else if self.peek() == b'<' {
                    self.pos += 1;
                    if self.peek() == b'<' {
                        self.pos += 1;
                        if self.peek() == b'=' {
                            self.pos += 1;
                            SvToken::TripleLessEquals
                        } else {
                            SvToken::TripleLess
                        }
                    } else if self.peek() == b'=' {
                        self.pos += 1;
                        SvToken::DoubleLessEquals
                    } else {
                        SvToken::DoubleLess
                    }
                } else {
                    SvToken::LessThan
                }
            }
            b'>' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::GreaterEquals
                } else if self.peek() == b'>' {
                    self.pos += 1;
                    if self.peek() == b'>' {
                        self.pos += 1;
                        if self.peek() == b'=' {
                            self.pos += 1;
                            SvToken::TripleGreaterEquals
                        } else {
                            SvToken::TripleGreater
                        }
                    } else if self.peek() == b'=' {
                        self.pos += 1;
                        SvToken::DoubleGreaterEquals
                    } else {
                        SvToken::DoubleGreater
                    }
                } else {
                    SvToken::GreaterThan
                }
            }
            b'+' => {
                if self.peek() == b'+' {
                    self.pos += 1;
                    SvToken::PlusPlus
                } else if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::PlusEquals
                } else {
                    SvToken::Plus
                }
            }
            b'-' => {
                if self.peek() == b'-' {
                    self.pos += 1;
                    SvToken::MinusMinus
                } else if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::MinusEquals
                } else if self.peek() == b'>' {
                    self.pos += 1;
                    SvToken::Arrow
                } else {
                    SvToken::Minus
                }
            }
            b'*' => {
                if self.peek() == b'*' {
                    self.pos += 1;
                    SvToken::DoubleStar
                } else if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::StarEquals
                } else {
                    SvToken::Star
                }
            }
            b'/' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::SlashEquals
                } else {
                    SvToken::Slash
                }
            }
            b'%' => {
                if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::PercentEquals
                } else {
                    SvToken::Percent
                }
            }
            b'&' => {
                if self.peek() == b'&' {
                    self.pos += 1;
                    SvToken::DoubleAmpersand
                } else if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::AmpersandEquals
                } else {
                    SvToken::Ampersand
                }
            }
            b'|' => {
                if self.peek() == b'|' {
                    self.pos += 1;
                    SvToken::DoublePipe
                } else if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::PipeEquals
                } else {
                    SvToken::Pipe
                }
            }
            b'^' => {
                if self.peek() == b'~' {
                    self.pos += 1;
                    SvToken::TildeCaret
                } else if self.peek() == b'=' {
                    self.pos += 1;
                    SvToken::CaretEquals
                } else {
                    SvToken::Caret
                }
            }
            b'~' => {
                if self.peek() == b'^' {
                    self.pos += 1;
                    SvToken::TildeCaret
                } else if self.peek() == b'&' {
                    self.pos += 1;
                    SvToken::TildeAmpersand
                } else if self.peek() == b'|' {
                    self.pos += 1;
                    SvToken::TildePipe
                } else {
                    SvToken::Tilde
                }
            }
            _ => {
                self.error(
                    &format!("unrecognized character '{}'", b as char),
                    self.span_from(start),
                );
                SvToken::Error
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

    fn kinds(tokens: &[Token]) -> Vec<SvToken> {
        tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn empty_input() {
        let tokens = lex_tokens("");
        assert_eq!(kinds(&tokens), vec![SvToken::Eof]);
    }

    #[test]
    fn whitespace_only() {
        let tokens = lex_tokens("  \t\n  ");
        assert_eq!(kinds(&tokens), vec![SvToken::Eof]);
    }

    #[test]
    fn keywords_case_sensitive() {
        let tokens = lex_tokens("module Module MODULE");
        assert_eq!(
            kinds(&tokens),
            vec![
                SvToken::Module,
                SvToken::Identifier,
                SvToken::Identifier,
                SvToken::Eof,
            ]
        );
    }

    #[test]
    fn sv_keywords() {
        let tokens = lex_tokens(
            "always_comb always_ff always_latch logic bit int interface package import modport",
        );
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::AlwaysComb);
        assert_eq!(k[1], SvToken::AlwaysFf);
        assert_eq!(k[2], SvToken::AlwaysLatch);
        assert_eq!(k[3], SvToken::Logic);
        assert_eq!(k[4], SvToken::Bit);
        assert_eq!(k[5], SvToken::Int);
        assert_eq!(k[6], SvToken::Interface);
        assert_eq!(k[7], SvToken::Package);
        assert_eq!(k[8], SvToken::Import);
        assert_eq!(k[9], SvToken::Modport);
    }

    #[test]
    fn sv_type_keywords() {
        let tokens = lex_tokens("enum struct typedef byte longint shortint");
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::Enum);
        assert_eq!(k[1], SvToken::Struct);
        assert_eq!(k[2], SvToken::Typedef);
        assert_eq!(k[3], SvToken::Byte);
        assert_eq!(k[4], SvToken::Longint);
        assert_eq!(k[5], SvToken::Shortint);
    }

    #[test]
    fn sv_statement_keywords() {
        let tokens = lex_tokens("unique priority return break continue do foreach");
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::Unique);
        assert_eq!(k[1], SvToken::Priority);
        assert_eq!(k[2], SvToken::Return);
        assert_eq!(k[3], SvToken::Break);
        assert_eq!(k[4], SvToken::Continue);
        assert_eq!(k[5], SvToken::Do);
        assert_eq!(k[6], SvToken::Foreach);
    }

    #[test]
    fn identifiers() {
        let tokens = lex_tokens("my_signal clk data_in_0");
        assert_eq!(
            kinds(&tokens),
            vec![
                SvToken::Identifier,
                SvToken::Identifier,
                SvToken::Identifier,
                SvToken::Eof,
            ]
        );
    }

    #[test]
    fn escaped_identifier() {
        let tokens = lex_tokens("\\my+signal ");
        assert_eq!(
            kinds(&tokens),
            vec![SvToken::EscapedIdentifier, SvToken::Eof]
        );
    }

    #[test]
    fn system_identifiers() {
        let tokens = lex_tokens("$display $clog2 $finish");
        assert_eq!(
            kinds(&tokens),
            vec![
                SvToken::SystemIdentifier,
                SvToken::SystemIdentifier,
                SvToken::SystemIdentifier,
                SvToken::Eof,
            ]
        );
    }

    #[test]
    fn integer_literals() {
        let tokens = lex_tokens("0 42 1_000_000");
        assert_eq!(
            kinds(&tokens),
            vec![
                SvToken::IntLiteral,
                SvToken::IntLiteral,
                SvToken::IntLiteral,
                SvToken::Eof,
            ]
        );
    }

    #[test]
    fn sized_binary_literal() {
        let tokens = lex_tokens("4'b1010");
        assert_eq!(kinds(&tokens), vec![SvToken::SizedLiteral, SvToken::Eof]);
    }

    #[test]
    fn sized_hex_literal() {
        let tokens = lex_tokens("16'hFF 8'hAB");
        assert_eq!(
            kinds(&tokens),
            vec![SvToken::SizedLiteral, SvToken::SizedLiteral, SvToken::Eof]
        );
    }

    #[test]
    fn real_literals() {
        let tokens = lex_tokens("1.5 0.0 1.0e3 2.5E-2");
        assert_eq!(
            kinds(&tokens),
            vec![
                SvToken::RealLiteral,
                SvToken::RealLiteral,
                SvToken::RealLiteral,
                SvToken::RealLiteral,
                SvToken::Eof,
            ]
        );
    }

    #[test]
    fn string_literal() {
        let tokens = lex_tokens("\"hello\" \"world\"");
        assert_eq!(
            kinds(&tokens),
            vec![SvToken::StringLiteral, SvToken::StringLiteral, SvToken::Eof]
        );
    }

    #[test]
    fn verilog_operators() {
        let tokens = lex_tokens("( ) [ ] { } , ; : . # @ = == != === !== < <= > >= + - * ** / % & && | || ^ ~^ ~ ~& ~| ! << >> <<< >>> ?");
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::LeftParen);
        assert_eq!(k[1], SvToken::RightParen);
        assert_eq!(k[2], SvToken::LeftBracket);
        assert_eq!(k[3], SvToken::RightBracket);
        assert_eq!(k[12], SvToken::Equals);
        assert_eq!(k[13], SvToken::DoubleEquals);
        assert_eq!(k[14], SvToken::BangEquals);
        assert_eq!(k[15], SvToken::TripleEquals);
        assert_eq!(k[16], SvToken::BangDoubleEquals);
        assert_eq!(k[41], SvToken::Question);
    }

    #[test]
    fn sv_increment_decrement() {
        let tokens = lex_tokens("++ --");
        assert_eq!(
            kinds(&tokens),
            vec![SvToken::PlusPlus, SvToken::MinusMinus, SvToken::Eof]
        );
    }

    #[test]
    fn sv_compound_assignments() {
        let tokens = lex_tokens("+= -= *= /= %= &= |= ^=");
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::PlusEquals);
        assert_eq!(k[1], SvToken::MinusEquals);
        assert_eq!(k[2], SvToken::StarEquals);
        assert_eq!(k[3], SvToken::SlashEquals);
        assert_eq!(k[4], SvToken::PercentEquals);
        assert_eq!(k[5], SvToken::AmpersandEquals);
        assert_eq!(k[6], SvToken::PipeEquals);
        assert_eq!(k[7], SvToken::CaretEquals);
    }

    #[test]
    fn sv_shift_assignments() {
        let tokens = lex_tokens("<<= >>= <<<= >>>=");
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::DoubleLessEquals);
        assert_eq!(k[1], SvToken::DoubleGreaterEquals);
        assert_eq!(k[2], SvToken::TripleLessEquals);
        assert_eq!(k[3], SvToken::TripleGreaterEquals);
    }

    #[test]
    fn sv_scope_resolution() {
        let tokens = lex_tokens("pkg::name");
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::Identifier);
        assert_eq!(k[1], SvToken::ColonColon);
        assert_eq!(k[2], SvToken::Identifier);
    }

    #[test]
    fn sv_arrow() {
        let tokens = lex_tokens("->");
        assert_eq!(kinds(&tokens), vec![SvToken::Arrow, SvToken::Eof]);
    }

    #[test]
    fn sv_wildcard_equality() {
        let tokens = lex_tokens("==? !=?");
        assert_eq!(
            kinds(&tokens),
            vec![SvToken::WildcardEq, SvToken::WildcardNeq, SvToken::Eof]
        );
    }

    #[test]
    fn sv_tick() {
        let tokens = lex_tokens("int'(x)");
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::Int);
        assert_eq!(k[1], SvToken::Tick);
        assert_eq!(k[2], SvToken::LeftParen);
    }

    #[test]
    fn line_comment() {
        let tokens = lex_tokens("wire // this is a comment\nclk");
        assert_eq!(
            kinds(&tokens),
            vec![SvToken::Wire, SvToken::Identifier, SvToken::Eof]
        );
    }

    #[test]
    fn block_comment() {
        let tokens = lex_tokens("wire /* block\ncomment */ clk");
        assert_eq!(
            kinds(&tokens),
            vec![SvToken::Wire, SvToken::Identifier, SvToken::Eof]
        );
    }

    #[test]
    fn compiler_directive_skipped() {
        let (tokens, errors) = lex_tokens_with_errors("`timescale 1ns/1ps\nmodule top;");
        let k = kinds(&tokens);
        assert_eq!(k[0], SvToken::Module);
        assert!(!errors.is_empty());
    }

    #[test]
    fn unterminated_string_error() {
        let (tokens, errors) = lex_tokens_with_errors("\"unterminated\n");
        assert!(tokens.iter().any(|t| t.kind == SvToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn unterminated_block_comment_error() {
        let (tokens, errors) = lex_tokens_with_errors("/* unterminated");
        assert_eq!(tokens.last().unwrap().kind, SvToken::Eof);
        assert!(!errors.is_empty());
    }

    #[test]
    fn unrecognized_char_error() {
        let (tokens, errors) = lex_tokens_with_errors("§");
        assert!(tokens.iter().any(|t| t.kind == SvToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn spans_are_correct() {
        let tokens = lex_tokens("module top");
        assert_eq!(tokens[0].span.start, 0);
        assert_eq!(tokens[0].span.end, 6);
        assert_eq!(tokens[1].span.start, 7);
        assert_eq!(tokens[1].span.end, 10);
    }

    #[test]
    fn empty_escaped_identifier_error() {
        let (tokens, errors) = lex_tokens_with_errors("\\ ");
        assert!(tokens.iter().any(|t| t.kind == SvToken::Error));
        assert!(!errors.is_empty());
    }

    #[test]
    fn dollar_without_ident_error() {
        let (tokens, errors) = lex_tokens_with_errors("$ ;");
        assert!(tokens.iter().any(|t| t.kind == SvToken::Error));
        assert!(!errors.is_empty());
    }
}
