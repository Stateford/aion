//! Declaration parsing for VHDL-2008.
//!
//! Handles signal, variable, constant, type, subtype, component,
//! function, procedure, alias, and attribute declarations.

use crate::ast::*;
use crate::parser::VhdlParser;
use crate::token::VhdlToken;
use aion_common::Ident;

impl VhdlParser<'_> {
    /// Parses declarations until a terminating keyword is reached.
    pub fn parse_declarations(&mut self) -> Vec<Declaration> {
        let mut decls = Vec::new();
        loop {
            match self.current() {
                VhdlToken::Begin | VhdlToken::End | VhdlToken::Eof => break,
                _ => {
                    if let Some(decl) = self.parse_declaration() {
                        decls.push(decl);
                    }
                }
            }
        }
        decls
    }

    /// Parses a single declaration, returning `None` if recovery consumed it.
    fn parse_declaration(&mut self) -> Option<Declaration> {
        match self.current() {
            VhdlToken::Signal => Some(self.parse_signal_declaration()),
            VhdlToken::Variable | VhdlToken::Shared => Some(self.parse_variable_declaration()),
            VhdlToken::Constant => Some(self.parse_constant_declaration()),
            VhdlToken::Type => Some(self.parse_type_declaration()),
            VhdlToken::Subtype => Some(self.parse_subtype_declaration()),
            VhdlToken::Component => Some(self.parse_component_declaration()),
            VhdlToken::Function | VhdlToken::Pure | VhdlToken::Impure => {
                Some(self.parse_function_declaration())
            }
            VhdlToken::Procedure => Some(self.parse_procedure_declaration()),
            VhdlToken::Alias => Some(self.parse_alias_declaration()),
            VhdlToken::Attribute => Some(self.parse_attribute_decl_or_spec()),
            _ => {
                let span = self.current_span();
                self.error("expected declaration");
                self.recover_to_semicolon();
                Some(Declaration::Error(span))
            }
        }
    }

    /// Parses a signal declaration.
    fn parse_signal_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        self.expect(VhdlToken::Signal);

        let names = self.parse_identifier_list();
        self.expect(VhdlToken::Colon);
        let ty = self.parse_type_indication();

        let default = if self.eat(VhdlToken::ColonEquals) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        Declaration::Signal(SignalDecl {
            names,
            ty,
            default,
            span,
        })
    }

    /// Parses a variable declaration (possibly shared).
    fn parse_variable_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        let shared = self.eat(VhdlToken::Shared);
        self.expect(VhdlToken::Variable);

        let names = self.parse_identifier_list();
        self.expect(VhdlToken::Colon);
        let ty = self.parse_type_indication();

        let default = if self.eat(VhdlToken::ColonEquals) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        Declaration::Variable(VariableDecl {
            shared,
            names,
            ty,
            default,
            span,
        })
    }

    /// Parses a constant declaration.
    fn parse_constant_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        self.expect(VhdlToken::Constant);

        let names = self.parse_identifier_list();
        self.expect(VhdlToken::Colon);
        let ty = self.parse_type_indication();

        let value = if self.eat(VhdlToken::ColonEquals) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        Declaration::Constant(ConstantDecl {
            names,
            ty,
            value,
            span,
        })
    }

    /// Parses a type declaration.
    fn parse_type_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        self.expect(VhdlToken::Type);

        let name = self.expect_ident();

        // Incomplete type declaration: `type foo;`
        if self.eat(VhdlToken::Semicolon) {
            let span = start.merge(self.prev_span());
            return Declaration::Type(TypeDecl {
                name,
                def: TypeDef::Incomplete { span },
                span,
            });
        }

        self.expect(VhdlToken::Is);

        let def = match self.current() {
            // Enumeration: ( literal {, literal} )
            VhdlToken::LeftParen => self.parse_enum_type_def(),
            // Range: range constraint
            VhdlToken::Range => {
                let def_start = self.current_span();
                self.advance();
                let constraint = self.parse_range_constraint();
                let span = def_start.merge(constraint.span);
                TypeDef::Range { constraint, span }
            }
            // Array: array ( ... ) of ...
            VhdlToken::Array => self.parse_array_type_def(),
            // Record: record ... end record
            VhdlToken::Record => self.parse_record_type_def(),
            _ => {
                let span = self.current_span();
                self.error("expected type definition");
                self.recover_to_semicolon();
                TypeDef::Incomplete { span }
            }
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        Declaration::Type(TypeDecl { name, def, span })
    }

    /// Parses an enumeration type definition.
    fn parse_enum_type_def(&mut self) -> TypeDef {
        let start = self.current_span();
        self.expect(VhdlToken::LeftParen);

        let mut literals = Vec::new();
        loop {
            if self.at(VhdlToken::CharLiteral) {
                let span = self.current_span();
                let text = self.current_text();
                let ch = text.chars().nth(1).unwrap_or('?');
                self.advance();
                literals.push(EnumLiteral::Char(ch, span));
            } else {
                let span = self.current_span();
                let name = self.expect_ident();
                literals.push(EnumLiteral::Ident(name, span));
            }
            if !self.eat(VhdlToken::Comma) {
                break;
            }
        }

        self.expect(VhdlToken::RightParen);
        let span = start.merge(self.prev_span());
        TypeDef::Enumeration { literals, span }
    }

    /// Parses an array type definition.
    fn parse_array_type_def(&mut self) -> TypeDef {
        let start = self.current_span();
        self.expect(VhdlToken::Array);
        self.expect(VhdlToken::LeftParen);

        let mut indices = Vec::new();
        indices.push(self.parse_discrete_range());
        while self.eat(VhdlToken::Comma) {
            indices.push(self.parse_discrete_range());
        }

        self.expect(VhdlToken::RightParen);
        self.expect(VhdlToken::Of);
        let element_type = self.parse_type_indication();

        let span = start.merge(element_type.span);
        TypeDef::Array {
            indices,
            element_type: Box::new(element_type),
            span,
        }
    }

    /// Parses a record type definition.
    fn parse_record_type_def(&mut self) -> TypeDef {
        let start = self.current_span();
        self.expect(VhdlToken::Record);

        let mut fields = Vec::new();
        while !self.at(VhdlToken::End) && !self.at_eof() {
            let field_start = self.current_span();
            let names = self.parse_identifier_list();
            self.expect(VhdlToken::Colon);
            let ty = self.parse_type_indication();
            self.expect(VhdlToken::Semicolon);
            let span = field_start.merge(self.prev_span());
            fields.push(RecordField { names, ty, span });
        }

        self.expect(VhdlToken::End);
        self.expect(VhdlToken::Record);
        let span = start.merge(self.prev_span());
        TypeDef::Record { fields, span }
    }

    /// Parses a subtype declaration.
    fn parse_subtype_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        self.expect(VhdlToken::Subtype);
        let name = self.expect_ident();
        self.expect(VhdlToken::Is);
        let ty = self.parse_type_indication();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());
        Declaration::Subtype(SubtypeDecl { name, ty, span })
    }

    /// Parses a component declaration.
    fn parse_component_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        self.expect(VhdlToken::Component);
        let name = self.expect_ident();
        self.eat(VhdlToken::Is); // optional

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

        self.expect(VhdlToken::End);
        self.eat(VhdlToken::Component);
        self.eat_ident(); // optional name
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        Declaration::Component(ComponentDecl {
            name,
            generics,
            ports,
            span,
        })
    }

    /// Parses a function declaration or body.
    fn parse_function_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        let pure = if self.eat(VhdlToken::Impure) {
            false
        } else {
            self.eat(VhdlToken::Pure);
            true
        };
        self.expect(VhdlToken::Function);
        let name = self.expect_ident();

        let params = if self.at(VhdlToken::LeftParen) {
            self.parse_interface_list()
        } else {
            Vec::new()
        };

        self.expect(VhdlToken::Return);
        let return_type = self.parse_type_indication();

        // Check if this is a body or just a declaration
        if self.eat(VhdlToken::Is) {
            let decls = self.parse_declarations();
            self.expect(VhdlToken::Begin);
            let stmts = self.parse_sequential_statements();
            self.expect(VhdlToken::End);
            self.eat(VhdlToken::Function);
            self.eat_ident();
            self.expect(VhdlToken::Semicolon);
            let span = start.merge(self.prev_span());
            Declaration::Function(FunctionDecl {
                pure,
                name,
                params,
                return_type,
                decls,
                stmts,
                has_body: true,
                span,
            })
        } else {
            self.expect(VhdlToken::Semicolon);
            let span = start.merge(self.prev_span());
            Declaration::Function(FunctionDecl {
                pure,
                name,
                params,
                return_type,
                decls: Vec::new(),
                stmts: Vec::new(),
                has_body: false,
                span,
            })
        }
    }

    /// Parses a procedure declaration or body.
    fn parse_procedure_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        self.expect(VhdlToken::Procedure);
        let name = self.expect_ident();

        let params = if self.at(VhdlToken::LeftParen) {
            self.parse_interface_list()
        } else {
            Vec::new()
        };

        if self.eat(VhdlToken::Is) {
            let decls = self.parse_declarations();
            self.expect(VhdlToken::Begin);
            let stmts = self.parse_sequential_statements();
            self.expect(VhdlToken::End);
            self.eat(VhdlToken::Procedure);
            self.eat_ident();
            self.expect(VhdlToken::Semicolon);
            let span = start.merge(self.prev_span());
            Declaration::Procedure(ProcedureDecl {
                name,
                params,
                decls,
                stmts,
                has_body: true,
                span,
            })
        } else {
            self.expect(VhdlToken::Semicolon);
            let span = start.merge(self.prev_span());
            Declaration::Procedure(ProcedureDecl {
                name,
                params,
                decls: Vec::new(),
                stmts: Vec::new(),
                has_body: false,
                span,
            })
        }
    }

    /// Parses an alias declaration.
    fn parse_alias_declaration(&mut self) -> Declaration {
        let start = self.current_span();
        self.expect(VhdlToken::Alias);
        let name = self.expect_ident();

        let ty = if self.eat(VhdlToken::Colon) {
            Some(self.parse_type_indication())
        } else {
            None
        };

        self.expect(VhdlToken::Is);
        let value = self.parse_expr();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        Declaration::Alias(AliasDecl {
            name,
            ty,
            value,
            span,
        })
    }

    /// Parses an attribute declaration or specification.
    fn parse_attribute_decl_or_spec(&mut self) -> Declaration {
        let start = self.current_span();
        self.expect(VhdlToken::Attribute);
        let name = self.expect_ident();

        if self.eat(VhdlToken::Colon) {
            // Attribute declaration: attribute name : type;
            let ty = self.parse_type_indication();
            self.expect(VhdlToken::Semicolon);
            let span = start.merge(self.prev_span());
            Declaration::AttributeDecl(AttributeDeclNode { name, ty, span })
        } else if self.eat(VhdlToken::Of) {
            // Attribute specification: attribute name of entity : class is expr;
            let entity = self.expect_ident();
            self.expect(VhdlToken::Colon);
            let entity_class = self.expect_ident_or_keyword();
            self.expect(VhdlToken::Is);
            let value = self.parse_expr();
            self.expect(VhdlToken::Semicolon);
            let span = start.merge(self.prev_span());
            Declaration::AttributeSpec(AttributeSpecNode {
                name,
                entity,
                entity_class,
                value,
                span,
            })
        } else {
            let span = self.current_span();
            self.error("expected ':' or 'of' after attribute name");
            self.recover_to_semicolon();
            Declaration::Error(start.merge(span))
        }
    }

    /// Parses a comma-separated list of identifiers.
    pub(crate) fn parse_identifier_list(&mut self) -> Vec<Ident> {
        let mut names = Vec::new();
        names.push(self.expect_ident());
        while self.eat(VhdlToken::Comma) {
            names.push(self.expect_ident());
        }
        names
    }
}
