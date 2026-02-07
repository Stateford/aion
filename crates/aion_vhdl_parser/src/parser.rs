//! Core parser infrastructure and top-level VHDL-2008 parsing rules.
//!
//! The [`VhdlParser`] struct provides primitive operations (advance, expect, eat)
//! and error recovery, while top-level methods parse design files, entities,
//! architectures, and packages.

use crate::ast::*;
use crate::token::{Token, VhdlToken};
use aion_common::{Ident, Interner};
use aion_diagnostics::code::{Category, DiagnosticCode};
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::{FileId, Span};

/// A recursive descent parser for VHDL-2008 source text.
///
/// The parser consumes a token stream produced by the lexer and builds an
/// [`VhdlDesignFile`] AST. Errors are reported to the diagnostic sink and
/// represented as `Error` variants in the AST for error recovery.
pub struct VhdlParser<'src> {
    pub(crate) tokens: Vec<Token>,
    pub(crate) pos: usize,
    source: &'src str,
    #[allow(dead_code)]
    file: FileId,
    pub(crate) interner: &'src Interner,
    sink: &'src DiagnosticSink,
}

impl<'src> VhdlParser<'src> {
    /// Creates a new parser from a token stream.
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
    pub(crate) fn current(&self) -> VhdlToken {
        self.tokens[self.pos].kind
    }

