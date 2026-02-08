//! Core parser infrastructure and top-level Verilog-2005 parsing rules.
//!
//! The [`VerilogParser`] struct provides primitive operations (advance, expect, eat)
//! and error recovery, while top-level methods parse source files, modules,
//! port lists (ANSI and non-ANSI), and parameter port lists.

use crate::ast::*;
use crate::token::{Token, VerilogToken};
use aion_common::{Ident, Interner};
use aion_diagnostics::code::{Category, DiagnosticCode};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, Span};

/// A recursive descent parser for Verilog-2005 source text.
///
/// The parser consumes a token stream produced by the lexer and builds a
/// [`VerilogSourceFile`] AST. Errors are reported to the diagnostic sink and
/// represented as `Error` variants in the AST for error recovery.
pub struct VerilogParser<'src> {
    pub(crate) tokens: Vec<Token>,
    pub(crate) pos: usize,
    pub(crate) source: &'src str,
    #[allow(dead_code)]
    file: FileId,
    pub(crate) interner: &'src Interner,
    pub(crate) sink: &'src DiagnosticSink,
}

impl<'src> VerilogParser<'src> {
    /// Creates a new parser from a token stream produced by the lexer.
    ///
    /// The `tokens` must have been lexed from `source` for the given `file`.
    /// Identifiers are interned via `interner`, and parse errors are emitted to `sink`.
    pub fn new(
        tokens: Vec<Token>,
        source: &'src str,
        file: FileId,
        interner: &'src Interner,
        sink: &'src DiagnosticSink,
    ) -> Self {
        Self {
            tokens,
            pos: 0,
            source,
            file,
            interner,
            sink,
        }
    }

    // ========================================================================
    // Primitive operations
    // ========================================================================

    /// Returns the kind of the current token.
    pub(crate) fn current(&self) -> VerilogToken {
        self.tokens[self.pos].kind
    }

