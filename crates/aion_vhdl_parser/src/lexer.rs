//! Lexical analyzer for VHDL-2008 source text.
//!
//! Converts source text into a sequence of [`Token`]s, handling case-insensitive
//! keywords, based literals, string/character/bit-string literals, block and line
//! comments, and extended identifiers. Errors are reported to the [`DiagnosticSink`]
//! and produce [`VhdlToken::Error`] tokens.

use crate::token::{lookup_keyword, Token, VhdlToken};
use aion_diagnostics::code::{Category, DiagnosticCode};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, Span};

/// Lexes the given source text into a vector of tokens.
///
/// Whitespace and comments are skipped. The returned vector always ends with
/// a [`VhdlToken::Eof`] token. Lexer errors are reported via the diagnostic
/// sink and produce [`VhdlToken::Error`] tokens in the output.
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

impl<'a> Lexer<'a> {
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
            // Line comment: --
            if self.peek() == b'-' && self.peek_at(1) == b'-' {
                self.pos += 2;
                while self.pos < self.source.len() && self.source[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            // Block comment: /* ... */ (VHDL-2008)
            if self.peek() == b'/' && self.peek_at(1) == b'*' {
                let start = self.pos;
                self.pos += 2;
                let mut depth = 1;
                while self.pos < self.source.len() && depth > 0 {
                    if self.source[self.pos] == b'/' && self.peek_at(1) == b'*' {
                        depth += 1;
                        self.pos += 2;
                    } else if self.source[self.pos] == b'*' && self.peek_at(1) == b'/' {
                        depth -= 1;
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                    }
                }
                if depth > 0 {
                    self.error("unterminated block comment", self.span_from(start));
                }
                continue;
            }
            break;
        }
    }

    fn next_token(&mut self) -> Token {
        let start = self.pos;
        let b = self.peek();

        // Character literal: 'X' (single character between single quotes)
        // Must check this before identifiers since tick is also used for attributes.
        // Character literal: exactly '<char>' where <char> is any printable char
        if b == b'\'' && self.pos + 2 < self.source.len() && self.source[self.pos + 2] == b'\'' {
            // Check that the next char after closing quote is not an identifier char
            // (to avoid matching things like 'a' in the middle of names)
            let after = if self.pos + 3 < self.source.len() {
                self.source[self.pos + 3]
            } else {
                0
            };
            if !is_ident_char(after) {
                self.pos += 3;
                return Token {
                    kind: VhdlToken::CharLiteral,
                    span: self.span_from(start),
                };
            }
        }

        // Identifiers and keywords
        if is_ident_start(b) {
            return self.lex_identifier_or_keyword(start);
        }

        // Extended identifier: \...\
        if b == b'\\' {
            return self.lex_extended_identifier(start);
        }

        // Numeric literals
        if b.is_ascii_digit() {
            return self.lex_number(start);
        }

        // String literal
        if b == b'"' {
            return self.lex_string(start);
        }

        // Bit string literal: B"...", O"...", X"...", etc.
        // Handled within lex_identifier_or_keyword for base prefixes

        // Operators and punctuation
        self.lex_operator(start)
    }

    fn lex_identifier_or_keyword(&mut self, start: usize) -> Token {
        while self.pos < self.source.len() && is_ident_char(self.source[self.pos]) {
            self.pos += 1;
        }

        let text = &self.source[start..self.pos];

        // Check for bit string literal prefix: B"...", O"...", X"...", etc.
        if self.pos < self.source.len() && self.source[self.pos] == b'"' && text.len() <= 2 {
            let lower: Vec<u8> = text.iter().map(|b| b.to_ascii_lowercase()).collect();
            let is_bit_prefix = matches!(
                lower.as_slice(),
                b"b" | b"o" | b"x" | b"ub" | b"uo" | b"ux" | b"sb" | b"so" | b"sx" | b"d"
            );
            if is_bit_prefix {
                // Consume the string part
                self.pos += 1; // skip opening "
                while self.pos < self.source.len() && self.source[self.pos] != b'"' {
                    if self.source[self.pos] == b'\n' {
                        self.error("unterminated bit string literal", self.span_from(start));
                        return Token {
                            kind: VhdlToken::Error,
                            span: self.span_from(start),
                        };
                    }
                    self.pos += 1;
                }
                if self.pos < self.source.len() {
                    self.pos += 1; // skip closing "
                }
                return Token {
                    kind: VhdlToken::BitStringLiteral,
                    span: self.span_from(start),
                };
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

    fn lex_number(&mut self, start: usize) -> Token {
        // Consume digits (and underscores)
        self.eat_digits();

        // Check for based literal: digits#...#
        if self.pos < self.source.len() && self.source[self.pos] == b'#' {
            self.pos += 1; // skip #
                           // Consume based digits (hex + underscores + optional dot)
            while self.pos < self.source.len() {
                let ch = self.source[self.pos];
                if ch.is_ascii_hexdigit() || ch == b'_' || ch == b'.' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // Expect closing #
            if self.pos < self.source.len() && self.source[self.pos] == b'#' {
                self.pos += 1;
            } else {
                self.error(
                    "expected closing '#' in based literal",
                    self.span_from(start),
                );
                return Token {
                    kind: VhdlToken::Error,
                    span: self.span_from(start),
                };
            }
            // Optional exponent
            self.eat_exponent();
            // Determine if integer or real based on presence of '.'
            let text = &self.source[start..self.pos];
            let kind = if text.contains(&b'.') {
                VhdlToken::RealLiteral
            } else {
                VhdlToken::IntLiteral
            };
            return Token {
                kind,
                span: self.span_from(start),
            };
        }

        // Check for real literal: digits.digits
        if self.pos < self.source.len()
            && self.source[self.pos] == b'.'
            && self.peek_at(1).is_ascii_digit()
        {
            self.pos += 1; // skip .
            self.eat_digits();
            self.eat_exponent();
            return Token {
                kind: VhdlToken::RealLiteral,
                span: self.span_from(start),
            };
        }

        // Optional exponent for integer
        self.eat_exponent();

        Token {
            kind: VhdlToken::IntLiteral,
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
            self.eat_digits();
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
            b'|' => VhdlToken::Bar,
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
                _ => {
                    self.error("unexpected character after '?'", self.span_from(start));
                    VhdlToken::Error
                }
            },
            _ => {
                self.error(
                    &format!("unrecognized character '{}'", b as char),
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
        let tokens = lex_tokens("signal /* outer /* inner */ still comment */ clk");
        assert_eq!(
            kinds(&tokens),
            vec![VhdlToken::Signal, VhdlToken::Identifier, VhdlToken::Eof]
        );
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