    /// Returns the current token.
    #[allow(dead_code)]
    pub(crate) fn current_token(&self) -> Token {
        self.tokens[self.pos]
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
    pub(crate) fn at(&self, kind: VhdlToken) -> bool {
        self.current() == kind
    }

    /// Returns `true` if the parser is at end of file.
    pub(crate) fn at_eof(&self) -> bool {
        self.current() == VhdlToken::Eof
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
    pub(crate) fn eat(&mut self, kind: VhdlToken) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Expects the current token to match the given kind. Emits an error if not.
    pub(crate) fn expect(&mut self, kind: VhdlToken) {
        if !self.eat(kind) {
            self.expected(&format!("{kind:?}"));
        }
    }

    /// Expects and returns an identifier. Emits an error and returns a dummy if not.
    pub(crate) fn expect_ident(&mut self) -> Ident {
        if self.at(VhdlToken::Identifier) || self.at(VhdlToken::ExtendedIdentifier) {
            let text = self.current_text();
            let ident = self.interner.get_or_intern(text);
            self.advance();
            ident
        } else {
            self.expected("identifier");
            self.interner.get_or_intern("<missing>")
        }
    }

    /// Expects an identifier or keyword (for attribute names, entity classes, etc.).
    pub(crate) fn expect_ident_or_keyword(&mut self) -> Ident {
        if self.at(VhdlToken::Identifier) || self.at(VhdlToken::ExtendedIdentifier) {
            return self.expect_ident();
        }
        // Allow keywords to be used as identifiers in certain contexts
        if self.current().is_keyword() {
            let text = self.current_text();
            let ident = self.interner.get_or_intern(text);
            self.advance();
            return ident;
        }
        self.expected("identifier or keyword");
        self.interner.get_or_intern("<missing>")
    }

    /// Tries to consume an identifier. Returns `Some(Ident)` if present, `None` otherwise.
    pub(crate) fn eat_ident(&mut self) -> Option<Ident> {
        if self.at(VhdlToken::Identifier) || self.at(VhdlToken::ExtendedIdentifier) {
            Some(self.expect_ident())
        } else {
            None
        }
    }

    /// Returns `true` if the next token (after current) matches the given kind.
    pub(crate) fn peek_is(&self, kind: VhdlToken) -> bool {
        if self.pos + 1 < self.tokens.len() {
            self.tokens[self.pos + 1].kind == kind
        } else {
            false
        }
    }

    /// Returns `true` if the next token is an identifier or keyword.
    pub(crate) fn peek_is_ident_or_keyword(&self) -> bool {
        if self.pos + 1 < self.tokens.len() {
            let kind = self.tokens[self.pos + 1].kind;
            kind == VhdlToken::Identifier
                || kind == VhdlToken::ExtendedIdentifier
                || kind.is_keyword()
        } else {
            false
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
        while !self.at_eof() && !self.at(VhdlToken::Semicolon) {
            self.advance();
        }
        self.eat(VhdlToken::Semicolon);
    }

    /// Recovers to a design unit boundary (entity, architecture, package, or EOF).
    #[allow(dead_code)]
    fn recover_to_design_unit(&mut self) {
        while !self.at_eof() && !self.at_design_unit_start() {
            self.advance();
        }
    }

    /// Returns `true` if the current token could start a design unit.
    fn at_design_unit_start(&self) -> bool {
        matches!(
            self.current(),
            VhdlToken::Entity
                | VhdlToken::Architecture
                | VhdlToken::Package
                | VhdlToken::Library
                | VhdlToken::Use
                | VhdlToken::Context
                | VhdlToken::Configuration
        )
    }

    // ========================================================================
    // Top-level parsing
    // ========================================================================

    /// Parses a complete VHDL design file.
    pub fn parse_design_file(&mut self) -> VhdlDesignFile {
        let start = self.current_span();
        let mut units = Vec::new();

        while !self.at_eof() {
            match self.parse_design_unit() {
                Some(unit) => units.push(unit),
                None => {
                    // Error recovery: skip to next design unit
                    if !self.at_eof() {
                        let span = self.current_span();
                        self.error("unexpected token at top level");
                        self.advance();
                        units.push(DesignUnit::Error(span));
                    }
                }
            }
        }

        let span = if units.is_empty() {
            start
        } else {
            start.merge(self.prev_span())
        };

        VhdlDesignFile { units, span }
    }

    /// Parses a single design unit with its context items.
    fn parse_design_unit(&mut self) -> Option<DesignUnit> {
        let start = self.current_span();

        // Parse context items (library, use)
        let context = self.parse_context_items();

        // Parse the design unit kind
        let unit = match self.current() {
            VhdlToken::Entity => {
                let entity = self.parse_entity_declaration();
                Some(DesignUnitKind::Entity(entity))
            }
            VhdlToken::Architecture => {
                let arch = self.parse_architecture_declaration();
                Some(DesignUnitKind::Architecture(arch))
            }
            VhdlToken::Package => {
                if self.peek_is(VhdlToken::Body) {
                    let pkg_body = self.parse_package_body_declaration();
                    Some(DesignUnitKind::PackageBody(pkg_body))
                } else {
                    let pkg = self.parse_package_declaration();
                    Some(DesignUnitKind::Package(pkg))
                }
            }
            _ => {
                if !context.is_empty() {
                    // We have context but no unit — error
                    self.error("expected design unit after context clauses");
                    return Some(DesignUnit::Error(start.merge(self.prev_span())));
                }
                return None;
            }
        };

        unit.map(|u| {
            let span = start.merge(self.prev_span());
            DesignUnit::ContextUnit {
                context,
                unit: u,
                span,
            }
        })
    }

    /// Parses context items (library and use clauses).
    fn parse_context_items(&mut self) -> Vec<ContextItem> {
        let mut items = Vec::new();
        loop {
            match self.current() {
                VhdlToken::Library => items.push(self.parse_library_clause()),
                VhdlToken::Use => items.push(self.parse_use_clause()),
                _ => break,
            }
        }
        items
    }

    /// Parses a library clause: `library name {, name};`.
    fn parse_library_clause(&mut self) -> ContextItem {
        let start = self.current_span();
        self.expect(VhdlToken::Library);

        let mut names = Vec::new();
        names.push(self.expect_ident());
        while self.eat(VhdlToken::Comma) {
            names.push(self.expect_ident());
        }

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());
        ContextItem::Library { names, span }
    }

    /// Parses a use clause: `use selected_name;`.
    fn parse_use_clause(&mut self) -> ContextItem {
        let start = self.current_span();
        self.expect(VhdlToken::Use);
        let name = self.parse_selected_name();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());
        ContextItem::Use { name, span }
    }

    /// Parses a dotted selected name (e.g., `ieee.std_logic_1164.all`).
    pub fn parse_selected_name(&mut self) -> SelectedName {
        let start = self.current_span();
        let mut parts = Vec::new();

        // First part: identifier or keyword
        if self.at(VhdlToken::All) {
            parts.push(self.interner.get_or_intern("all"));
            self.advance();
        } else {
            parts.push(self.expect_ident());
        }

        while self.eat(VhdlToken::Dot) {
            if self.at(VhdlToken::All) {
                parts.push(self.interner.get_or_intern("all"));
                self.advance();
                break;
            }
            parts.push(self.expect_ident());
        }

        let span = start.merge(self.prev_span());
        SelectedName { parts, span }
    }

    // ========================================================================
    // Entity, Architecture, Package
    // ========================================================================

    /// Parses an entity declaration.
    fn parse_entity_declaration(&mut self) -> EntityDecl {
        let start = self.current_span();
        self.expect(VhdlToken::Entity);
        let name = self.expect_ident();
        self.expect(VhdlToken::Is);

        let generics = if self.at(VhdlToken::Generic) {
            Some(self.parse_generic_clause())
        } else {
            None
        };

        let ports = if self.at(VhdlToken::Port) {
            Some(self.parse_port_clause())
        } else {
            None
        };

        // Parse optional entity declarative items and statements
        let mut decls = Vec::new();
        let mut stmts = Vec::new();

        if self.at(VhdlToken::Begin) {
            self.advance();
            stmts = self.parse_concurrent_statements();
        } else if !self.at(VhdlToken::End) {
            decls = self.parse_declarations();
            if self.eat(VhdlToken::Begin) {
                stmts = self.parse_concurrent_statements();
            }
        }

        self.expect(VhdlToken::End);
        self.eat(VhdlToken::Entity);
        self.eat_ident(); // optional name
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        EntityDecl {
            name,
            generics,
            ports,
            decls,
            stmts,
            span,
        }
    }

    /// Parses an architecture declaration.
    fn parse_architecture_declaration(&mut self) -> ArchitectureDecl {
        let start = self.current_span();
        self.expect(VhdlToken::Architecture);
        let name = self.expect_ident();
        self.expect(VhdlToken::Of);
        let entity_name = self.expect_ident();
        self.expect(VhdlToken::Is);

        let decls = self.parse_declarations();
        self.expect(VhdlToken::Begin);
        let stmts = self.parse_concurrent_statements();

        self.expect(VhdlToken::End);
        self.eat(VhdlToken::Architecture);
        self.eat_ident(); // optional name
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        ArchitectureDecl {
            name,
            entity_name,
            decls,
            stmts,
            span,
        }
    }

    /// Parses a package declaration.
    fn parse_package_declaration(&mut self) -> PackageDecl {
        let start = self.current_span();
        self.expect(VhdlToken::Package);
        let name = self.expect_ident();
        self.expect(VhdlToken::Is);

        let decls = self.parse_declarations();

        self.expect(VhdlToken::End);
        self.eat(VhdlToken::Package);
        self.eat_ident();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        PackageDecl { name, decls, span }
    }

    /// Parses a package body declaration.
    fn parse_package_body_declaration(&mut self) -> PackageBodyDecl {
        let start = self.current_span();
        self.expect(VhdlToken::Package);
        self.expect(VhdlToken::Body);
        let name = self.expect_ident();
        self.expect(VhdlToken::Is);

        let decls = self.parse_declarations();

        self.expect(VhdlToken::End);
        self.eat(VhdlToken::Package);
        self.eat(VhdlToken::Body);
        self.eat_ident();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        PackageBodyDecl { name, decls, span }
    }

    // ========================================================================
    // Interface declarations (generics, ports)
    // ========================================================================

    /// Parses a generic clause: `generic ( interface_list );`.
    pub fn parse_generic_clause(&mut self) -> GenericClause {
        let start = self.current_span();
        self.expect(VhdlToken::Generic);
        let decls = self.parse_interface_list();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());
        GenericClause { decls, span }
    }

