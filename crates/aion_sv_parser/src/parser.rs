//! Core parser infrastructure and top-level SystemVerilog-2017 parsing rules.
//!
//! The `SvParser` struct provides primitive operations (advance, expect, eat)
//! and error recovery, while top-level methods parse source files, modules,
//! interfaces, packages, port lists, and parameter port lists.

use crate::ast::*;
use crate::token::{SvToken, Token};
use aion_common::{Ident, Interner};
use aion_diagnostics::code::{Category, DiagnosticCode};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, Span};

/// A recursive descent parser for SystemVerilog-2017 source text.
///
/// The parser consumes a token stream produced by the lexer and builds a
/// [`SvSourceFile`] AST. Errors are reported to the diagnostic sink and
/// represented as `Error` variants in the AST for error recovery.
pub struct SvParser<'src> {
    pub(crate) tokens: Vec<Token>,
    pub(crate) pos: usize,
    pub(crate) source: &'src str,
    #[allow(dead_code)]
    file: FileId,
    pub(crate) interner: &'src Interner,
    pub(crate) sink: &'src DiagnosticSink,
}

impl<'src> SvParser<'src> {
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
    pub(crate) fn current(&self) -> SvToken {
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
    pub(crate) fn at(&self, kind: SvToken) -> bool {
        self.current() == kind
    }

    /// Returns `true` if the parser is at end of file.
    pub(crate) fn at_eof(&self) -> bool {
        self.current() == SvToken::Eof
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
    pub(crate) fn eat(&mut self, kind: SvToken) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Expects the current token to match the given kind. Emits an error if not.
    pub(crate) fn expect(&mut self, kind: SvToken) {
        if !self.eat(kind) {
            self.expected(&format!("{kind:?}"));
        }
    }

    /// Expects and returns an identifier. Emits an error and returns a dummy if not.
    pub(crate) fn expect_ident(&mut self) -> Ident {
        if self.at(SvToken::Identifier) || self.at(SvToken::EscapedIdentifier) {
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
    pub(crate) fn peek_is(&self, kind: SvToken) -> bool {
        if self.pos + 1 < self.tokens.len() {
            self.tokens[self.pos + 1].kind == kind
        } else {
            false
        }
    }

    /// Returns the kind of the token at pos+offset.
    pub(crate) fn peek_kind(&self, offset: usize) -> SvToken {
        let idx = self.pos + offset;
        if idx < self.tokens.len() {
            self.tokens[idx].kind
        } else {
            SvToken::Eof
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
        while !self.at_eof() && !self.at(SvToken::Semicolon) {
            self.advance();
        }
        self.eat(SvToken::Semicolon);
    }

    // ========================================================================
    // Top-level parsing
    // ========================================================================

    /// Parses a complete SystemVerilog source file.
    pub fn parse_source_file(&mut self) -> SvSourceFile {
        let start = self.current_span();
        let mut items = Vec::new();

        while !self.at_eof() {
            match self.current() {
                SvToken::Module => {
                    items.push(SvItem::Module(self.parse_module()));
                }
                SvToken::Interface => {
                    items.push(SvItem::Interface(self.parse_interface()));
                }
                SvToken::Package => {
                    items.push(SvItem::Package(self.parse_package()));
                }
                _ => {
                    let span = self.current_span();
                    self.error("expected 'module', 'interface', or 'package'");
                    self.advance();
                    items.push(SvItem::Error(span));
                }
            }
        }

        let span = if items.is_empty() {
            start
        } else {
            start.merge(self.prev_span())
        };

        SvSourceFile { items, span }
    }

    /// Parses a module declaration.
    fn parse_module(&mut self) -> SvModuleDecl {
        let start = self.current_span();
        self.expect(SvToken::Module);
        let name = self.expect_ident();

        // Optional parameter port list: #(...)
        let params = if self.at(SvToken::Hash) {
            self.parse_parameter_port_list()
        } else {
            Vec::new()
        };

        // Port list
        let (port_style, ports, port_names) = if self.at(SvToken::LeftParen) {
            self.parse_port_list()
        } else {
            (PortStyle::Empty, Vec::new(), Vec::new())
        };

        self.expect(SvToken::Semicolon);

        // Module items
        let items = self.parse_module_items();

        self.expect(SvToken::Endmodule);
        let end_label = self.parse_end_label();
        let span = start.merge(self.prev_span());

        SvModuleDecl {
            name,
            port_style,
            params,
            ports,
            port_names,
            items,
            end_label,
            span,
        }
    }

    /// Parses an interface declaration.
    fn parse_interface(&mut self) -> SvInterfaceDecl {
        let start = self.current_span();
        self.expect(SvToken::Interface);
        let name = self.expect_ident();

        let params = if self.at(SvToken::Hash) {
            self.parse_parameter_port_list()
        } else {
            Vec::new()
        };

        let (port_style, ports, _port_names) = if self.at(SvToken::LeftParen) {
            self.parse_port_list()
        } else {
            (PortStyle::Empty, Vec::new(), Vec::new())
        };

        self.expect(SvToken::Semicolon);

        let items = self.parse_interface_items();

        self.expect(SvToken::Endinterface);
        let end_label = self.parse_end_label();
        let span = start.merge(self.prev_span());

        SvInterfaceDecl {
            name,
            params,
            ports,
            port_style,
            items,
            end_label,
            span,
        }
    }

    /// Parses a package declaration.
    fn parse_package(&mut self) -> SvPackageDecl {
        let start = self.current_span();
        self.expect(SvToken::Package);
        let name = self.expect_ident();
        self.expect(SvToken::Semicolon);

        let items = self.parse_package_items();

        self.expect(SvToken::Endpackage);
        let end_label = self.parse_end_label();
        let span = start.merge(self.prev_span());

        SvPackageDecl {
            name,
            items,
            end_label,
            span,
        }
    }

    /// Parses an optional end label (e.g., `: name` after `endmodule`).
    pub(crate) fn parse_end_label(&mut self) -> Option<Ident> {
        if self.eat(SvToken::Colon) {
            Some(self.expect_ident())
        } else {
            None
        }
    }

    /// Parses interface items until `endinterface` or EOF.
    fn parse_interface_items(&mut self) -> Vec<ModuleItem> {
        let mut items = Vec::new();
        while !self.at(SvToken::Endinterface) && !self.at_eof() {
            if let Some(item) = self.parse_module_item_inner() {
                items.push(item);
            }
        }
        items
    }

    /// Parses package items until `endpackage` or EOF.
    fn parse_package_items(&mut self) -> Vec<ModuleItem> {
        let mut items = Vec::new();
        while !self.at(SvToken::Endpackage) && !self.at_eof() {
            if let Some(item) = self.parse_package_item_inner() {
                items.push(item);
            }
        }
        items
    }

    /// Parses a single package item (subset of module items).
    fn parse_package_item_inner(&mut self) -> Option<ModuleItem> {
        match self.current() {
            SvToken::Parameter => Some(self.parse_parameter_item(false)),
            SvToken::Localparam => Some(self.parse_parameter_item(true)),
            SvToken::Typedef => Some(self.parse_typedef_item()),
            SvToken::Function => Some(self.parse_function_declaration()),
            SvToken::Task => Some(self.parse_task_declaration()),
            SvToken::Import => Some(self.parse_import_item()),
            SvToken::Logic
            | SvToken::Bit
            | SvToken::Byte
            | SvToken::Shortint
            | SvToken::Int
            | SvToken::Longint => Some(self.parse_var_declaration()),
            SvToken::Enum => Some(self.parse_typedef_or_enum_var()),
            _ => {
                let span = self.current_span();
                self.error("expected package item");
                self.recover_to_semicolon();
                Some(ModuleItem::Error(span))
            }
        }
    }

    // ========================================================================
    // Parameter port list
    // ========================================================================

    /// Parses a parameter port list: `#( param_decl {, param_decl} )`.
    fn parse_parameter_port_list(&mut self) -> Vec<ParameterDecl> {
        self.expect(SvToken::Hash);
        self.expect(SvToken::LeftParen);

        let mut params = Vec::new();
        if !self.at(SvToken::RightParen) {
            loop {
                let param = self.parse_single_parameter_decl(false);
                params.push(param);
                if !self.eat(SvToken::Comma) {
                    break;
                }
            }
        }

        self.expect(SvToken::RightParen);
        params
    }

    /// Parses a single parameter declaration.
    pub(crate) fn parse_single_parameter_decl(&mut self, local: bool) -> ParameterDecl {
        let start = self.current_span();
        let is_local = if self.eat(SvToken::Localparam) {
            true
        } else {
            self.eat(SvToken::Parameter);
            local
        };

        // Optional type spec (int, logic, etc.)
        let type_spec = self.try_parse_simple_type_spec();

        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let name = self.expect_ident();

        let value = if self.eat(SvToken::Equals) {
            Some(self.parse_expr())
        } else {
            None
        };

        let span = start.merge(self.prev_span());
        ParameterDecl {
            local: is_local,
            signed,
            type_spec,
            range,
            name,
            value,
            span,
        }
    }

    /// Tries to parse a simple type spec (logic, bit, int, etc.) without consuming if not found.
    pub(crate) fn try_parse_simple_type_spec(&mut self) -> Option<TypeSpec> {
        match self.current() {
            SvToken::Logic => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Logic))
            }
            SvToken::Bit => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Bit))
            }
            SvToken::Byte => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Byte))
            }
            SvToken::Shortint => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Shortint))
            }
            SvToken::Int => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Int))
            }
            SvToken::Longint => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Longint))
            }
            SvToken::Integer => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Integer))
            }
            SvToken::Real => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Real))
            }
            SvToken::Reg => {
                self.advance();
                Some(TypeSpec::Simple(VarType::Reg))
            }
            _ => None,
        }
    }

    // ========================================================================
    // Port list
    // ========================================================================

    /// Parses a port list — detects ANSI vs non-ANSI style.
    fn parse_port_list(&mut self) -> (PortStyle, Vec<SvPortDecl>, Vec<Ident>) {
        self.expect(SvToken::LeftParen);

        // Empty port list
        if self.at(SvToken::RightParen) {
            self.advance();
            return (PortStyle::Empty, Vec::new(), Vec::new());
        }

        // Detect ANSI vs non-ANSI: peek for direction keyword, net/var type,
        // or interface port (ident.modport)
        let is_ansi = self.current().is_direction()
            || self.at(SvToken::Wire)
            || self.at(SvToken::Reg)
            || self.at(SvToken::Logic)
            || self.at(SvToken::Bit)
            || self.at(SvToken::Int)
            // Interface port: ident.modport ident
            || (self.at(SvToken::Identifier) && self.peek_is(SvToken::Dot));

        if is_ansi {
            let ports = self.parse_ansi_port_list();
            self.expect(SvToken::RightParen);
            (PortStyle::Ansi, ports, Vec::new())
        } else {
            let names = self.parse_port_name_list();
            self.expect(SvToken::RightParen);
            (PortStyle::NonAnsi, Vec::new(), names)
        }
    }

    /// Parses ANSI-style port declarations.
    fn parse_ansi_port_list(&mut self) -> Vec<SvPortDecl> {
        let mut ports = Vec::new();
        let mut current_dir = Direction::Input;

        loop {
            let start = self.current_span();

            // Check for interface port: ident.modport name
            if self.at(SvToken::Identifier) && self.peek_is(SvToken::Dot) {
                let interface_name = self.expect_ident();
                self.advance(); // eat .
                let modport = self.expect_ident();
                let name = self.expect_ident();

                let span = start.merge(self.prev_span());
                ports.push(SvPortDecl {
                    direction: Direction::Inout, // interface ports are bidirectional
                    port_type: SvPortType::InterfacePort {
                        interface_name,
                        modport: Some(modport),
                    },
                    signed: false,
                    range: None,
                    names: vec![name],
                    span,
                });

                if !self.eat(SvToken::Comma) {
                    break;
                }
                continue;
            }

            // Direction (optional — inherits from previous)
            let dir = if self.at(SvToken::Input) {
                self.advance();
                current_dir = Direction::Input;
                Direction::Input
            } else if self.at(SvToken::Output) {
                self.advance();
                current_dir = Direction::Output;
                Direction::Output
            } else if self.at(SvToken::Inout) {
                self.advance();
                current_dir = Direction::Inout;
                Direction::Inout
            } else {
                current_dir
            };

            // Port type
            let port_type = self.eat_port_type();

            // Optional signed
            let signed = self.eat(SvToken::Signed);

            // Optional range
            let range = if self.at(SvToken::LeftBracket) {
                Some(self.parse_range())
            } else {
                None
            };

            // Names
            let mut names = Vec::new();
            names.push(self.expect_ident());
            while self.at(SvToken::Comma) {
                let next = self.peek_kind(1);
                if next == SvToken::Input
                    || next == SvToken::Output
                    || next == SvToken::Inout
                    || next == SvToken::Wire
                    || next == SvToken::Reg
                    || next == SvToken::Logic
                    || next == SvToken::Bit
                    || next == SvToken::Int
                {
                    break;
                }
                // Check if next is an interface port (ident.modport)
                if next == SvToken::Identifier && self.peek_kind(2) == SvToken::Dot {
                    break;
                }
                self.advance(); // eat comma
                if self.at(SvToken::Identifier) || self.at(SvToken::EscapedIdentifier) {
                    names.push(self.expect_ident());
                } else {
                    break;
                }
            }

            let span = start.merge(self.prev_span());
            ports.push(SvPortDecl {
                direction: dir,
                port_type,
                signed,
                range,
                names,
                span,
            });

            if !self.eat(SvToken::Comma) {
                break;
            }
        }

        ports
    }

    /// Tries to consume a port type (net or variable type).
    pub(crate) fn eat_port_type(&mut self) -> SvPortType {
        match self.current() {
            SvToken::Wire => {
                self.advance();
                SvPortType::Net(NetType::Wire)
            }
            SvToken::Tri => {
                self.advance();
                SvPortType::Net(NetType::Tri)
            }
            SvToken::Supply0 => {
                self.advance();
                SvPortType::Net(NetType::Supply0)
            }
            SvToken::Supply1 => {
                self.advance();
                SvPortType::Net(NetType::Supply1)
            }
            SvToken::Logic => {
                self.advance();
                SvPortType::Var(VarType::Logic)
            }
            SvToken::Reg => {
                self.advance();
                SvPortType::Var(VarType::Reg)
            }
            SvToken::Bit => {
                self.advance();
                SvPortType::Var(VarType::Bit)
            }
            SvToken::Byte => {
                self.advance();
                SvPortType::Var(VarType::Byte)
            }
            SvToken::Shortint => {
                self.advance();
                SvPortType::Var(VarType::Shortint)
            }
            SvToken::Int => {
                self.advance();
                SvPortType::Var(VarType::Int)
            }
            SvToken::Longint => {
                self.advance();
                SvPortType::Var(VarType::Longint)
            }
            SvToken::Integer => {
                self.advance();
                SvPortType::Var(VarType::Integer)
            }
            SvToken::Real => {
                self.advance();
                SvPortType::Var(VarType::Real)
            }
            _ => SvPortType::Implicit,
        }
    }

    /// Parses a non-ANSI port name list.
    fn parse_port_name_list(&mut self) -> Vec<Ident> {
        let mut names = Vec::new();
        names.push(self.expect_ident());
        while self.eat(SvToken::Comma) {
            names.push(self.expect_ident());
        }
        names
    }

    /// Tries to consume a net type keyword, returning the type if found.
    pub(crate) fn eat_net_type(&mut self) -> Option<NetType> {
        match self.current() {
            SvToken::Wire => {
                self.advance();
                Some(NetType::Wire)
            }
            SvToken::Tri => {
                self.advance();
                Some(NetType::Tri)
            }
            SvToken::Supply0 => {
                self.advance();
                Some(NetType::Supply0)
            }
            SvToken::Supply1 => {
                self.advance();
                Some(NetType::Supply1)
            }
            _ => None,
        }
    }

    /// Parses a range: `[ expr : expr ]`.
    pub(crate) fn parse_range(&mut self) -> Range {
        let start = self.current_span();
        self.expect(SvToken::LeftBracket);
        let msb = self.parse_expr();
        self.expect(SvToken::Colon);
        let lsb = self.parse_expr();
        self.expect(SvToken::RightBracket);
        let span = start.merge(self.prev_span());
        Range { msb, lsb, span }
    }

    /// Parses a comma-separated list of identifiers.
    pub(crate) fn parse_identifier_list(&mut self) -> Vec<Ident> {
        let mut names = Vec::new();
        names.push(self.expect_ident());
        while self.eat(SvToken::Comma) {
            names.push(self.expect_ident());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;

    fn parse_sv(source: &str) -> (SvSourceFile, Vec<Diagnostic>) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = SvParser::new(tokens, source, file, &interner, &sink);
        let ast = parser.parse_source_file();
        (ast, sink.take_all())
    }

    fn parse_ok(source: &str) -> SvSourceFile {
        let (ast, errors) = parse_sv(source);
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
            SvItem::Module(m) => {
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
            SvItem::Module(m) => {
                assert_eq!(m.port_style, PortStyle::Empty);
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn module_ansi_ports_logic() {
        let ast = parse_ok(
            "module counter(
                input logic clk,
                input logic rst,
                output logic [7:0] count
            );
            endmodule",
        );
        match &ast.items[0] {
            SvItem::Module(m) => {
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
    fn module_with_end_label() {
        let ast = parse_ok("module top; endmodule : top");
        match &ast.items[0] {
            SvItem::Module(m) => {
                assert!(m.end_label.is_some());
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn module_with_parameters() {
        let ast = parse_ok(
            "module counter #(parameter int WIDTH = 8)(
                input logic clk,
                output logic [WIDTH-1:0] count
            );
            endmodule",
        );
        match &ast.items[0] {
            SvItem::Module(m) => {
                assert_eq!(m.params.len(), 1);
                assert!(m.params[0].value.is_some());
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
            "module m(input logic a, b, output logic c);
            endmodule",
        );
        match &ast.items[0] {
            SvItem::Module(m) => {
                assert_eq!(m.port_style, PortStyle::Ansi);
                assert_eq!(m.ports[0].direction, Direction::Input);
                assert_eq!(m.ports[0].names.len(), 2);
                assert_eq!(m.ports[1].direction, Direction::Output);
            }
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn interface_declaration() {
        let ast = parse_ok(
            "interface axi_if;
                logic valid;
                logic ready;
                modport master(output valid, input ready);
                modport slave(input valid, output ready);
            endinterface",
        );
        assert_eq!(ast.items.len(), 1);
        match &ast.items[0] {
            SvItem::Interface(i) => {
                assert!(i.items.len() >= 2);
            }
            _ => panic!("expected interface"),
        }
    }

    #[test]
    fn interface_with_end_label() {
        let ast = parse_ok("interface axi_if; endinterface : axi_if");
        match &ast.items[0] {
            SvItem::Interface(i) => {
                assert!(i.end_label.is_some());
            }
            _ => panic!("expected interface"),
        }
    }

    #[test]
    fn package_declaration() {
        let ast = parse_ok(
            "package my_pkg;
                parameter int WIDTH = 8;
                typedef logic [WIDTH-1:0] data_t;
            endpackage",
        );
        assert_eq!(ast.items.len(), 1);
        match &ast.items[0] {
            SvItem::Package(p) => {
                assert_eq!(p.items.len(), 2);
            }
            _ => panic!("expected package"),
        }
    }

    #[test]
    fn package_with_end_label() {
        let ast = parse_ok("package my_pkg; endpackage : my_pkg");
        match &ast.items[0] {
            SvItem::Package(p) => {
                assert!(p.end_label.is_some());
            }
            _ => panic!("expected package"),
        }
    }

    #[test]
    fn error_recovery_bad_top_level() {
        let (ast, errors) = parse_sv("badtoken; module top; endmodule");
        assert!(!ast.items.is_empty());
        assert!(!errors.is_empty());
    }

    #[test]
    fn module_signed_port() {
        let ast = parse_ok(
            "module m(input logic signed [7:0] a, output logic signed [7:0] b);
            endmodule",
        );
        match &ast.items[0] {
            SvItem::Module(m) => {
                assert!(m.ports[0].signed);
                assert!(m.ports[1].signed);
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
            SvItem::Module(m) => {
                assert_eq!(m.port_style, PortStyle::NonAnsi);
                assert_eq!(m.port_names.len(), 3);
            }
            _ => panic!("expected module"),
        }
    }
}
