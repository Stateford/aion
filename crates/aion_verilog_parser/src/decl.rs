//! Declaration and module item parsing for Verilog-2005.
//!
//! Handles net/reg/integer/real declarations, parameter/localparam, continuous
//! assign, always/initial blocks, module instantiation, generate blocks,
//! function/task declarations, gate primitives, and genvar/defparam.
//!
//! **Instantiation detection:** When an identifier appears at module-item level,
//! the parser peeks at the next token — if it's also an identifier or `#`,
//! this is a module instantiation rather than a declaration.

use crate::ast::*;
use crate::parser::VerilogParser;
use crate::token::VerilogToken;
use aion_source::Span;

impl VerilogParser<'_> {
    /// Parses module items until `endmodule` or EOF.
    pub fn parse_module_items(&mut self) -> Vec<ModuleItem> {
        let mut items = Vec::new();
        while !self.at(VerilogToken::Endmodule) && !self.at_eof() {
            items.extend(self.parse_module_item());
        }
        items
    }

    /// Returns `true` if the current position looks like the start of a declaration.
    pub(crate) fn is_at_declaration_start(&self) -> bool {
        matches!(
            self.current(),
            VerilogToken::Wire
                | VerilogToken::Reg
                | VerilogToken::Integer
                | VerilogToken::Real
                | VerilogToken::Tri
                | VerilogToken::Supply0
                | VerilogToken::Supply1
                | VerilogToken::Parameter
                | VerilogToken::Localparam
                | VerilogToken::Input
                | VerilogToken::Output
                | VerilogToken::Inout
                | VerilogToken::Genvar
        )
    }

    /// Parses one module item declaration and returns every AST item it
    /// declares.
    ///
    /// Most declarations yield exactly one item, but a comma-separated
    /// parameter list (`localparam A = 0, B = 1;`) yields one
    /// [`ModuleItem::ParameterDecl`]/[`ModuleItem::LocalparamDecl`] per name,
    /// and a multi-instance gate statement (`and g1(...), g2(...);`) yields
    /// one [`ModuleItem::GateInst`] per instance. Every caller that collects
    /// module items must go through this so none of them is dropped.
    pub(crate) fn parse_module_item(&mut self) -> Vec<ModuleItem> {
        match self.current() {
            VerilogToken::Parameter => self.parse_parameter_item(false),
            VerilogToken::Localparam => self.parse_parameter_item(true),
            VerilogToken::And
            | VerilogToken::Or
            | VerilogToken::Nand
            | VerilogToken::Nor
            | VerilogToken::Xor
            | VerilogToken::Xnor
            | VerilogToken::Not
            | VerilogToken::Buf => self.parse_gate_instantiation(),
            _ => self.parse_module_item_inner().into_iter().collect(),
        }
    }

    /// Parses a single module item that declares exactly one AST item.
    ///
    /// Parameters and gate instantiations, which can declare several items,
    /// are handled by [`Self::parse_module_item`]; call that instead.
    fn parse_module_item_inner(&mut self) -> Option<ModuleItem> {
        match self.current() {
            // Net declarations
            VerilogToken::Wire
            | VerilogToken::Tri
            | VerilogToken::Supply0
            | VerilogToken::Supply1 => Some(self.parse_net_declaration()),

            // Reg declaration
            VerilogToken::Reg => Some(self.parse_reg_declaration()),

            // Integer declaration
            VerilogToken::Integer => Some(self.parse_integer_declaration()),

            // Real declaration
            VerilogToken::Real => Some(self.parse_real_declaration()),

            // Port declarations (non-ANSI style)
            VerilogToken::Input | VerilogToken::Output | VerilogToken::Inout => {
                Some(self.parse_port_declaration())
            }

            // Continuous assignment
            VerilogToken::Assign => Some(self.parse_continuous_assign()),

            // Always block
            VerilogToken::Always => Some(self.parse_always_block()),

            // Initial block
            VerilogToken::Initial => Some(self.parse_initial_block()),

            // Generate block
            VerilogToken::Generate => Some(self.parse_generate_block()),

            // Genvar
            VerilogToken::Genvar => Some(self.parse_genvar_declaration()),

            // Function
            VerilogToken::Function => Some(self.parse_function_declaration()),

            // Task
            VerilogToken::Task => Some(self.parse_task_declaration()),

            // Defparam
            VerilogToken::Defparam => Some(self.parse_defparam()),

            // Identifier: could be module instantiation
            VerilogToken::Identifier | VerilogToken::EscapedIdentifier => {
                // Detect instantiation: ident ident or ident #
                let next = self.peek_kind(1);
                if next == VerilogToken::Identifier
                    || next == VerilogToken::EscapedIdentifier
                    || next == VerilogToken::Hash
                {
                    Some(self.parse_module_instantiation())
                } else {
                    let span = self.current_span();
                    self.error("expected module item");
                    self.recover_to_semicolon();
                    Some(ModuleItem::Error(span))
                }
            }

            _ => {
                let span = self.current_span();
                self.error("expected module item");
                self.recover_to_semicolon();
                Some(ModuleItem::Error(span))
            }
        }
    }

    /// Parses a net declaration (wire, tri, supply0, supply1).
    fn parse_net_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        let net_type = self.eat_net_type().expect("should be at net type");
        let signed = self.eat(VerilogToken::Signed);
        let range = if self.at(VerilogToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };
        // Optional net delay: `wire [7:0] #1 w;`
        let delay = if self.at(VerilogToken::Hash) {
            Some(self.parse_delay(3))
        } else {
            None
        };

        let names = self.parse_decl_name_list();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::NetDecl(NetDecl {
            net_type,
            signed,
            range,
            delay,
            names,
            span,
        })
    }

    /// Parses a reg declaration.
    fn parse_reg_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Reg);
        let signed = self.eat(VerilogToken::Signed);
        let range = if self.at(VerilogToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let names = self.parse_decl_name_list();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::RegDecl(RegDecl {
            signed,
            range,
            names,
            span,
        })
    }

    /// Parses an integer variable declaration.
    fn parse_integer_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Integer);
        let names = self.parse_decl_name_list();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::IntegerDecl(IntegerDecl { names, span })
    }

    /// Parses a real variable declaration.
    fn parse_real_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Real);
        let names = self.parse_decl_name_list();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::RealDecl(RealDecl { names, span })
    }

    /// Parses a comma-separated list of declaration names with optional dimensions and init.
    fn parse_decl_name_list(&mut self) -> Vec<DeclName> {
        let mut names = Vec::new();
        names.push(self.parse_decl_name());
        while self.eat(VerilogToken::Comma) {
            names.push(self.parse_decl_name());
        }
        names
    }

    /// Parses a single declaration name with optional array dimensions and init.
    fn parse_decl_name(&mut self) -> DeclName {
        let start = self.current_span();
        let name = self.expect_ident();
        let mut dimensions = Vec::new();
        while self.at(VerilogToken::LeftBracket) {
            dimensions.push(self.parse_range());
        }
        let init = if self.eat(VerilogToken::Equals) {
            Some(self.parse_expr())
        } else {
            None
        };
        let span = start.merge(self.prev_span());
        DeclName {
            name,
            dimensions,
            init,
            span,
        }
    }

    /// Parses a `parameter`/`localparam` declaration in a module body and
    /// returns one item per declared name.
    ///
    /// `localparam [1:0] IDLE = 0, RUN = 1, DONE = 2;` yields three
    /// [`ModuleItem::LocalparamDecl`] items. Following names inherit the
    /// `signed` flag and range of the first one, as in IEEE 1364-2005 12.2
    /// (`parameter [signed] [range] list_of_param_assignments`).
    fn parse_parameter_item(&mut self, local: bool) -> Vec<ModuleItem> {
        let first = self.parse_single_parameter_decl(local);
        let mut params = Vec::new();
        while self.eat(VerilogToken::Comma) {
            let p_start = self.current_span();
            // Lenient: accept a per-name `signed`/range, else inherit.
            let signed = self.eat(VerilogToken::Signed);
            let range = if self.at(VerilogToken::LeftBracket) {
                Some(self.parse_range())
            } else {
                None
            };
            let (signed, range) = if !signed && range.is_none() {
                (first.signed, first.range.clone())
            } else {
                (signed, range)
            };
            let name = self.expect_ident();
            let value = if self.eat(VerilogToken::Equals) {
                Some(self.parse_expr())
            } else {
                None
            };
            let span = p_start.merge(self.prev_span());
            params.push(ParameterDecl {
                local: first.local,
                signed,
                range,
                name,
                value,
                span,
            });
        }
        self.expect(VerilogToken::Semicolon);

        std::iter::once(first)
            .chain(params)
            .map(|p| {
                if p.local {
                    ModuleItem::LocalparamDecl(p)
                } else {
                    ModuleItem::ParameterDecl(p)
                }
            })
            .collect()
    }

    /// Parses a non-ANSI port declaration in the module body.
    fn parse_port_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        let direction = match self.current() {
            VerilogToken::Input => {
                self.advance();
                Direction::Input
            }
            VerilogToken::Output => {
                self.advance();
                Direction::Output
            }
            VerilogToken::Inout => {
                self.advance();
                Direction::Inout
            }
            _ => {
                self.error("expected port direction");
                Direction::Input
            }
        };

        let net_type = self.eat_net_type();
        let signed = self.eat(VerilogToken::Signed);
        let range = if self.at(VerilogToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let names = self.parse_identifier_list();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::PortDecl(PortDecl {
            direction,
            net_type,
            signed,
            range,
            names,
            span,
        })
    }

    /// Parses a continuous assignment.
    fn parse_continuous_assign(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Assign);
        // Optional delay: `assign #2 y = a;` / `assign #(1, 2) y = a;`
        let delay = if self.at(VerilogToken::Hash) {
            Some(self.parse_delay(3))
        } else {
            None
        };
        let target = self.parse_expr();
        self.expect(VerilogToken::Equals);
        let value = self.parse_expr();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::ContinuousAssign(ContinuousAssign {
            delay,
            target,
            value,
            span,
        })
    }

    /// Parses an always block.
    fn parse_always_block(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Always);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        ModuleItem::AlwaysBlock(AlwaysBlock { body, span })
    }

    /// Parses an initial block.
    fn parse_initial_block(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Initial);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        ModuleItem::InitialBlock(InitialBlock { body, span })
    }

    /// Parses a module instantiation.
    fn parse_module_instantiation(&mut self) -> ModuleItem {
        let start = self.current_span();
        let module_name = self.expect_ident();

        // Optional parameter overrides: #(...)
        let param_overrides = if self.at(VerilogToken::Hash) {
            self.advance();
            self.parse_connection_list()
        } else {
            Vec::new()
        };

        // Parse instances (there can be multiple: mod u1(...), u2(...);)
        let mut instances = Vec::new();
        loop {
            let inst = self.parse_single_instance();
            instances.push(inst);
            if !self.eat(VerilogToken::Comma) {
                break;
            }
        }

        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::Instantiation(Instantiation {
            module_name,
            param_overrides,
            instances,
            span,
        })
    }

    /// Parses a single instance: `name [range] ( connections )`.
    fn parse_single_instance(&mut self) -> Instance {
        let start = self.current_span();
        let name = self.expect_ident();
        let range = if self.at(VerilogToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };
        let connections = self.parse_connection_list();
        let span = start.merge(self.prev_span());
        Instance {
            name,
            range,
            connections,
            span,
        }
    }

    /// Parses a connection list: `( connection {, connection} )`.
    fn parse_connection_list(&mut self) -> Vec<Connection> {
        self.expect(VerilogToken::LeftParen);
        let mut connections = Vec::new();
        if !self.at(VerilogToken::RightParen) {
            loop {
                connections.push(self.parse_connection());
                if !self.eat(VerilogToken::Comma) {
                    break;
                }
            }
        }
        self.expect(VerilogToken::RightParen);
        connections
    }

    /// Parses a single connection (named or positional).
    fn parse_connection(&mut self) -> Connection {
        let start = self.current_span();
        // Named connection: .name(expr) or .name()
        if self.at(VerilogToken::Dot) {
            self.advance();
            let formal = self.expect_ident();
            self.expect(VerilogToken::LeftParen);
            let actual = if self.at(VerilogToken::RightParen) {
                None
            } else {
                Some(self.parse_expr())
            };
            self.expect(VerilogToken::RightParen);
            let span = start.merge(self.prev_span());
            Connection {
                formal: Some(formal),
                actual,
                span,
            }
        } else {
            // Positional connection
            let actual = self.parse_expr();
            let span = start.merge(actual.span());
            Connection {
                formal: None,
                actual: Some(actual),
                span,
            }
        }
    }

    /// Parses a gate primitive instantiation and returns one item per
    /// instance.
    ///
    /// Accepts an optional delay and several comma-separated instances:
    /// `and #1 g1(y, a, b), g2(z, c, d);` yields two [`GateInst`] items,
    /// each carrying a copy of the delay.
    fn parse_gate_instantiation(&mut self) -> Vec<ModuleItem> {
        let start = self.current_span();
        let text = self.current_text();
        let gate_type = self.interner.get_or_intern(text);
        self.advance();

        // Optional gate delay: `and #1 ...`, `and #(1, 2) ...`
        let delay = if self.at(VerilogToken::Hash) {
            Some(self.parse_delay(3))
        } else {
            None
        };

        let mut items = Vec::new();
        let mut inst_start = start;
        loop {
            // Optional instance name
            let name = if self.at(VerilogToken::Identifier) && self.peek_is(VerilogToken::LeftParen)
            {
                Some(self.expect_ident())
            } else {
                None
            };

            self.expect(VerilogToken::LeftParen);
            let mut ports = Vec::new();
            if !self.at(VerilogToken::RightParen) {
                ports.push(self.parse_expr());
                while self.eat(VerilogToken::Comma) {
                    ports.push(self.parse_expr());
                }
            }
            self.expect(VerilogToken::RightParen);
            let span = inst_start.merge(self.prev_span());

            items.push(ModuleItem::GateInst(GateInst {
                gate_type,
                name,
                delay: delay.clone(),
                ports,
                span,
            }));

            if !self.eat(VerilogToken::Comma) {
                break;
            }
            inst_start = self.current_span();
        }
        self.expect(VerilogToken::Semicolon);
        items
    }

    /// Parses a generate block.
    fn parse_generate_block(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Generate);

        let block = if self.at(VerilogToken::For) {
            self.parse_generate_for(start)
        } else if self.at(VerilogToken::If) {
            self.parse_generate_if(start)
        } else {
            // Just a generate ... endgenerate wrapper
            let mut items = Vec::new();
            while !self.at(VerilogToken::Endgenerate) && !self.at_eof() {
                items.extend(self.parse_module_item());
            }
            self.expect(VerilogToken::Endgenerate);
            let span = start.merge(self.prev_span());
            return ModuleItem::GenerateBlock(GenerateBlock::If {
                condition: Expr::Literal { span },
                then_items: items,
                else_items: Vec::new(),
                span,
            });
        };

        self.expect(VerilogToken::Endgenerate);
        ModuleItem::GenerateBlock(block)
    }

    /// Parses a generate-for loop.
    fn parse_generate_for(&mut self, start: Span) -> GenerateBlock {
        self.expect(VerilogToken::For);
        self.expect(VerilogToken::LeftParen);

        let init = Box::new(self.parse_blocking_assignment_stmt());
        let condition = self.parse_expr();
        self.expect(VerilogToken::Semicolon);
        let step = Box::new(self.parse_blocking_assignment_no_semi());
        self.expect(VerilogToken::RightParen);

        // Optional begin : label
        let label = if self.eat(VerilogToken::Begin) {
            if self.eat(VerilogToken::Colon) {
                Some(self.expect_ident())
            } else {
                None
            }
        } else {
            None
        };

        let mut items = Vec::new();
        let has_begin = label.is_some() || self.at(VerilogToken::End);
        if has_begin {
            // Wrapped in begin...end
            while !self.at(VerilogToken::End) && !self.at_eof() {
                items.extend(self.parse_module_item());
            }
            self.expect(VerilogToken::End);
        } else {
            // Single item after for (...)
            // We already didn't see begin, so check if there are items before endgenerate
            while !self.at(VerilogToken::Endgenerate) && !self.at_eof() {
                let parsed = self.parse_module_item();
                if !parsed.is_empty() {
                    items.extend(parsed);
                    break;
                }
            }
        }

        let span = start.merge(self.prev_span());
        GenerateBlock::For {
            init,
            condition,
            step,
            label,
            items,
            span,
        }
    }

    /// Parses a generate-if conditional.
    fn parse_generate_if(&mut self, start: Span) -> GenerateBlock {
        self.expect(VerilogToken::If);
        self.expect(VerilogToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(VerilogToken::RightParen);

        let then_items = self.parse_generate_body();

        let else_items = if self.eat(VerilogToken::Else) {
            self.parse_generate_body()
        } else {
            Vec::new()
        };

        let span = start.merge(self.prev_span());
        GenerateBlock::If {
            condition,
            then_items,
            else_items,
            span,
        }
    }

    /// Parses a generate body: either a begin...end block or a single item.
    fn parse_generate_body(&mut self) -> Vec<ModuleItem> {
        let mut items = Vec::new();
        if self.eat(VerilogToken::Begin) {
            // Optional label
            if self.eat(VerilogToken::Colon) {
                let _ = self.expect_ident();
            }
            while !self.at(VerilogToken::End) && !self.at_eof() {
                items.extend(self.parse_module_item());
            }
            self.expect(VerilogToken::End);
        } else {
            items.extend(self.parse_module_item());
        }
        items
    }

    /// Parses a genvar declaration.
    fn parse_genvar_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Genvar);
        let names = self.parse_identifier_list();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::GenvarDecl(GenvarDecl { names, span })
    }

    /// Parses a defparam statement.
    fn parse_defparam(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Defparam);
        let target = self.parse_expr();
        self.expect(VerilogToken::Equals);
        let value = self.parse_expr();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::DefparamDecl(DefparamDecl {
            target,
            value,
            span,
        })
    }

    /// Parses a function declaration.
    fn parse_function_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Function);

        let automatic = self.eat(VerilogToken::Automatic);
        let signed = self.eat(VerilogToken::Signed);
        let range = if self.at(VerilogToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let name = self.expect_ident();
        self.expect(VerilogToken::Semicolon);

        // Parse inputs and declarations
        let mut inputs = Vec::new();
        let mut decls = Vec::new();
        while !self.at(VerilogToken::Begin) && !self.at(VerilogToken::Endfunction) && !self.at_eof()
        {
            if self.at(VerilogToken::Input) {
                let port = self.parse_port_decl_in_subprogram();
                inputs.push(port);
            } else if self.is_at_declaration_start() {
                decls.extend(self.parse_module_item());
            } else {
                break;
            }
        }

        // Body statements
        let mut body = Vec::new();
        if self.at(VerilogToken::Begin) {
            let block = self.parse_statement();
            body.push(block);
        } else {
            // Single statement or just statements until endfunction
            while !self.at(VerilogToken::Endfunction) && !self.at_eof() {
                body.push(self.parse_statement());
            }
        }

        self.expect(VerilogToken::Endfunction);
        let span = start.merge(self.prev_span());

        ModuleItem::FunctionDecl(FunctionDecl {
            automatic,
            signed,
            range,
            name,
            inputs,
            decls,
            body,
            span,
        })
    }

    /// Parses a task declaration.
    fn parse_task_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(VerilogToken::Task);

        let automatic = self.eat(VerilogToken::Automatic);
        let name = self.expect_ident();
        self.expect(VerilogToken::Semicolon);

        // Parse ports and declarations
        let mut ports = Vec::new();
        let mut decls = Vec::new();
        while !self.at(VerilogToken::Begin) && !self.at(VerilogToken::Endtask) && !self.at_eof() {
            if self.current().is_direction() {
                let port = self.parse_port_decl_in_subprogram();
                ports.push(port);
            } else if self.is_at_declaration_start() {
                decls.extend(self.parse_module_item());
            } else {
                break;
            }
        }

        let mut body = Vec::new();
        while !self.at(VerilogToken::Endtask) && !self.at_eof() {
            body.push(self.parse_statement());
        }

        self.expect(VerilogToken::Endtask);
        let span = start.merge(self.prev_span());

        ModuleItem::TaskDecl(TaskDecl {
            automatic,
            name,
            ports,
            decls,
            body,
            span,
        })
    }

    /// Parses a port declaration inside a function or task.
    fn parse_port_decl_in_subprogram(&mut self) -> PortDecl {
        let start = self.current_span();
        let direction = match self.current() {
            VerilogToken::Input => {
                self.advance();
                Direction::Input
            }
            VerilogToken::Output => {
                self.advance();
                Direction::Output
            }
            VerilogToken::Inout => {
                self.advance();
                Direction::Inout
            }
            _ => {
                self.error("expected port direction");
                Direction::Input
            }
        };

        let net_type = self.eat_net_type();
        let signed = self.eat(VerilogToken::Signed);
        let range = if self.at(VerilogToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let names = self.parse_identifier_list();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());

        PortDecl {
            direction,
            net_type,
            signed,
            range,
            names,
            span,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::*;
    use crate::lexer;
    use crate::parser::VerilogParser;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::FileId;

    fn parse_module(source: &str) -> ModuleDecl {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = VerilogParser::new(tokens, source, file, &interner, &sink);
        let ast = parser.parse_source_file();
        assert!(
            !sink.has_errors(),
            "unexpected errors: {:?}",
            sink.diagnostics()
        );
        match ast.items.into_iter().next().unwrap() {
            VerilogItem::Module(m) => m,
            _ => panic!("expected module"),
        }
    }

    #[test]
    fn wire_declaration() {
        let m = parse_module("module t; wire [7:0] data; endmodule");
        assert!(matches!(m.items[0], ModuleItem::NetDecl(_)));
        if let ModuleItem::NetDecl(ref n) = m.items[0] {
            assert_eq!(n.net_type, NetType::Wire);
            assert!(n.range.is_some());
        }
    }

    #[test]
    fn reg_declaration() {
        let m = parse_module("module t; reg [7:0] q; endmodule");
        assert!(matches!(m.items[0], ModuleItem::RegDecl(_)));
    }

    #[test]
    fn integer_declaration() {
        let m = parse_module("module t; integer i, j; endmodule");
        assert!(matches!(m.items[0], ModuleItem::IntegerDecl(_)));
        if let ModuleItem::IntegerDecl(ref d) = m.items[0] {
            assert_eq!(d.names.len(), 2);
        }
    }

    #[test]
    fn real_declaration() {
        let m = parse_module("module t; real x; endmodule");
        assert!(matches!(m.items[0], ModuleItem::RealDecl(_)));
    }

    #[test]
    fn parameter_declaration() {
        let m = parse_module("module t; parameter WIDTH = 8; endmodule");
        assert!(matches!(m.items[0], ModuleItem::ParameterDecl(_)));
    }

    #[test]
    fn localparam_declaration() {
        let m = parse_module("module t; localparam MAX = 255; endmodule");
        assert!(matches!(m.items[0], ModuleItem::LocalparamDecl(_)));
    }

    #[test]
    fn continuous_assign() {
        let m = parse_module("module t; assign y = a & b; endmodule");
        assert!(matches!(m.items[0], ModuleItem::ContinuousAssign(_)));
    }

    #[test]
    fn always_block() {
        let m = parse_module("module t; always @(posedge clk) q <= d; endmodule");
        assert!(matches!(m.items[0], ModuleItem::AlwaysBlock(_)));
    }

    #[test]
    fn initial_block() {
        let m = parse_module("module t; initial begin clk = 0; end endmodule");
        assert!(matches!(m.items[0], ModuleItem::InitialBlock(_)));
    }

    #[test]
    fn module_instantiation_named() {
        let m = parse_module(
            "module t;
                counter #(.WIDTH(8)) u1 (.clk(clk), .rst(rst), .count(count));
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::Instantiation(_)));
        if let ModuleItem::Instantiation(ref inst) = m.items[0] {
            assert!(!inst.param_overrides.is_empty());
            assert_eq!(inst.instances.len(), 1);
            assert_eq!(inst.instances[0].connections.len(), 3);
            assert!(inst.instances[0].connections[0].formal.is_some());
        }
    }

    #[test]
    fn module_instantiation_positional() {
        let m = parse_module(
            "module t;
                counter u1 (clk, rst, count);
            endmodule",
        );
        if let ModuleItem::Instantiation(ref inst) = m.items[0] {
            assert_eq!(inst.instances[0].connections.len(), 3);
            assert!(inst.instances[0].connections[0].formal.is_none());
        }
    }

    #[test]
    fn gate_instantiation() {
        let m = parse_module("module t; and g1(y, a, b); endmodule");
        assert!(matches!(m.items[0], ModuleItem::GateInst(_)));
        if let ModuleItem::GateInst(ref g) = m.items[0] {
            assert_eq!(g.ports.len(), 3);
        }
    }

    #[test]
    fn genvar_declaration() {
        let m = parse_module("module t; genvar i; endmodule");
        assert!(matches!(m.items[0], ModuleItem::GenvarDecl(_)));
    }

    #[test]
    fn function_declaration() {
        let m = parse_module(
            "module t;
                function [7:0] add;
                    input [7:0] a;
                    input [7:0] b;
                    begin
                        add = a + b;
                    end
                endfunction
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::FunctionDecl(_)));
        if let ModuleItem::FunctionDecl(ref f) = m.items[0] {
            assert_eq!(f.inputs.len(), 2);
            assert!(f.range.is_some());
        }
    }

    #[test]
    fn task_declaration() {
        let m = parse_module(
            "module t;
                task do_reset;
                    input clk;
                    begin
                        @(posedge clk) ;
                    end
                endtask
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::TaskDecl(_)));
    }

    #[test]
    fn non_ansi_port_decl() {
        let m = parse_module(
            "module t(clk, data);
                input clk;
                output [7:0] data;
            endmodule",
        );
        assert_eq!(m.port_names.len(), 2);
        assert_eq!(m.items.len(), 2);
        assert!(matches!(m.items[0], ModuleItem::PortDecl(_)));
    }

    #[test]
    fn generate_for() {
        let m = parse_module(
            "module t;
                genvar i;
                generate
                    for (i = 0; i < 8; i = i + 1) begin : gen_bits
                        assign data[i] = 0;
                    end
                endgenerate
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::GenvarDecl(_)));
        assert!(matches!(m.items[1], ModuleItem::GenerateBlock(_)));
    }

    #[test]
    fn generate_if() {
        let m = parse_module(
            "module t;
                generate
                    if (WIDTH > 8) begin
                        wire [15:0] wide;
                    end else begin
                        wire [7:0] narrow;
                    end
                endgenerate
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::GenerateBlock(_)));
        if let ModuleItem::GenerateBlock(GenerateBlock::If {
            then_items,
            else_items,
            ..
        }) = &m.items[0]
        {
            assert_eq!(then_items.len(), 1);
            assert_eq!(else_items.len(), 1);
        }
    }

    // ---- PAR-1: comma-separated parameters / PAR-21: delays ----

    /// Parses `source`, returning the first module, the interner (to resolve
    /// names), and every diagnostic.
    fn parse_full(source: &str) -> (ModuleDecl, Interner, Vec<aion_diagnostics::Diagnostic>) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let ast = VerilogParser::new(tokens, source, file, &interner, &sink).parse_source_file();
        let m = match ast.items.into_iter().next().unwrap() {
            VerilogItem::Module(m) => m,
            _ => panic!("expected module"),
        };
        (m, interner, sink.take_all())
    }

    /// Returns `(name, value text, local)` for every parameter item.
    fn param_items(
        items: &[ModuleItem],
        interner: &Interner,
        source: &str,
    ) -> Vec<(String, String, bool)> {
        items
            .iter()
            .filter_map(|item| match item {
                ModuleItem::ParameterDecl(p) | ModuleItem::LocalparamDecl(p) => {
                    let is_local = matches!(item, ModuleItem::LocalparamDecl(_));
                    assert_eq!(is_local, p.local);
                    let v = p.value.as_ref().expect("value").span();
                    Some((
                        interner.resolve(p.name).to_string(),
                        source[v.start as usize..v.end as usize].to_string(),
                        p.local,
                    ))
                }
                _ => None,
            })
            .collect()
    }

    fn owned(v: &[(&str, &str, bool)]) -> Vec<(String, String, bool)> {
        v.iter()
            .map(|(n, val, l)| (n.to_string(), val.to_string(), *l))
            .collect()
    }

    #[test]
    fn localparam_list_keeps_every_name() {
        let src = "module t; localparam IDLE = 0, RUN = 1, DONE = 2; endmodule";
        let (m, interner, diags) = parse_full(src);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(m.items.len(), 3);
        assert_eq!(
            param_items(&m.items, &interner, src),
            owned(&[("IDLE", "0", true), ("RUN", "1", true), ("DONE", "2", true)])
        );
    }

    #[test]
    fn parameter_list_inherits_range_and_signed() {
        let src = "module t; parameter signed [7:0] A = 1, B = 2; parameter C = 3, [3:0] D = 4; endmodule";
        let (m, interner, diags) = parse_full(src);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            param_items(&m.items, &interner, src),
            owned(&[
                ("A", "1", false),
                ("B", "2", false),
                ("C", "3", false),
                ("D", "4", false)
            ])
        );
        let decl = |i: usize| match &m.items[i] {
            ModuleItem::ParameterDecl(p) => p.clone(),
            other => panic!("{other:?}"),
        };
        assert!(decl(1).signed && decl(1).range.is_some(), "B inherits");
        assert!(!decl(2).signed && decl(2).range.is_none());
        assert!(decl(3).range.is_some(), "D keeps its own range");
    }

    #[test]
    fn parameter_lists_in_generate_function_and_task() {
        let src = "module t;
            generate if (1) begin localparam A = 1, B = 2; end endgenerate
            function f; input x; localparam C = 3, D = 4; begin f = x; end endfunction
            task tk; localparam E = 5, F = 6; begin end endtask
            always @(*) begin : blk localparam G = 7, H = 8; end
        endmodule";
        let (m, interner, diags) = parse_full(src);
        assert!(diags.is_empty(), "{diags:?}");
        let names = |items: &[ModuleItem]| -> Vec<String> {
            param_items(items, &interner, src)
                .into_iter()
                .map(|(n, _, _)| n)
                .collect()
        };
        match &m.items[0] {
            ModuleItem::GenerateBlock(GenerateBlock::If { then_items, .. }) => {
                assert_eq!(names(then_items), ["A", "B"]);
            }
            other => panic!("{other:?}"),
        }
        match &m.items[1] {
            ModuleItem::FunctionDecl(f) => assert_eq!(names(&f.decls), ["C", "D"]),
            other => panic!("{other:?}"),
        }
        match &m.items[2] {
            ModuleItem::TaskDecl(t) => assert_eq!(names(&t.decls), ["E", "F"]),
            other => panic!("{other:?}"),
        }
        match &m.items[3] {
            ModuleItem::AlwaysBlock(ab) => match &ab.body {
                Statement::EventControl { body, .. } => match body.as_ref() {
                    Statement::Block { decls, .. } => assert_eq!(names(decls), ["G", "H"]),
                    other => panic!("{other:?}"),
                },
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parameter_list_missing_name_reports_error() {
        let (m, _, diags) = parse_full("module t; localparam A = 1, ; wire w; endmodule");
        assert!(!diags.is_empty());
        // Recovery keeps A and the following declaration.
        assert!(matches!(m.items[0], ModuleItem::LocalparamDecl(_)));
        assert!(m.items.iter().any(|i| matches!(i, ModuleItem::NetDecl(_))));
    }

    #[test]
    fn continuous_assign_delays() {
        let src = "module t; assign #2 y = a; assign #(1, 2) z = b; assign w = c; endmodule";
        let (m, _, diags) = parse_full(src);
        assert!(diags.is_empty(), "{diags:?}");
        let delays: Vec<usize> = m
            .items
            .iter()
            .map(|i| match i {
                ModuleItem::ContinuousAssign(ca) => ca.delay.as_ref().map_or(0, |d| d.values.len()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(delays, [1, 2, 0]);
    }

    #[test]
    fn delay_with_too_many_values_is_an_error() {
        let (m, _, diags) = parse_full("module t; assign #(1, 2, 3, 4) y = a; endmodule");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("at most 3"));
        match &m.items[0] {
            ModuleItem::ContinuousAssign(ca) => {
                assert_eq!(ca.delay.as_ref().unwrap().values.len(), 3)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn net_declaration_delays() {
        let src =
            "module t; wire #1 w; wire #1 v = a; wire [7:0] #(1, 2) bus; wire plain; endmodule";
        let (m, interner, diags) = parse_full(src);
        assert!(diags.is_empty(), "{diags:?}");
        let nets: Vec<_> = m
            .items
            .iter()
            .map(|i| match i {
                ModuleItem::NetDecl(n) => n.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(nets[0].delay.as_ref().unwrap().values.len(), 1);
        assert_eq!(interner.resolve(nets[0].names[0].name), "w");
        assert!(
            nets[1].names[0].init.is_some(),
            "`wire #1 v = a` keeps its init"
        );
        assert!(nets[2].range.is_some());
        assert_eq!(nets[2].delay.as_ref().unwrap().values.len(), 2);
        assert!(nets[3].delay.is_none());
    }

    #[test]
    fn gate_delay_and_multiple_instances() {
        let src = "module t; and #1 g1(y, a, b), g2(z, c, d); or o1(p, q, r); endmodule";
        let (m, interner, diags) = parse_full(src);
        assert!(diags.is_empty(), "{diags:?}");
        let gates: Vec<_> = m
            .items
            .iter()
            .map(|i| match i {
                ModuleItem::GateInst(g) => g.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(gates.len(), 3);
        assert_eq!(interner.resolve(gates[0].name.unwrap()), "g1");
        assert_eq!(interner.resolve(gates[1].name.unwrap()), "g2");
        assert_eq!(interner.resolve(gates[1].gate_type), "and");
        assert!(gates[0].delay.is_some() && gates[1].delay.is_some());
        assert_eq!(gates[1].ports.len(), 3);
        assert!(gates[2].delay.is_none());
    }

    #[test]
    fn gate_identifier_delay_without_instance_name() {
        // `#d (` must not be read as a call `d(...)`.
        let src = "module t; and #d (y, a, b); endmodule";
        let (m, interner, diags) = parse_full(src);
        assert!(diags.is_empty(), "{diags:?}");
        match &m.items[0] {
            ModuleItem::GateInst(g) => {
                assert!(g.name.is_none());
                assert_eq!(g.ports.len(), 3);
                match &g.delay.as_ref().unwrap().values[0] {
                    Expr::Identifier { name, .. } => assert_eq!(interner.resolve(*name), "d"),
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gate_instance_list_missing_paren_reports_error() {
        let (_, _, diags) = parse_full("module t; and g1(y, a, b), g2; endmodule");
        assert!(!diags.is_empty());
    }
}