    /// Parses a port clause: `port ( interface_list );`.
    pub fn parse_port_clause(&mut self) -> PortClause {
        let start = self.current_span();
        self.expect(VhdlToken::Port);
        let decls = self.parse_interface_list();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());
        PortClause { decls, span }
    }

    /// Parses an interface list: `( decl {; decl} )`.
    pub fn parse_interface_list(&mut self) -> Vec<InterfaceDecl> {
        self.expect(VhdlToken::LeftParen);
        let mut decls = Vec::new();

        loop {
            decls.push(self.parse_interface_decl());
            if !self.eat(VhdlToken::Semicolon) {
                break;
            }
            // Check if next token starts another decl or closes the paren
            if self.at(VhdlToken::RightParen) {
                break;
            }
        }

        self.expect(VhdlToken::RightParen);
        decls
    }

    /// Parses a single interface declaration.
    fn parse_interface_decl(&mut self) -> InterfaceDecl {
        let start = self.current_span();

        // Optional object class keywords (signal, variable, constant)
        self.eat(VhdlToken::Signal);
        self.eat(VhdlToken::Variable);
        self.eat(VhdlToken::Constant);

        let names = self.parse_identifier_list();
        self.expect(VhdlToken::Colon);

        // Optional mode
        let mode = match self.current() {
            VhdlToken::In => {
                self.advance();
                Some(PortMode::In)
            }
            VhdlToken::Out => {
                self.advance();
                Some(PortMode::Out)
            }
            VhdlToken::Inout => {
                self.advance();
                Some(PortMode::Inout)
            }
            VhdlToken::Buffer => {
                self.advance();
                Some(PortMode::Buffer)
            }
            VhdlToken::Linkage => {
                self.advance();
                Some(PortMode::Linkage)
            }
            _ => None,
        };

        let ty = self.parse_type_indication();

        let default = if self.eat(VhdlToken::ColonEquals) {
            Some(self.parse_expr())
        } else {
            None
        };

        let span = start.merge(self.prev_span());

        InterfaceDecl {
            names,
            mode,
            ty,
            default,
            span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;

    fn parse_vhdl(source: &str) -> (VhdlDesignFile, Vec<Diagnostic>) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = VhdlParser::new(tokens, source, file, &interner, &sink);
        let ast = parser.parse_design_file();
        (ast, sink.take_all())
    }

    fn parse_ok(source: &str) -> VhdlDesignFile {
        let (ast, errors) = parse_vhdl(source);
        assert!(
            errors.is_empty(),
            "unexpected errors: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        ast
    }

    // ========================================================================
    // Entity tests
    // ========================================================================

    #[test]
    fn minimal_entity() {
        let ast = parse_ok("entity top is end entity top;");
        assert_eq!(ast.units.len(), 1);
        match &ast.units[0] {
            DesignUnit::ContextUnit { unit, .. } => match unit {
                DesignUnitKind::Entity(e) => {
                    assert!(e.generics.is_none());
                    assert!(e.ports.is_none());
                    assert!(e.generics.is_none());
                }
                _ => panic!("expected entity"),
            },
            _ => panic!("expected context unit"),
        }
    }

    #[test]
    fn entity_with_ports() {
        let ast = parse_ok(
            "entity counter is
                port (
                    clk : in std_logic;
                    rst : in std_logic;
                    count : out std_logic_vector(7 downto 0)
                );
            end entity counter;",
        );
        assert_eq!(ast.units.len(), 1);
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Entity(e),
            ..
        } = &ast.units[0]
        {
            let ports = e.ports.as_ref().unwrap();
            assert_eq!(ports.decls.len(), 3);
            assert_eq!(ports.decls[0].mode, Some(PortMode::In));
            assert_eq!(ports.decls[2].mode, Some(PortMode::Out));
        } else {
            panic!("expected entity");
        }
    }

    #[test]
    fn entity_with_generics() {
        let ast = parse_ok(
            "entity counter is
                generic (
                    WIDTH : integer := 8
                );
                port (
                    clk : in std_logic
                );
            end entity counter;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Entity(e),
            ..
        } = &ast.units[0]
        {
            assert!(e.generics.is_some());
            assert!(e.ports.is_some());
            let generics = e.generics.as_ref().unwrap();
            assert_eq!(generics.decls.len(), 1);
            assert!(generics.decls[0].default.is_some());
        } else {
            panic!("expected entity");
        }
    }

    #[test]
    fn entity_minimal_no_end_keyword() {
        let ast = parse_ok("entity top is end top;");
        assert_eq!(ast.units.len(), 1);
    }

    // ========================================================================
    // Architecture tests
    // ========================================================================

    #[test]
    fn minimal_architecture() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert!(a.decls.is_empty());
            assert!(a.stmts.is_empty());
        } else {
            panic!("expected architecture");
        }
    }

    #[test]
    fn architecture_with_signals() {
        let ast = parse_ok(
            "architecture rtl of top is
                signal a, b : std_logic;
                signal c : std_logic_vector(7 downto 0);
            begin
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert_eq!(a.decls.len(), 2);
        } else {
            panic!("expected architecture");
        }
    }

    #[test]
    fn architecture_with_process() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process(clk)
                begin
                    if clk'event and clk = '1' then
                        q <= d;
                    end if;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert_eq!(a.stmts.len(), 1);
            match &a.stmts[0] {
                ConcurrentStatement::Process(p) => {
                    assert!(matches!(p.sensitivity, SensitivityList::List(_)));
                }
                _ => panic!("expected process"),
            }
        } else {
            panic!("expected architecture");
        }
    }

    #[test]
    fn architecture_with_instantiation() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                u1 : entity work.counter
                    port map (
                        clk => clk,
                        rst => rst,
                        count => count_out
                    );
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert_eq!(a.stmts.len(), 1);
            match &a.stmts[0] {
                ConcurrentStatement::ComponentInstantiation(c) => {
                    assert!(c.port_map.is_some());
                    let pm = c.port_map.as_ref().unwrap();
                    assert_eq!(pm.elements.len(), 3);
                }
                _ => panic!("expected component instantiation"),
            }
        } else {
            panic!("expected architecture");
        }
    }

    // ========================================================================
    // Package tests
    // ========================================================================

    #[test]
    fn package_with_types() {
        let ast = parse_ok(
            "package my_pkg is
                type state_t is (idle, running, stopped);
                constant MAX_VAL : integer := 255;
            end package my_pkg;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        {
            assert_eq!(p.decls.len(), 2);
        } else {
            panic!("expected package");
        }
    }

    #[test]
    fn package_body() {
        let ast = parse_ok(
            "package body my_pkg is
                constant HIDDEN : integer := 42;
            end package body my_pkg;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::PackageBody(pb),
            ..
        } = &ast.units[0]
        {
            assert_eq!(pb.decls.len(), 1);
        } else {
            panic!("expected package body");
        }
    }

    // ========================================================================
    // Context items
    // ========================================================================

    #[test]
    fn library_and_use() {
        let ast = parse_ok(
            "library ieee;
            use ieee.std_logic_1164.all;
            entity top is end entity top;",
        );
        if let DesignUnit::ContextUnit { context, .. } = &ast.units[0] {
            assert_eq!(context.len(), 2);
        } else {
            panic!("expected context unit");
        }
    }

    // ========================================================================
    // Error recovery tests
    // ========================================================================

    #[test]
    fn error_recovery_multiple_errors() {
        let (ast, errors) = parse_vhdl(
            "entity top is
                port (
                    clk : in std_logic
                );
            end entity top;

            entity bottom is end entity bottom;",
        );
        // Should parse both entities despite potential issues
        assert_eq!(ast.units.len(), 2);
        // This should parse cleanly
        assert!(errors.is_empty());
    }

    #[test]
    fn error_recovery_bad_declaration() {
        let (ast, errors) = parse_vhdl(
            "architecture rtl of top is
                badkeyword stuff here;
                signal good : std_logic;
            begin
            end architecture rtl;",
        );
        assert_eq!(ast.units.len(), 1);
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            // Should have recovered and parsed the signal decl
            assert!(a.decls.len() >= 2);
        }
        assert!(!errors.is_empty());
    }

    // ========================================================================
    // Multi-unit file
    // ========================================================================

    #[test]
    fn multi_unit_file() {
        let ast = parse_ok(
            "library ieee;
            use ieee.std_logic_1164.all;

            entity top is
                port (
                    clk : in std_logic;
                    data_out : out std_logic
                );
            end entity top;

            architecture rtl of top is
            begin
                data_out <= clk;
            end architecture rtl;",
        );
        assert_eq!(ast.units.len(), 2);
    }

    // ========================================================================
    // Declaration parsing tests
    // ========================================================================

    #[test]
    fn signal_decl_multi_name() {
        let ast = parse_ok(
            "architecture rtl of top is
                signal a, b, c : std_logic;
            begin
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let Declaration::Signal(s) = &a.decls[0] {
                assert_eq!(s.names.len(), 3);
            } else {
                panic!("expected signal decl");
            }
        }
    }

    #[test]
    fn signal_with_default() {
        let ast = parse_ok(
            "architecture rtl of top is
                signal en : std_logic := '0';
            begin
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let Declaration::Signal(s) = &a.decls[0] {
                assert!(s.default.is_some());
            } else {
                panic!("expected signal decl");
            }
        }
    }

    #[test]
    fn constant_decl() {
        let ast = parse_ok(
            "package p is
                constant WIDTH : integer := 8;
            end package p;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        {
            assert!(matches!(p.decls[0], Declaration::Constant(_)));
        }
    }

    #[test]
    fn variable_decl() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process
                    variable count : integer := 0;
                begin
                    null;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(p.decls[0], Declaration::Variable(_)));
            }
        }
    }

    #[test]
    fn type_enum_decl() {
        let ast = parse_ok(
            "package p is
                type state_t is (idle, running, stopped);
            end package p;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        {
            if let Declaration::Type(t) = &p.decls[0] {
                if let TypeDef::Enumeration { literals, .. } = &t.def {
                    assert_eq!(literals.len(), 3);
                } else {
                    panic!("expected enum type");
                }
            }
        }
    }

    #[test]
    fn type_array_decl() {
        let ast = parse_ok(
            "package p is
                type byte_array is array (0 to 255) of std_logic_vector(7 downto 0);
            end package p;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        {
            assert!(matches!(
                p.decls[0],
                Declaration::Type(TypeDecl {
                    def: TypeDef::Array { .. },
                    ..
                })
            ));
        }
    }

    #[test]
    fn type_record_decl() {
        let ast = parse_ok(
            "package p is
                type pixel is record
                    r : std_logic_vector(7 downto 0);
                    g : std_logic_vector(7 downto 0);
                    b : std_logic_vector(7 downto 0);
                end record;
            end package p;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        {
            if let Declaration::Type(t) = &p.decls[0] {
                if let TypeDef::Record { fields, .. } = &t.def {
                    assert_eq!(fields.len(), 3);
                } else {
                    panic!("expected record type");
                }
            }
        }
    }

    #[test]
    fn subtype_decl() {
        let ast = parse_ok(
            "package p is
                subtype byte is std_logic_vector(7 downto 0);
            end package p;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        {
            assert!(matches!(p.decls[0], Declaration::Subtype(_)));
        }
    }

    #[test]
    fn component_decl() {
        let ast = parse_ok(
            "architecture rtl of top is
                component counter is
                    generic (WIDTH : integer := 8);
                    port (clk : in std_logic);
                end component counter;
            begin
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let Declaration::Component(c) = &a.decls[0] {
                assert!(c.generics.is_some());
                assert!(c.ports.is_some());
            } else {
                panic!("expected component decl");
            }
        }
    }

    #[test]
    fn function_decl() {
        let ast = parse_ok(
            "package p is
                function add(a : integer; b : integer) return integer;
            end package p;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        {
            if let Declaration::Function(f) = &p.decls[0] {
                assert!(!f.has_body);
                assert!(f.pure);
                assert_eq!(f.params.len(), 2);
            } else {
                panic!("expected function decl");
            }
        }
    }

    #[test]
    fn procedure_decl() {
        let ast = parse_ok(
            "package p is
                procedure reset(signal clk : in std_logic);
            end package p;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        {
            if let Declaration::Procedure(pr) = &p.decls[0] {
                assert!(!pr.has_body);
            } else {
                panic!("expected procedure decl");
            }
        }
    }

    #[test]
    fn attribute_decl_and_spec() {
        let ast = parse_ok(
            "architecture rtl of top is
                attribute syn_keep : boolean;
                attribute syn_keep of clk : signal is true;
            begin
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert!(matches!(a.decls[0], Declaration::AttributeDecl(_)));
            assert!(matches!(a.decls[1], Declaration::AttributeSpec(_)));
        }
    }

    // ========================================================================
    // Expression parsing tests
    // ========================================================================

    #[test]
    fn expr_binary_ops() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                y <= a and b;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert_eq!(a.stmts.len(), 1);
        }
    }

    #[test]
    fn expr_precedence() {
        // a + b * c should parse as a + (b * c)
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                y <= a + b * c;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::SignalAssignment(sa) = &a.stmts[0] {
                if let Expr::Binary { op, right, .. } = &sa.waveforms[0].value {
                    assert_eq!(*op, BinaryOp::Add);
                    assert!(matches!(
                        **right,
                        Expr::Binary {
                            op: BinaryOp::Mul,
                            ..
                        }
                    ));
                } else {
                    panic!("expected binary expr");
                }
            }
        }
    }

    #[test]
    fn expr_unary() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                y <= not a;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::SignalAssignment(sa) = &a.stmts[0] {
                assert!(matches!(
                    sa.waveforms[0].value,
                    Expr::Unary {
                        op: UnaryOp::Not,
                        ..
                    }
                ));
            }
        }
    }

    #[test]
    fn expr_parenthesized() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                y <= (a + b) * c;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::SignalAssignment(sa) = &a.stmts[0] {
                if let Expr::Binary { op, left, .. } = &sa.waveforms[0].value {
                    assert_eq!(*op, BinaryOp::Mul);
                    assert!(matches!(**left, Expr::Paren { .. }));
                } else {
                    panic!("expected binary expr");
                }
            }
        }
    }

    #[test]
    fn expr_aggregate_others() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                y <= (others => '0');
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::SignalAssignment(sa) = &a.stmts[0] {
                assert!(matches!(sa.waveforms[0].value, Expr::Aggregate { .. }));
            }
        }
    }

    // ========================================================================
    // Statement parsing tests
    // ========================================================================

    #[test]
    fn process_with_all() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process(all)
                begin
                    y <= a;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(p.sensitivity, SensitivityList::All));
            }
        }
    }

    #[test]
    fn if_elsif_else() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process(sel, a, b, c)
                begin
                    if sel = \"00\" then
                        y <= a;
                    elsif sel = \"01\" then
                        y <= b;
                    else
                        y <= c;
                    end if;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                if let SequentialStatement::If(ifs) = &p.stmts[0] {
                    assert_eq!(ifs.elsif_branches.len(), 1);
                    assert!(!ifs.else_stmts.is_empty());
                } else {
                    panic!("expected if statement");
                }
            }
        }
    }

    #[test]
    fn case_statement() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process(sel)
                begin
                    case sel is
                        when \"00\" =>
                            y <= a;
                        when \"01\" =>
                            y <= b;
                        when others =>
                            y <= '0';
                    end case;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                if let SequentialStatement::Case(cs) = &p.stmts[0] {
                    assert_eq!(cs.alternatives.len(), 3);
                } else {
                    panic!("expected case statement");
                }
            }
        }
    }

    #[test]
    fn for_loop() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process
                begin
                    for i in 0 to 7 loop
                        data(i) <= '0';
                    end loop;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(p.stmts[0], SequentialStatement::ForLoop(_)));
            }
        }
    }

    #[test]
    fn while_loop() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process
                    variable i : integer := 0;
                begin
                    while i < 8 loop
                        data(i) <= '0';
                        i := i + 1;
                    end loop;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(p.stmts[0], SequentialStatement::WhileLoop(_)));
            }
        }
    }

    #[test]
    fn wait_statement() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process
                begin
                    wait until clk'event and clk = '1';
                    q <= d;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(p.stmts[0], SequentialStatement::Wait(_)));
            }
        }
    }

    #[test]
    fn variable_assignment() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process
                    variable count : integer := 0;
                begin
                    count := count + 1;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(
                    p.stmts[0],
                    SequentialStatement::VariableAssignment { .. }
                ));
            }
        }
    }

    #[test]
    fn null_statement() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process
                begin
                    null;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(p.stmts[0], SequentialStatement::Null { .. }));
            }
        }
    }

    #[test]
    fn assert_statement() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                process
                begin
                    assert count < 256
                        report \"counter overflow\"
                        severity error;
                end process;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::Process(p) = &a.stmts[0] {
                assert!(matches!(p.stmts[0], SequentialStatement::Assert { .. }));
            }
        }
    }

    #[test]
    fn for_generate() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                gen : for i in 0 to 7 generate
                    data(i) <= '0';
                end generate gen;
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            assert!(matches!(a.stmts[0], ConcurrentStatement::ForGenerate(_)));
        }
    }

    #[test]
    fn component_instantiation_named() {
        let ast = parse_ok(
            "architecture rtl of top is
            begin
                u1 : counter
                    generic map (WIDTH => 16)
                    port map (clk => clk, rst => rst);
            end architecture rtl;",
        );
        if let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Architecture(a),
            ..
        } = &ast.units[0]
        {
            if let ConcurrentStatement::ComponentInstantiation(c) = &a.stmts[0] {
                assert!(c.generic_map.is_some());
                assert!(c.port_map.is_some());
            }
        }
    }
}
