//! Declaration and module item parsing for SystemVerilog-2017.
//!
//! Handles net/reg/variable declarations, parameter/localparam, continuous
//! assign, all always-block variants (always, always_comb, always_ff, always_latch),
//! initial blocks, module instantiation, generate blocks, function/task
//! declarations (with SV return types), gate primitives, genvar/defparam,
//! typedef/enum/struct, import statements, modport declarations, and
//! immediate assertions.
//!
//! **Instantiation detection:** When an identifier appears at module-item level,
//! the parser peeks at the next token — if it's also an identifier or `#`,
//! this is a module instantiation rather than a declaration.

use crate::ast::*;
use crate::parser::SvParser;
use crate::token::SvToken;
use aion_source::Span;

impl SvParser<'_> {
    /// Parses module items until `endmodule` or EOF.
    pub fn parse_module_items(&mut self) -> Vec<ModuleItem> {
        let mut items = Vec::new();
        while !self.at(SvToken::Endmodule) && !self.at_eof() {
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
            SvToken::Wire
                | SvToken::Tri
                | SvToken::Supply0
                | SvToken::Supply1
                | SvToken::Reg
                | SvToken::Integer
                | SvToken::Real
                | SvToken::Logic
                | SvToken::Bit
                | SvToken::Byte
                | SvToken::Shortint
                | SvToken::Int
                | SvToken::Longint
                | SvToken::Parameter
                | SvToken::Localparam
                | SvToken::Input
                | SvToken::Output
                | SvToken::Inout
                | SvToken::Genvar
                | SvToken::Typedef
                | SvToken::Enum
                | SvToken::Import
        )
    }

    /// Parses a single module item.
    pub(crate) fn parse_module_item_inner(&mut self) -> Option<ModuleItem> {
        match self.current() {
            // Net declarations
            SvToken::Wire | SvToken::Tri | SvToken::Supply0 | SvToken::Supply1 => {
                Some(self.parse_net_declaration())
            }

            // Reg declaration
            SvToken::Reg => Some(self.parse_reg_declaration()),

            // SV variable declarations (logic, bit, byte, int, etc.)
            SvToken::Logic
            | SvToken::Bit
            | SvToken::Byte
            | SvToken::Shortint
            | SvToken::Int
            | SvToken::Longint => Some(self.parse_var_declaration()),

            // Integer declaration
            SvToken::Integer => Some(self.parse_integer_declaration()),

            // Real declaration
            SvToken::Real => Some(self.parse_real_declaration()),

            // Parameter
            SvToken::Parameter => Some(self.parse_parameter_item(false)),

            // Localparam
            SvToken::Localparam => Some(self.parse_parameter_item(true)),

            // Port declarations (non-ANSI style)
            SvToken::Input | SvToken::Output | SvToken::Inout => {
                Some(self.parse_port_declaration())
            }

            // Continuous assignment
            SvToken::Assign => Some(self.parse_continuous_assign()),

            // Verilog-2005 always block
            SvToken::Always => Some(self.parse_always_block()),

            // SystemVerilog always_comb
            SvToken::AlwaysComb => Some(self.parse_always_comb()),

            // SystemVerilog always_ff
            SvToken::AlwaysFf => Some(self.parse_always_ff()),

            // SystemVerilog always_latch
            SvToken::AlwaysLatch => Some(self.parse_always_latch()),

            // Initial block
            SvToken::Initial => Some(self.parse_initial_block()),

            // Generate block
            SvToken::Generate => Some(self.parse_generate_block()),

            // Genvar
            SvToken::Genvar => Some(self.parse_genvar_declaration()),

            // Function
            SvToken::Function => Some(self.parse_function_declaration()),

            // Task
            SvToken::Task => Some(self.parse_task_declaration()),

            // Defparam
            SvToken::Defparam => Some(self.parse_defparam()),

            // Typedef
            SvToken::Typedef => Some(self.parse_typedef_item()),

            // Enum used as a variable declaration
            SvToken::Enum => Some(self.parse_typedef_or_enum_var()),

            // Struct used as a variable declaration
            SvToken::Struct => Some(self.parse_struct_var()),

            // Import
            SvToken::Import => Some(self.parse_import_item()),

            // Immediate assertions
            SvToken::Assert | SvToken::Assume | SvToken::Cover => Some(self.parse_assertion_item()),

            // Modport (inside interfaces)
            SvToken::Modport => Some(self.parse_modport_declaration()),

            // Gate primitives
            SvToken::And
            | SvToken::Or
            | SvToken::Nand
            | SvToken::Nor
            | SvToken::Xor
            | SvToken::Xnor
            | SvToken::Not
            | SvToken::Buf => Some(self.parse_gate_instantiation()),

            // Identifier: could be module instantiation or named-type variable
            SvToken::Identifier | SvToken::EscapedIdentifier => {
                let next = self.peek_kind(1);
                if next == SvToken::Hash {
                    // module_name #(...) — always instantiation
                    Some(self.parse_module_instantiation())
                } else if next == SvToken::Identifier || next == SvToken::EscapedIdentifier {
                    // type_name var_name ... — could be instantiation or named-type var
                    // Peek further: if third token is `(`, it's instantiation; otherwise var
                    let third = self.peek_kind(2);
                    if third == SvToken::LeftParen {
                        Some(self.parse_module_instantiation())
                    } else {
                        Some(self.parse_named_type_var())
                    }
                } else if next == SvToken::ColonColon {
                    // pkg::type_name var_name — scoped type variable
                    Some(self.parse_scoped_type_var())
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

    // ========================================================================
    // Net / Reg / Variable declarations
    // ========================================================================

    /// Parses a net declaration (wire, tri, supply0, supply1).
    fn parse_net_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        let net_type = self.eat_net_type().expect("should be at net type");
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let names = self.parse_decl_name_list();
        self.expect(SvToken::Semicolon);
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
        self.expect(SvToken::Reg);
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let names = self.parse_decl_name_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::RegDecl(RegDecl {
            signed,
            range,
            names,
            span,
        })
    }

    /// Parses a SystemVerilog variable declaration (logic, bit, byte, int, etc.).
    pub(crate) fn parse_var_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        let var_type = self.eat_var_type().expect("should be at var type");
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let names = self.parse_decl_name_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::VarDecl(VarDecl {
            var_type,
            signed,
            range,
            names,
            span,
        })
    }

    /// Parses an integer variable declaration.
    fn parse_integer_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Integer);
        let names = self.parse_decl_name_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::IntegerDecl(IntegerDecl { names, span })
    }

    /// Parses a real variable declaration.
    fn parse_real_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Real);
        let names = self.parse_decl_name_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::RealDecl(RealDecl { names, span })
    }

    /// Parses a variable declaration using a named type (e.g., `state_t state;`).
    fn parse_named_type_var(&mut self) -> ModuleItem {
        let start = self.current_span();
        let type_name = self.expect_ident();
        let names = self.parse_decl_name_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::TypedefDecl(TypedefDecl {
            type_spec: TypeSpec::Named(type_name),
            signed: false,
            range: None,
            name: names[0].name,
            span,
        })
    }

    /// Parses a variable declaration using a scoped type (e.g., `pkg::type_t var;`).
    fn parse_scoped_type_var(&mut self) -> ModuleItem {
        let start = self.current_span();
        let scope = self.expect_ident();
        self.expect(SvToken::ColonColon);
        let type_name = self.expect_ident();
        let names = self.parse_decl_name_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::TypedefDecl(TypedefDecl {
            type_spec: TypeSpec::Scoped {
                scope,
                name: type_name,
            },
            signed: false,
            range: None,
            name: names[0].name,
            span,
        })
    }

    /// Tries to consume a variable type keyword, returning the type if found.
    fn eat_var_type(&mut self) -> Option<VarType> {
        match self.current() {
            SvToken::Logic => {
                self.advance();
                Some(VarType::Logic)
            }
            SvToken::Bit => {
                self.advance();
                Some(VarType::Bit)
            }
            SvToken::Byte => {
                self.advance();
                Some(VarType::Byte)
            }
            SvToken::Shortint => {
                self.advance();
                Some(VarType::Shortint)
            }
            SvToken::Int => {
                self.advance();
                Some(VarType::Int)
            }
            SvToken::Longint => {
                self.advance();
                Some(VarType::Longint)
            }
            _ => None,
        }
    }

    // ========================================================================
    // Parameter / Localparam
    // ========================================================================

    /// Parses a parameter or localparam item in a module body.
    pub(crate) fn parse_parameter_item(&mut self, local: bool) -> ModuleItem {
        let start = self.current_span();
        let param = self.parse_single_parameter_decl(local);
        // Handle comma-separated parameters
        let mut params = vec![param];
        while self.eat(SvToken::Comma) {
            let p_start = self.current_span();
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
            let span = p_start.merge(self.prev_span());
            params.push(ParameterDecl {
                local,
                signed,
                type_spec,
                range,
                name,
                value,
                span,
            });
        }
        self.expect(SvToken::Semicolon);
        let _span = start.merge(self.prev_span());

        if local {
            ModuleItem::LocalparamDecl(params.into_iter().next().unwrap())
        } else {
            ModuleItem::ParameterDecl(params.into_iter().next().unwrap())
        }
    }

    // ========================================================================
    // Port declarations (non-ANSI style, in module body)
    // ========================================================================

    /// Parses a non-ANSI port declaration in the module body.
    fn parse_port_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        let direction = match self.current() {
            SvToken::Input => {
                self.advance();
                Direction::Input
            }
            SvToken::Output => {
                self.advance();
                Direction::Output
            }
            SvToken::Inout => {
                self.advance();
                Direction::Inout
            }
            _ => {
                self.error("expected port direction");
                Direction::Input
            }
        };

        let port_type = self.eat_port_type();
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let names = self.parse_identifier_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::PortDecl(SvPortDecl {
            direction,
            port_type,
            signed,
            range,
            names,
            span,
        })
    }

    // ========================================================================
    // Continuous assignment
    // ========================================================================

    /// Parses a continuous assignment (e.g., `assign y = a & b;`).
    fn parse_continuous_assign(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Assign);
        let target = self.parse_expr();
        self.expect(SvToken::Equals);
        let value = self.parse_expr();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::ContinuousAssign(ContinuousAssign {
            target,
            value,
            span,
        })
    }

    // ========================================================================
    // Always blocks (Verilog + SystemVerilog)
    // ========================================================================

    /// Parses a Verilog-2005 `always` block.
    fn parse_always_block(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Always);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        ModuleItem::AlwaysBlock(AlwaysBlock { body, span })
    }

    /// Parses an `always_comb` block.
    fn parse_always_comb(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::AlwaysComb);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        ModuleItem::AlwaysComb(AlwaysCombBlock { body, span })
    }

    /// Parses an `always_ff` block with sensitivity list.
    fn parse_always_ff(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::AlwaysFf);

        // Parse sensitivity list: @(posedge clk or negedge rst)
        let sensitivity = if self.at(SvToken::At) {
            self.advance(); // eat @
            if self.eat(SvToken::Star) {
                SensitivityList::Star
            } else if self.at(SvToken::LeftParen) {
                self.advance(); // eat (
                if self.at(SvToken::Star) {
                    self.advance();
                    self.expect(SvToken::RightParen);
                    SensitivityList::Star
                } else {
                    let list = self.parse_sensitivity_list();
                    self.expect(SvToken::RightParen);
                    list
                }
            } else {
                self.error("expected '(' or '*' after '@'");
                SensitivityList::Star
            }
        } else {
            SensitivityList::Star
        };

        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        ModuleItem::AlwaysFf(AlwaysFfBlock {
            sensitivity,
            body,
            span,
        })
    }

    /// Parses an `always_latch` block.
    fn parse_always_latch(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::AlwaysLatch);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        ModuleItem::AlwaysLatch(AlwaysLatchBlock { body, span })
    }

    // ========================================================================
    // Initial block
    // ========================================================================

    /// Parses an `initial` block.
    fn parse_initial_block(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Initial);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        ModuleItem::InitialBlock(InitialBlock { body, span })
    }

    // ========================================================================
    // Typedef / Enum / Struct
    // ========================================================================

    /// Parses a typedef declaration.
    pub(crate) fn parse_typedef_item(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Typedef);

        let type_spec = self.parse_type_spec();
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let name = self.expect_ident();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::TypedefDecl(TypedefDecl {
            type_spec,
            signed,
            range,
            name,
            span,
        })
    }

    /// Parses an enum used as a variable declaration (not typedef).
    pub(crate) fn parse_typedef_or_enum_var(&mut self) -> ModuleItem {
        let start = self.current_span();
        let type_spec = TypeSpec::Enum(self.parse_enum_type());
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        // Check if this is followed by a name (variable declaration) or semicolon
        if self.at(SvToken::Identifier) || self.at(SvToken::EscapedIdentifier) {
            let names = self.parse_decl_name_list();
            self.expect(SvToken::Semicolon);
            let span = start.merge(self.prev_span());
            // Wrap as a typedef-like declaration using VarDecl
            // But we need a specific type — use the enum as a VarDecl with embedded type
            ModuleItem::TypedefDecl(TypedefDecl {
                type_spec,
                signed,
                range,
                name: names[0].name,
                span,
            })
        } else {
            self.expect(SvToken::Semicolon);
            let span = start.merge(self.prev_span());
            ModuleItem::Error(span)
        }
    }

    /// Parses a struct used as a variable declaration.
    fn parse_struct_var(&mut self) -> ModuleItem {
        let start = self.current_span();
        let type_spec = TypeSpec::Struct(self.parse_struct_type());
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        if self.at(SvToken::Identifier) || self.at(SvToken::EscapedIdentifier) {
            let names = self.parse_decl_name_list();
            self.expect(SvToken::Semicolon);
            let span = start.merge(self.prev_span());
            ModuleItem::TypedefDecl(TypedefDecl {
                type_spec,
                signed,
                range,
                name: names[0].name,
                span,
            })
        } else {
            self.expect(SvToken::Semicolon);
            let span = start.merge(self.prev_span());
            ModuleItem::Error(span)
        }
    }

    /// Parses a type specification used in typedef and declarations.
    fn parse_type_spec(&mut self) -> TypeSpec {
        match self.current() {
            SvToken::Logic => {
                self.advance();
                TypeSpec::Simple(VarType::Logic)
            }
            SvToken::Bit => {
                self.advance();
                TypeSpec::Simple(VarType::Bit)
            }
            SvToken::Byte => {
                self.advance();
                TypeSpec::Simple(VarType::Byte)
            }
            SvToken::Shortint => {
                self.advance();
                TypeSpec::Simple(VarType::Shortint)
            }
            SvToken::Int => {
                self.advance();
                TypeSpec::Simple(VarType::Int)
            }
            SvToken::Longint => {
                self.advance();
                TypeSpec::Simple(VarType::Longint)
            }
            SvToken::Integer => {
                self.advance();
                TypeSpec::Simple(VarType::Integer)
            }
            SvToken::Real => {
                self.advance();
                TypeSpec::Simple(VarType::Real)
            }
            SvToken::Reg => {
                self.advance();
                TypeSpec::Simple(VarType::Reg)
            }
            SvToken::Enum => TypeSpec::Enum(self.parse_enum_type()),
            SvToken::Struct => TypeSpec::Struct(self.parse_struct_type()),
            SvToken::Identifier | SvToken::EscapedIdentifier => {
                let name = self.expect_ident();
                // Check for scoped type: ident::ident
                if self.at(SvToken::ColonColon) {
                    self.advance();
                    let type_name = self.expect_ident();
                    TypeSpec::Scoped {
                        scope: name,
                        name: type_name,
                    }
                } else {
                    TypeSpec::Named(name)
                }
            }
            _ => {
                self.error("expected type specification");
                TypeSpec::Simple(VarType::Logic)
            }
        }
    }

    /// Parses an enum type: `enum [base_type [range]] { members }`.
    fn parse_enum_type(&mut self) -> EnumDecl {
        let start = self.current_span();
        self.expect(SvToken::Enum);

        // Optional base type
        let base_type = match self.current() {
            SvToken::Logic => {
                self.advance();
                Some(VarType::Logic)
            }
            SvToken::Bit => {
                self.advance();
                Some(VarType::Bit)
            }
            SvToken::Int => {
                self.advance();
                Some(VarType::Int)
            }
            SvToken::Integer => {
                self.advance();
                Some(VarType::Integer)
            }
            SvToken::Byte => {
                self.advance();
                Some(VarType::Byte)
            }
            SvToken::Shortint => {
                self.advance();
                Some(VarType::Shortint)
            }
            SvToken::Longint => {
                self.advance();
                Some(VarType::Longint)
            }
            SvToken::Reg => {
                self.advance();
                Some(VarType::Reg)
            }
            _ => None,
        };

        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        self.expect(SvToken::LeftBrace);
        let mut members = Vec::new();
        if !self.at(SvToken::RightBrace) {
            members.push(self.parse_enum_member());
            while self.eat(SvToken::Comma) {
                members.push(self.parse_enum_member());
            }
        }
        self.expect(SvToken::RightBrace);
        let span = start.merge(self.prev_span());

        EnumDecl {
            base_type,
            range,
            members,
            span,
        }
    }

    /// Parses a single enum member: `name [= value]`.
    fn parse_enum_member(&mut self) -> EnumMember {
        let start = self.current_span();
        let name = self.expect_ident();
        let value = if self.eat(SvToken::Equals) {
            Some(self.parse_expr())
        } else {
            None
        };
        let span = start.merge(self.prev_span());
        EnumMember { name, value, span }
    }

    /// Parses a struct type: `struct [packed [signed]] { members }`.
    fn parse_struct_type(&mut self) -> StructDecl {
        let start = self.current_span();
        self.expect(SvToken::Struct);
        let packed = self.eat(SvToken::Packed);
        let signed = self.eat(SvToken::Signed);

        self.expect(SvToken::LeftBrace);
        let mut members = Vec::new();
        while !self.at(SvToken::RightBrace) && !self.at_eof() {
            members.push(self.parse_struct_member());
        }
        self.expect(SvToken::RightBrace);
        let span = start.merge(self.prev_span());

        StructDecl {
            packed,
            signed,
            members,
            span,
        }
    }

    /// Parses a single struct member: `type [signed] [range] names;`.
    fn parse_struct_member(&mut self) -> StructMember {
        let start = self.current_span();
        let type_spec = self.parse_type_spec();
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };
        let names = self.parse_identifier_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        StructMember {
            type_spec,
            signed,
            range,
            names,
            span,
        }
    }

    // ========================================================================
    // Import
    // ========================================================================

    /// Parses an import statement: `import pkg::*;` or `import pkg::name;`.
    pub(crate) fn parse_import_item(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Import);
        let package = self.expect_ident();
        self.expect(SvToken::ColonColon);

        let name = if self.eat(SvToken::Star) {
            None
        } else {
            Some(self.expect_ident())
        };

        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::Import(SvImport {
            package,
            name,
            span,
        })
    }

    // ========================================================================
    // Immediate assertions
    // ========================================================================

    /// Parses an immediate assertion as a module item.
    fn parse_assertion_item(&mut self) -> ModuleItem {
        let start = self.current_span();
        let kind = match self.current() {
            SvToken::Assert => {
                self.advance();
                AssertionKind::Assert
            }
            SvToken::Assume => {
                self.advance();
                AssertionKind::Assume
            }
            SvToken::Cover => {
                self.advance();
                AssertionKind::Cover
            }
            _ => {
                self.error("expected assertion keyword");
                AssertionKind::Assert
            }
        };

        self.expect(SvToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(SvToken::RightParen);

        let pass_stmt = if !self.at(SvToken::Else) && !self.at(SvToken::Semicolon) {
            Some(Box::new(self.parse_statement()))
        } else if self.at(SvToken::Semicolon) && !self.peek_is(SvToken::Else) {
            self.advance();
            None
        } else {
            None
        };

        let fail_stmt = if self.eat(SvToken::Else) {
            Some(Box::new(self.parse_statement()))
        } else {
            None
        };

        let span = start.merge(self.prev_span());
        ModuleItem::Assertion(SvAssertion {
            kind,
            condition,
            pass_stmt,
            fail_stmt,
            span,
        })
    }

    // ========================================================================
    // Modport
    // ========================================================================

    /// Parses a modport declaration: `modport name(dir ports, ...);`.
    fn parse_modport_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Modport);
        let name = self.expect_ident();
        self.expect(SvToken::LeftParen);

        let mut ports = Vec::new();
        if !self.at(SvToken::RightParen) {
            ports.push(self.parse_modport_port());
            while self.eat(SvToken::Comma) {
                ports.push(self.parse_modport_port());
            }
        }

        self.expect(SvToken::RightParen);
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::ModportDecl(SvModportDecl { name, ports, span })
    }

    /// Parses a single modport port group: `direction name {, name}`.
    fn parse_modport_port(&mut self) -> SvModportPort {
        let start = self.current_span();
        let direction = match self.current() {
            SvToken::Input => {
                self.advance();
                Direction::Input
            }
            SvToken::Output => {
                self.advance();
                Direction::Output
            }
            SvToken::Inout => {
                self.advance();
                Direction::Inout
            }
            _ => {
                self.error("expected direction in modport");
                Direction::Input
            }
        };

        let mut names = Vec::new();
        names.push(self.expect_ident());
        // Collect additional names at same direction until we hit a new direction or )
        while self.at(SvToken::Comma) {
            let next = self.peek_kind(1);
            if next == SvToken::Input || next == SvToken::Output || next == SvToken::Inout {
                break;
            }
            self.advance(); // eat comma
            names.push(self.expect_ident());
        }

        let span = start.merge(self.prev_span());
        SvModportPort {
            direction,
            names,
            span,
        }
    }

    // ========================================================================
    // Module instantiation
    // ========================================================================

    /// Parses a module instantiation (e.g., `counter #(.W(8)) u1 (.clk(clk));`).
    fn parse_module_instantiation(&mut self) -> ModuleItem {
        let start = self.current_span();
        let module_name = self.expect_ident();

        // Optional parameter overrides: #(...)
        let param_overrides = if self.at(SvToken::Hash) {
            self.advance();
            self.parse_connection_list()
        } else {
            Vec::new()
        };

        // Parse instances
        let mut instances = Vec::new();
        loop {
            let inst = self.parse_single_instance();
            instances.push(inst);
            if !self.eat(SvToken::Comma) {
                break;
            }
        }

        self.expect(SvToken::Semicolon);
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
        let range = if self.at(SvToken::LeftBracket) {
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
        self.expect(SvToken::LeftParen);
        let mut connections = Vec::new();
        if !self.at(SvToken::RightParen) {
            loop {
                connections.push(self.parse_connection());
                if !self.eat(SvToken::Comma) {
                    break;
                }
            }
        }
        self.expect(SvToken::RightParen);
        connections
    }

    /// Parses a single connection (named or positional).
    fn parse_connection(&mut self) -> Connection {
        let start = self.current_span();
        if self.at(SvToken::Dot) {
            self.advance();
            let formal = self.expect_ident();
            self.expect(SvToken::LeftParen);
            let actual = if self.at(SvToken::RightParen) {
                None
            } else {
                Some(self.parse_expr())
            };
            self.expect(SvToken::RightParen);
            let span = start.merge(self.prev_span());
            Connection {
                formal: Some(formal),
                actual,
                span,
            }
        } else {
            let actual = self.parse_expr();
            let span = start.merge(actual.span());
            Connection {
                formal: None,
                actual: Some(actual),
                span,
            }
        }
    }

    // ========================================================================
    // Gate primitives
    // ========================================================================

    /// Parses a gate primitive instantiation (e.g., `and g1(y, a, b);`).
    fn parse_gate_instantiation(&mut self) -> ModuleItem {
        let start = self.current_span();
        let text = self.current_text();
        let gate_type = self.interner.get_or_intern(text);
        self.advance();

        let name = if self.at(SvToken::Identifier) && self.peek_is(SvToken::LeftParen) {
            Some(self.expect_ident())
        } else {
            None
        };

        self.expect(SvToken::LeftParen);
        let mut ports = Vec::new();
        if !self.at(SvToken::RightParen) {
            ports.push(self.parse_expr());
            while self.eat(SvToken::Comma) {
                ports.push(self.parse_expr());
            }
        }
        self.expect(SvToken::RightParen);
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        ModuleItem::GateInst(GateInst {
            gate_type,
            name,
            ports,
            span,
        })
    }

    // ========================================================================
    // Generate blocks
    // ========================================================================

    /// Parses a generate block.
    fn parse_generate_block(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Generate);

        let block = if self.at(SvToken::For) {
            self.parse_generate_for(start)
        } else if self.at(SvToken::If) {
            self.parse_generate_if(start)
        } else {
            // Generate wrapper with items
            let mut items = Vec::new();
            while !self.at(SvToken::Endgenerate) && !self.at_eof() {
                if let Some(item) = self.parse_module_item_inner() {
                    items.push(item);
                }
            }
            self.expect(SvToken::Endgenerate);
            let span = start.merge(self.prev_span());
            return ModuleItem::GenerateBlock(GenerateBlock::If {
                condition: Expr::Literal { span },
                then_items: items,
                else_items: Vec::new(),
                span,
            });
        };

        self.expect(SvToken::Endgenerate);
        ModuleItem::GenerateBlock(block)
    }

    /// Parses a generate-for loop.
    fn parse_generate_for(&mut self, start: Span) -> GenerateBlock {
        self.expect(SvToken::For);
        self.expect(SvToken::LeftParen);

        let init = Box::new(self.parse_blocking_assignment_stmt());
        let condition = self.parse_expr();
        self.expect(SvToken::Semicolon);
        let step = Box::new(self.parse_blocking_assignment_no_semi());
        self.expect(SvToken::RightParen);

        let label = if self.eat(SvToken::Begin) {
            if self.eat(SvToken::Colon) {
                Some(self.expect_ident())
            } else {
                None
            }
        } else {
            None
        };

        let mut items = Vec::new();
        let has_begin = label.is_some() || self.at(SvToken::End);
        if has_begin {
            while !self.at(SvToken::End) && !self.at_eof() {
                if let Some(item) = self.parse_module_item_inner() {
                    items.push(item);
                }
            }
            self.expect(SvToken::End);
        } else {
            while !self.at(SvToken::Endgenerate) && !self.at_eof() {
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
        self.expect(SvToken::If);
        self.expect(SvToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(SvToken::RightParen);

        let then_items = self.parse_generate_body();

        let else_items = if self.eat(SvToken::Else) {
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
        if self.eat(SvToken::Begin) {
            if self.eat(SvToken::Colon) {
                let _ = self.expect_ident();
            }
            while !self.at(SvToken::End) && !self.at_eof() {
                if let Some(item) = self.parse_module_item_inner() {
                    items.push(item);
                }
            }
            self.expect(SvToken::End);
        } else if let Some(item) = self.parse_module_item_inner() {
            items.push(item);
        }
        items
    }

    // ========================================================================
    // Genvar / Defparam
    // ========================================================================

    /// Parses a genvar declaration.
    fn parse_genvar_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Genvar);
        let names = self.parse_identifier_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::GenvarDecl(GenvarDecl { names, span })
    }

    /// Parses a defparam statement.
    fn parse_defparam(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Defparam);
        let target = self.parse_expr();
        self.expect(SvToken::Equals);
        let value = self.parse_expr();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        ModuleItem::DefparamDecl(DefparamDecl {
            target,
            value,
            span,
        })
    }

    // ========================================================================
    // Function / Task (extended with SV return types)
    // ========================================================================

    /// Parses a function declaration with optional SV return type.
    pub(crate) fn parse_function_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Function);

        let automatic = self.eat(SvToken::Automatic);

        // Optional return type (SV extension)
        let return_type = if self.at(SvToken::Void) {
            self.advance();
            Some(TypeSpec::Simple(VarType::Logic)) // void mapped to Logic placeholder
        } else {
            self.try_parse_simple_type_spec()
        };

        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let name = self.expect_ident();

        // Function ports can be in parentheses (ANSI-style) or after semicolon
        let mut inputs = Vec::new();
        let mut decls = Vec::new();

        if self.at(SvToken::LeftParen) {
            // ANSI-style ports
            self.advance();
            if !self.at(SvToken::RightParen) {
                loop {
                    let port = self.parse_port_decl_in_subprogram();
                    inputs.push(port);
                    if !self.eat(SvToken::Comma) {
                        break;
                    }
                }
            }
            self.expect(SvToken::RightParen);
            self.expect(SvToken::Semicolon);
        } else {
            self.expect(SvToken::Semicolon);

            // Non-ANSI: ports and declarations before body
            while !self.at(SvToken::Begin)
                && !self.at(SvToken::Endfunction)
                && !self.at_eof()
                && !self.at(SvToken::Return)
            {
                if self.at(SvToken::Input) || self.at(SvToken::Output) || self.at(SvToken::Inout) {
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
        }

        // Body statements
        let mut body = Vec::new();
        if self.at(SvToken::Begin) {
            let block = self.parse_statement();
            body.push(block);
        } else {
            while !self.at(SvToken::Endfunction) && !self.at_eof() {
                body.push(self.parse_statement());
            }
        }

        self.expect(SvToken::Endfunction);
        let end_label = self.parse_end_label();
        let span = start.merge(self.prev_span());

        ModuleItem::FunctionDecl(FunctionDecl {
            automatic,
            return_type,
            signed,
            range,
            name,
            inputs,
            decls,
            body,
            end_label,
            span,
        })
    }

    /// Parses a task declaration with SV extensions.
    pub(crate) fn parse_task_declaration(&mut self) -> ModuleItem {
        let start = self.current_span();
        self.expect(SvToken::Task);

        let automatic = self.eat(SvToken::Automatic);
        let name = self.expect_ident();

        let mut ports = Vec::new();
        let mut decls = Vec::new();

        if self.at(SvToken::LeftParen) {
            // ANSI-style ports
            self.advance();
            if !self.at(SvToken::RightParen) {
                loop {
                    let port = self.parse_port_decl_in_subprogram();
                    ports.push(port);
                    if !self.eat(SvToken::Comma) {
                        break;
                    }
                }
            }
            self.expect(SvToken::RightParen);
            self.expect(SvToken::Semicolon);
        } else {
            self.expect(SvToken::Semicolon);

            while !self.at(SvToken::Begin) && !self.at(SvToken::Endtask) && !self.at_eof() {
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
        }

        let mut body = Vec::new();
        while !self.at(SvToken::Endtask) && !self.at_eof() {
            body.push(self.parse_statement());
        }

        self.expect(SvToken::Endtask);
        let end_label = self.parse_end_label();
        let span = start.merge(self.prev_span());

        ModuleItem::TaskDecl(TaskDecl {
            automatic,
            name,
            ports,
            decls,
            body,
            end_label,
            span,
        })
    }

    /// Parses a port declaration inside a function or task.
    ///
    /// In ANSI style (`function f(input int a, input int b)`), each port has
    /// a single name. In non-ANSI style, ports are followed by a semicolon.
    fn parse_port_decl_in_subprogram(&mut self) -> SvPortDecl {
        let start = self.current_span();
        let direction = match self.current() {
            SvToken::Input => {
                self.advance();
                Direction::Input
            }
            SvToken::Output => {
                self.advance();
                Direction::Output
            }
            SvToken::Inout => {
                self.advance();
                Direction::Inout
            }
            _ => {
                self.error("expected port direction");
                Direction::Input
            }
        };

        let port_type = self.eat_port_type();
        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        // In non-ANSI style (followed by ;), parse multiple names
        // In ANSI style, parse just one name (comma handled by caller)
        let name = self.expect_ident();
        let mut names = vec![name];

        // If we see a semicolon, this is non-ANSI style — parse all names
        if self.at(SvToken::Semicolon) {
            self.advance();
        } else {
            // ANSI style: check if comma leads to another identifier (same direction)
            while self.at(SvToken::Comma) {
                let next = self.peek_kind(1);
                // If next token is a direction keyword or type keyword, new port declaration
                if next == SvToken::Input
                    || next == SvToken::Output
                    || next == SvToken::Inout
                    || next == SvToken::RightParen
                {
                    break;
                }
                // If next is a type keyword, it's also a new port
                if matches!(
                    next,
                    SvToken::Logic
                        | SvToken::Bit
                        | SvToken::Byte
                        | SvToken::Int
                        | SvToken::Shortint
                        | SvToken::Longint
                        | SvToken::Integer
                        | SvToken::Real
                        | SvToken::Reg
                        | SvToken::Wire
                ) {
                    break;
                }
                self.advance(); // eat comma
                names.push(self.expect_ident());
            }
        }
        let span = start.merge(self.prev_span());

        SvPortDecl {
            direction,
            port_type,
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
    use crate::parser::SvParser;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::FileId;

    fn parse_module(source: &str) -> SvModuleDecl {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = SvParser::new(tokens, source, file, &interner, &sink);
        let ast = parser.parse_source_file();
        assert!(
            !sink.has_errors(),
            "unexpected errors: {:?}",
            sink.diagnostics()
        );
        match ast.items.into_iter().next().unwrap() {
            SvItem::Module(m) => m,
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
    fn logic_var_declaration() {
        let m = parse_module("module t; logic [7:0] data; endmodule");
        assert!(matches!(m.items[0], ModuleItem::VarDecl(_)));
        if let ModuleItem::VarDecl(ref v) = m.items[0] {
            assert_eq!(v.var_type, VarType::Logic);
            assert!(v.range.is_some());
        }
    }

    #[test]
    fn int_var_declaration() {
        let m = parse_module("module t; int count; endmodule");
        assert!(matches!(m.items[0], ModuleItem::VarDecl(_)));
        if let ModuleItem::VarDecl(ref v) = m.items[0] {
            assert_eq!(v.var_type, VarType::Int);
        }
    }

    #[test]
    fn bit_var_declaration() {
        let m = parse_module("module t; bit [3:0] nibble; endmodule");
        assert!(matches!(m.items[0], ModuleItem::VarDecl(_)));
        if let ModuleItem::VarDecl(ref v) = m.items[0] {
            assert_eq!(v.var_type, VarType::Bit);
            assert!(v.range.is_some());
        }
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
    fn parameter_with_type() {
        let m = parse_module("module t; parameter int WIDTH = 8; endmodule");
        assert!(matches!(m.items[0], ModuleItem::ParameterDecl(_)));
        if let ModuleItem::ParameterDecl(ref p) = m.items[0] {
            assert!(p.type_spec.is_some());
        }
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
    fn always_comb_block() {
        let m = parse_module("module t; always_comb begin y = a & b; end endmodule");
        assert!(matches!(m.items[0], ModuleItem::AlwaysComb(_)));
    }

    #[test]
    fn always_ff_block() {
        let m = parse_module(
            "module t; always_ff @(posedge clk or negedge rst)
                if (!rst) q <= 0; else q <= d;
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::AlwaysFf(_)));
        if let ModuleItem::AlwaysFf(ref ab) = m.items[0] {
            match &ab.sensitivity {
                SensitivityList::List(items) => assert_eq!(items.len(), 2),
                _ => panic!("expected list"),
            }
        }
    }

    #[test]
    fn always_latch_block() {
        let m = parse_module(
            "module t; always_latch
                if (en) q <= d;
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::AlwaysLatch(_)));
    }

    #[test]
    fn initial_block() {
        let m = parse_module("module t; initial begin clk = 0; end endmodule");
        assert!(matches!(m.items[0], ModuleItem::InitialBlock(_)));
    }

    #[test]
    fn typedef_logic() {
        let m = parse_module("module t; typedef logic [7:0] byte_t; endmodule");
        assert!(matches!(m.items[0], ModuleItem::TypedefDecl(_)));
        if let ModuleItem::TypedefDecl(ref td) = m.items[0] {
            assert!(td.range.is_some());
        }
    }

    #[test]
    fn typedef_enum() {
        let m = parse_module(
            "module t;
                typedef enum logic [1:0] {IDLE, RUN, STOP} state_t;
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::TypedefDecl(_)));
        if let ModuleItem::TypedefDecl(ref td) = m.items[0] {
            assert!(matches!(td.type_spec, TypeSpec::Enum(_)));
        }
    }

    #[test]
    fn typedef_struct_packed() {
        let m = parse_module(
            "module t;
                typedef struct packed {
                    logic [7:0] data;
                    logic valid;
                } packet_t;
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::TypedefDecl(_)));
        if let ModuleItem::TypedefDecl(ref td) = m.items[0] {
            if let TypeSpec::Struct(ref s) = td.type_spec {
                assert!(s.packed);
                assert_eq!(s.members.len(), 2);
            } else {
                panic!("expected struct type");
            }
        }
    }

    #[test]
    fn enum_variable_decl() {
        let m = parse_module(
            "module t;
                enum logic [1:0] {IDLE, RUN, STOP} state;
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::TypedefDecl(_)));
    }

    #[test]
    fn import_wildcard() {
        let m = parse_module("module t; import my_pkg::*; endmodule");
        assert!(matches!(m.items[0], ModuleItem::Import(_)));
        if let ModuleItem::Import(ref i) = m.items[0] {
            assert!(i.name.is_none());
        }
    }

    #[test]
    fn import_named() {
        let m = parse_module("module t; import my_pkg::WIDTH; endmodule");
        assert!(matches!(m.items[0], ModuleItem::Import(_)));
        if let ModuleItem::Import(ref i) = m.items[0] {
            assert!(i.name.is_some());
        }
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

    #[test]
    fn function_with_return_type() {
        let m = parse_module(
            "module t;
                function int add(input int a, input int b);
                    return a + b;
                endfunction
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::FunctionDecl(_)));
        if let ModuleItem::FunctionDecl(ref f) = m.items[0] {
            assert!(f.return_type.is_some());
            assert_eq!(f.inputs.len(), 2);
        }
    }

    #[test]
    fn function_verilog_style() {
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
    fn function_with_end_label() {
        let m = parse_module(
            "module t;
                function int add(input int a, input int b);
                    return a + b;
                endfunction : add
            endmodule",
        );
        if let ModuleItem::FunctionDecl(ref f) = m.items[0] {
            assert!(f.end_label.is_some());
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
    fn task_ansi_ports() {
        let m = parse_module(
            "module t;
                task automatic do_work(input int a, input int b);
                    begin
                        a = a + b;
                    end
                endtask
            endmodule",
        );
        if let ModuleItem::TaskDecl(ref t) = m.items[0] {
            assert!(t.automatic);
            assert_eq!(t.ports.len(), 2);
        }
    }

    #[test]
    fn task_with_end_label() {
        let m = parse_module(
            "module t;
                task do_work;
                    ;
                endtask : do_work
            endmodule",
        );
        if let ModuleItem::TaskDecl(ref t) = m.items[0] {
            assert!(t.end_label.is_some());
        }
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
    fn assertion_item() {
        let m = parse_module(
            "module t;
                assert (a == b);
            endmodule",
        );
        assert!(matches!(m.items[0], ModuleItem::Assertion(_)));
    }

    #[test]
    fn var_with_init() {
        let m = parse_module("module t; logic [7:0] data = 8'hFF; endmodule");
        if let ModuleItem::VarDecl(ref v) = m.items[0] {
            assert!(v.names[0].init.is_some());
        }
    }

    #[test]
    fn multiple_var_names() {
        let m = parse_module("module t; logic a, b, c; endmodule");
        if let ModuleItem::VarDecl(ref v) = m.items[0] {
            assert_eq!(v.names.len(), 3);
        }
    }
}
