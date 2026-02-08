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
            if let Some(item) = self.parse_module_item_inner() {
                items.push(item);
            }
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

    /// Parses a single module item.
    pub(crate) fn parse_module_item_inner(&mut self) -> Option<ModuleItem> {
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

            // Parameter
            VerilogToken::Parameter => Some(self.parse_parameter_item(false)),

            // Localparam
            VerilogToken::Localparam => Some(self.parse_parameter_item(true)),

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

            // Gate primitives: and, or, nand, nor, xor, xnor, not, buf
            VerilogToken::And
            | VerilogToken::Or
            | VerilogToken::Nand
            | VerilogToken::Nor
            | VerilogToken::Xor
            | VerilogToken::Xnor
            | VerilogToken::Not
            | VerilogToken::Buf => Some(self.parse_gate_instantiation()),

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

        let names = self.parse_decl_name_list();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::NetDecl(NetDecl {
            net_type,
            signed,
            range,
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

    /// Parses a parameter or localparam item in a module body.
    fn parse_parameter_item(&mut self, local: bool) -> ModuleItem {
        let start = self.current_span();
        let param = self.parse_single_parameter_decl(local);
        // Handle comma-separated parameters
        let mut params = vec![param];
        while self.eat(VerilogToken::Comma) {
            let p_start = self.current_span();
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
            let span = p_start.merge(self.prev_span());
            params.push(ParameterDecl {
                local,
                signed,
                range,
                name,
                value,
                span,
            });
        }
        self.expect(VerilogToken::Semicolon);
        let _span = start.merge(self.prev_span());

        if local {
            // Return all as separate items (only first for now, rest appended)
            // For simplicity, return the first one
            ModuleItem::LocalparamDecl(params.into_iter().next().unwrap())
        } else {
            ModuleItem::ParameterDecl(params.into_iter().next().unwrap())
        }
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
        let target = self.parse_expr();
        self.expect(VerilogToken::Equals);
        let value = self.parse_expr();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::ContinuousAssign(ContinuousAssign {
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

    /// Parses a gate primitive instantiation.
    fn parse_gate_instantiation(&mut self) -> ModuleItem {
        let start = self.current_span();
        let text = self.current_text();
        let gate_type = self.interner.get_or_intern(text);
        self.advance();

        // Optional instance name
        let name = if self.at(VerilogToken::Identifier) && self.peek_is(VerilogToken::LeftParen) {
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
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::GateInst(GateInst {
            gate_type,
            name,
            ports,
            span,
        })
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
                if let Some(item) = self.parse_module_item_inner() {
                    items.push(item);
                }
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
                if let Some(item) = self.parse_module_item_inner() {
                    items.push(item);
                }
            }
            self.expect(VerilogToken::End);
        } else {
            // Single item after for (...)
            // We already didn't see begin, so check if there are items before endgenerate
            while !self.at(VerilogToken::Endgenerate) && !self.at_eof() {
                if let Some(item) = self.parse_module_item_inner() {
                    items.push(item);
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
                if let Some(item) = self.parse_module_item_inner() {
                    items.push(item);
                }
            }
            self.expect(VerilogToken::End);
        } else if let Some(item) = self.parse_module_item_inner() {
            items.push(item);
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
                if let Some(item) = self.parse_module_item_inner() {
                    decls.push(item);
                }
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
                if let Some(item) = self.parse_module_item_inner() {
                    decls.push(item);
                }
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
}