    /// Returns the span of the current token.
    pub(crate) fn current_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    /// Returns the source text of the current token.
    pub(crate) fn current_text(&self) -> &'src str {
        let span = self.current_span();
        &self.source[span.start as usize..span.end as usize]
    }

    /// Returns `true` if the current token matches the given kind.
    pub(crate) fn at(&self, kind: VerilogToken) -> bool {
        self.current() == kind
    }

    /// Returns `true` if the parser is at end of file.
    pub(crate) fn at_eof(&self) -> bool {
        self.current() == VerilogToken::Eof
    }

    /// Returns the span of the previous token.
    pub(crate) fn prev_span(&self) -> Span {
        if self.pos > 0 {
            self.tokens[self.pos - 1].span
        } else {
            self.current_span()
        }
    }

    /// Advances past the current token.
    pub(crate) fn advance(&mut self) {
        if !self.at_eof() {
            self.pos += 1;
        }
    }

    /// Consumes the current token if it matches the given kind. Returns `true` if consumed.
    pub(crate) fn eat(&mut self, kind: VerilogToken) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Expects the current token to match the given kind. Emits an error if not.
    pub(crate) fn expect(&mut self, kind: VerilogToken) {
        if !self.eat(kind) {
            self.expected(&format!("{kind:?}"));
        }
    }

    /// Expects and returns an identifier. Emits an error and returns a dummy if not.
    pub(crate) fn expect_ident(&mut self) -> Ident {
        if self.at(VerilogToken::Identifier) || self.at(VerilogToken::EscapedIdentifier) {
            let text = self.current_text();
            let ident = self.interner.get_or_intern(text);
            self.advance();
            ident
        } else {
            self.expected("identifier");
            self.interner.get_or_intern("<missing>")
        }
    }

    /// Returns `true` if the next token (after current) matches the given kind.
    pub(crate) fn peek_is(&self, kind: VerilogToken) -> bool {
        if self.pos + 1 < self.tokens.len() {
            self.tokens[self.pos + 1].kind == kind
        } else {
            false
        }
    }

    /// Returns the kind of the token at pos+offset.
    pub(crate) fn peek_kind(&self, offset: usize) -> VerilogToken {
        let idx = self.pos + offset;
        if idx < self.tokens.len() {
            self.tokens[idx].kind
        } else {
            VerilogToken::Eof
        }
    }

    // ========================================================================
    // Error handling and recovery
    // ========================================================================

    /// Emits an error diagnostic at the current position.
    pub(crate) fn error(&self, msg: &str) {
        self.sink.emit(Diagnostic::error(
            DiagnosticCode::new(Category::Error, 101),
            msg,
            self.current_span(),
        ));
    }

    /// Emits an "expected X" error at the current position.
    pub(crate) fn expected(&self, what: &str) {
        let actual = format!("{:?}", self.current());
        self.sink.emit(Diagnostic::error(
            DiagnosticCode::new(Category::Error, 101),
            format!("expected {what}, found {actual}"),
            self.current_span(),
        ));
    }

    /// Recovers to a semicolon, consuming everything before it (including the semicolon).
    pub(crate) fn recover_to_semicolon(&mut self) {
        while !self.at_eof() && !self.at(VerilogToken::Semicolon) {
            self.advance();
        }
        self.eat(VerilogToken::Semicolon);
    }

    // ========================================================================
    // Top-level parsing
    // ========================================================================

    /// Parses a complete Verilog source file.
    pub fn parse_source_file(&mut self) -> VerilogSourceFile {
        let start = self.current_span();
        let mut items = Vec::new();

        while !self.at_eof() {
            match self.current() {
                VerilogToken::Module => {
                    items.push(VerilogItem::Module(self.parse_module()));
                }
                _ => {
                    let span = self.current_span();
                    self.error("expected 'module'");
                    self.advance();
                    items.push(VerilogItem::Error(span));
                }
            }
        }

        let span = if items.is_empty() {
            start
        } else {
            start.merge(self.prev_span())
        };

        VerilogSourceFile { items, span }
    }

    /// Parses a module declaration.
    fn parse_module(&mut self) -> ModuleDecl {
        let start = self.current_span();
        self.expect(VerilogToken::Module);
        let name = self.expect_ident();

        // Optional parameter port list: #(...)
        let params = if self.at(VerilogToken::Hash) {
            self.parse_parameter_port_list()
        } else {
            Vec::new()
        };

        // Port list
        let (port_style, ports, port_names) = if self.at(VerilogToken::LeftParen) {
            self.parse_port_list()
        } else {
            (PortStyle::Empty, Vec::new(), Vec::new())
        };

        self.expect(VerilogToken::Semicolon);

        // Module items
        let items = self.parse_module_items();

        self.expect(VerilogToken::Endmodule);
        let span = start.merge(self.prev_span());

        ModuleDecl {
            name,
            port_style,
            params,
            ports,
            port_names,
            items,
            span,
        }
    }

    /// Parses a parameter port list: `#( param_decl {, param_decl} )`.
    fn parse_parameter_port_list(&mut self) -> Vec<ParameterDecl> {
        self.expect(VerilogToken::Hash);
        self.expect(VerilogToken::LeftParen);

        let mut params = Vec::new();
        if !self.at(VerilogToken::RightParen) {
            // Expect `parameter` keyword
            loop {
                let param = self.parse_single_parameter_decl(false);
                params.push(param);
                if !self.eat(VerilogToken::Comma) {
                    break;
                }
            }
        }

        self.expect(VerilogToken::RightParen);
        params
    }

    /// Parses a single parameter declaration.
    pub(crate) fn parse_single_parameter_decl(&mut self, local: bool) -> ParameterDecl {
        let start = self.current_span();
        let is_local = if self.eat(VerilogToken::Localparam) {
            true
        } else {
            self.eat(VerilogToken::Parameter);
            local
        };

        let signed = self.eat(VerilogToken::Signed);
        let range = if self.at(VerilogToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let name = self.expect_ident();

        let value = if self.eat(VerilogToken::Equals) {
            Some(self.parse_expr())
        } else {
            None
        };

        let span = start.merge(self.prev_span());
        ParameterDecl {
            local: is_local,
            signed,
            range,
            name,
            value,
            span,
        }
    }

    /// Parses a port list — detects ANSI vs non-ANSI style.
    ///
    /// ANSI: `(input a, output b)` — direction keyword after `(`
    /// Non-ANSI: `(a, b)` — just identifiers
    fn parse_port_list(&mut self) -> (PortStyle, Vec<PortDecl>, Vec<Ident>) {
        self.expect(VerilogToken::LeftParen);

        // Empty port list
        if self.at(VerilogToken::RightParen) {
            self.advance();
            return (PortStyle::Empty, Vec::new(), Vec::new());
        }

        // Detect ANSI vs non-ANSI: peek for direction keyword or net type
        if self.current().is_direction()
            || self.at(VerilogToken::Wire)
            || self.at(VerilogToken::Reg)
        {
            // ANSI-style port declarations
            let ports = self.parse_ansi_port_list();
            self.expect(VerilogToken::RightParen);
            (PortStyle::Ansi, ports, Vec::new())
        } else {
            // Non-ANSI: just identifier names
            let names = self.parse_port_name_list();
            self.expect(VerilogToken::RightParen);
            (PortStyle::NonAnsi, Vec::new(), names)
        }
    }

    /// Parses ANSI-style port declarations: `dir [type] [range] name {, name}`.
    fn parse_ansi_port_list(&mut self) -> Vec<PortDecl> {
        let mut ports = Vec::new();
        let mut current_dir = Direction::Input;

        loop {
            let start = self.current_span();

            // Direction (optional — inherits from previous)
            let dir = if self.at(VerilogToken::Input) {
                self.advance();
                current_dir = Direction::Input;
                Direction::Input
            } else if self.at(VerilogToken::Output) {
                self.advance();
                current_dir = Direction::Output;
                Direction::Output
            } else if self.at(VerilogToken::Inout) {
                self.advance();
                current_dir = Direction::Inout;
                Direction::Inout
            } else {
                current_dir
            };

            // Optional net type
            let net_type = self.eat_net_type();

            // Optional signed
            let signed = self.eat(VerilogToken::Signed);

            // Optional range
            let range = if self.at(VerilogToken::LeftBracket) {
                Some(self.parse_range())
            } else {
                None
            };

            // Names
            let mut names = Vec::new();
            names.push(self.expect_ident());
            // Allow comma-separated names with same type
            while self.at(VerilogToken::Comma) {
                // Peek ahead to see if next is a new direction/type or just another name
                let next = self.peek_kind(1);
                if next == VerilogToken::Input
                    || next == VerilogToken::Output
                    || next == VerilogToken::Inout
                    || next == VerilogToken::Wire
                    || next == VerilogToken::Reg
                {
                    break;
                }
                self.advance(); // eat comma
                if self.at(VerilogToken::Identifier) || self.at(VerilogToken::EscapedIdentifier) {
                    names.push(self.expect_ident());
                } else {
                    break;
                }
            }

            let span = start.merge(self.prev_span());
            ports.push(PortDecl {
                direction: dir,
                net_type,
                signed,
                range,
                names,
                span,
            });

            if !self.eat(VerilogToken::Comma) {
                break;
            }
        }

        ports
    }

    /// Parses a non-ANSI port name list: `name {, name}`.
    fn parse_port_name_list(&mut self) -> Vec<Ident> {
        let mut names = Vec::new();
        names.push(self.expect_ident());
        while self.eat(VerilogToken::Comma) {
            names.push(self.expect_ident());
        }
        names
    }

    /// Tries to consume a net type keyword, returning the type if found.
    pub(crate) fn eat_net_type(&mut self) -> Option<NetType> {
        match self.current() {
            VerilogToken::Wire => {
                self.advance();
                Some(NetType::Wire)
            }
            VerilogToken::Reg => {
                self.advance();
                Some(NetType::Reg)
            }
            VerilogToken::Integer => {
                self.advance();
                Some(NetType::Integer)
            }
            VerilogToken::Real => {
                self.advance();
                Some(NetType::Real)
            }
            VerilogToken::Tri => {
                self.advance();
                Some(NetType::Tri)
            }
            VerilogToken::Supply0 => {
                self.advance();
                Some(NetType::Supply0)
            }
            VerilogToken::Supply1 => {
                self.advance();
                Some(NetType::Supply1)
            }
            _ => None,
        }
    }

    /// Parses a range: `[ expr : expr ]`.
    pub(crate) fn parse_range(&mut self) -> Range {
        let start = self.current_span();
        self.expect(VerilogToken::LeftBracket);
        let msb = self.parse_expr();
        self.expect(VerilogToken::Colon);
        let lsb = self.parse_expr();
        self.expect(VerilogToken::RightBracket);
        let span = start.merge(self.prev_span());
        Range { msb, lsb, span }
    }

    /// Parses a comma-separated list of identifiers.
    pub(crate) fn parse_identifier_list(&mut self) -> Vec<Ident> {
        let mut names = Vec::new();
        names.push(self.expect_ident());
        while self.eat(VerilogToken::Comma) {
            names.push(self.expect_ident());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;

    fn parse_verilog(source: &str) -> (VerilogSourceFile, Vec<Diagnostic>) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = VerilogParser::new(tokens, source, file, &interner, &sink);
        let ast = parser.parse_source_file();
        (ast, sink.take_all())
    }

    fn parse_ok(source: &str) -> VerilogSourceFile {
        let (ast, errors) = parse_verilog(source);
        assert!(
            errors.is_empty(),
            "unexpected errors: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        ast
    }

    #[test]
    fn minimal_module() {
        let ast = parse_ok("module top; endmodule");
        assert_eq!(ast.items.len(), 1);
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert_eq!(m.port_style, PortStyle::Empty);
                assert!(m.ports.is_empty());
                assert!(m.items.is_empty());
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn module_empty_ports() {
        let ast = parse_ok("module top(); endmodule");
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert_eq!(m.port_style, PortStyle::Empty);
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn module_ansi_ports() {
        let ast = parse_ok(
            "module counter(
                input wire clk,
                input wire rst,
                output reg [7:0] count
            );
            endmodule",
        );
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert_eq!(m.port_style, PortStyle::Ansi);
                assert_eq!(m.ports.len(), 3);
                assert_eq!(m.ports[0].direction, Direction::Input);
                assert_eq!(m.ports[2].direction, Direction::Output);
                assert!(m.ports[2].range.is_some());
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn module_non_ansi_ports() {
        let ast = parse_ok(
            "module counter(clk, rst, count);
                input clk;
                input rst;
                output [7:0] count;
            endmodule",
        );
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert_eq!(m.port_style, PortStyle::NonAnsi);
                assert_eq!(m.port_names.len(), 3);
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn module_with_parameters() {
        let ast = parse_ok(
            "module counter #(parameter WIDTH = 8)(
                input wire clk,
                output wire [WIDTH-1:0] count
            );
            endmodule",
        );
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert_eq!(m.params.len(), 1);
                assert!(m.params[0].value.is_some());
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn module_multiple_params() {
        let ast = parse_ok(
            "module m #(parameter A = 1, parameter B = 2)(input clk);
            endmodule",
        );
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert_eq!(m.params.len(), 2);
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn two_modules() {
        let ast = parse_ok(
            "module a; endmodule
             module b; endmodule",
        );
        assert_eq!(ast.items.len(), 2);
    }

    #[test]
    fn module_direction_inheritance() {
        let ast = parse_ok(
            "module m(input a, b, output c);
            endmodule",
        );
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert_eq!(m.port_style, PortStyle::Ansi);
                // a and b share input direction
                assert_eq!(m.ports[0].direction, Direction::Input);
                assert_eq!(m.ports[0].names.len(), 2);
                assert_eq!(m.ports[1].direction, Direction::Output);
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn error_recovery_bad_top_level() {
        let (ast, errors) = parse_verilog("badtoken; module top; endmodule");
        // Should recover and parse the module
        assert!(!ast.items.is_empty());
        assert!(!errors.is_empty());
    }

    #[test]
    fn module_with_body_items() {
        let ast = parse_ok(
            "module top(input clk);
                wire [7:0] data;
                reg [7:0] q;
            endmodule",
        );
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert_eq!(m.items.len(), 2);
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn module_signed_port() {
        let ast = parse_ok(
            "module m(input signed [7:0] a, output signed [7:0] b);
            endmodule",
        );
        match &ast.items[0] {
            VerilogItem::Module(m) => {
                assert!(m.ports[0].signed);
                assert!(m.ports[1].signed);
            }
            _ => panic!("expected module"),
        }
    }
}
