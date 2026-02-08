//! Statement parsing for SystemVerilog-2017.
//!
//! Handles all Verilog-2005 statements plus SystemVerilog extensions: compound
//! assignments (`+=`, `-=`, etc.), increment/decrement (`++`, `--`),
//! `return`/`break`/`continue`, `do...while`, `foreach`, `unique if`/
//! `priority case`, and `for` with local variable declarations.
//!
//! **`<=` disambiguation:** Same strategy as Verilog parser — in statement context,
//! the LHS is parsed as a name expression first, then `<=` is treated as
//! non-blocking assignment.

use crate::ast::*;
use crate::parser::SvParser;
use crate::token::SvToken;

impl SvParser<'_> {
    /// Parses a single statement.
    pub fn parse_statement(&mut self) -> Statement {
        // Check for unique/priority modifier
        if self.at(SvToken::Unique) || self.at(SvToken::Priority) {
            let modifier = if self.at(SvToken::Unique) {
                self.advance();
                Some(CaseModifier::Unique)
            } else {
                self.advance();
                Some(CaseModifier::Priority)
            };

            return if self.at(SvToken::If) {
                self.parse_if_statement(modifier)
            } else if self.at(SvToken::Case) || self.at(SvToken::Casex) || self.at(SvToken::Casez) {
                self.parse_case_statement(modifier)
            } else {
                self.error("expected 'if' or 'case' after unique/priority");
                Statement::Error(self.current_span())
            };
        }

        match self.current() {
            // begin ... end block
            SvToken::Begin => self.parse_begin_end_block(),
            // if statement
            SvToken::If => self.parse_if_statement(None),
            // case/casex/casez
            SvToken::Case | SvToken::Casex | SvToken::Casez => self.parse_case_statement(None),
            // for loop
            SvToken::For => self.parse_for_statement(),
            // while loop
            SvToken::While => self.parse_while_statement(),
            // do ... while loop
            SvToken::Do => self.parse_do_while_statement(),
            // forever loop
            SvToken::Forever => self.parse_forever_statement(),
            // repeat loop
            SvToken::Repeat => self.parse_repeat_statement(),
            // foreach loop
            SvToken::Foreach => self.parse_foreach_statement(),
            // wait statement
            SvToken::Wait => self.parse_wait_statement(),
            // event control: @(...)
            SvToken::At => self.parse_event_control(),
            // delay control: #expr
            SvToken::Hash => self.parse_delay_control(),
            // disable statement
            SvToken::Disable => self.parse_disable_statement(),
            // return statement
            SvToken::Return => self.parse_return_statement(),
            // break statement
            SvToken::Break => {
                let span = self.current_span();
                self.advance();
                self.expect(SvToken::Semicolon);
                let span = span.merge(self.prev_span());
                Statement::Break { span }
            }
            // continue statement
            SvToken::Continue => {
                let span = self.current_span();
                self.advance();
                self.expect(SvToken::Semicolon);
                let span = span.merge(self.prev_span());
                Statement::Continue { span }
            }
            // system task call: $display(...)
            SvToken::SystemIdentifier => self.parse_system_task_call(),
            // null statement: ;
            SvToken::Semicolon => {
                let span = self.current_span();
                self.advance();
                Statement::Null { span }
            }
            // immediate assertions
            SvToken::Assert | SvToken::Assume | SvToken::Cover => {
                let assertion = self.parse_assertion_stmt();
                Statement::Assertion(assertion)
            }
            // Prefix ++ / --
            SvToken::PlusPlus | SvToken::MinusMinus => self.parse_incr_decr_statement(),
            // Local variable declaration in procedural context
            SvToken::Logic
            | SvToken::Bit
            | SvToken::Byte
            | SvToken::Shortint
            | SvToken::Int
            | SvToken::Longint => self.parse_local_var_decl(),
            // Assignment or task call (starts with identifier or concat)
            SvToken::Identifier | SvToken::EscapedIdentifier | SvToken::LeftBrace => {
                self.parse_assignment_or_task_call()
            }
            _ => {
                let span = self.current_span();
                self.error("expected statement");
                self.recover_to_semicolon();
                Statement::Error(span)
            }
        }
    }

    /// Parses a begin ... end block.
    fn parse_begin_end_block(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Begin);

        // Optional label: begin : label
        let label = if self.eat(SvToken::Colon) {
            Some(self.expect_ident())
        } else {
            None
        };

        let mut decls = Vec::new();
        let mut stmts = Vec::new();

        // In named blocks, declarations can appear before statements
        if label.is_some() {
            while !self.at(SvToken::End) && !self.at_eof() {
                if self.is_at_declaration_start() {
                    if let Some(item) = self.parse_module_item_inner() {
                        decls.push(item);
                    }
                } else {
                    break;
                }
            }
        }

        while !self.at(SvToken::End) && !self.at_eof() {
            stmts.push(self.parse_statement());
        }

        self.expect(SvToken::End);
        // Optional end label: end : label
        if self.at(SvToken::Colon) {
            self.advance();
            let _ = self.expect_ident();
        }
        let span = start.merge(self.prev_span());

        Statement::Block {
            label,
            decls,
            stmts,
            span,
        }
    }

    /// Parses an if statement with optional unique/priority modifier.
    fn parse_if_statement(&mut self, modifier: Option<CaseModifier>) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::If);
        self.expect(SvToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(SvToken::RightParen);

        let then_stmt = self.parse_statement();

        let else_stmt = if self.eat(SvToken::Else) {
            Some(Box::new(self.parse_statement()))
        } else {
            None
        };

        let end_span = if let Some(ref e) = else_stmt {
            match e.as_ref() {
                Statement::Error(s) => *s,
                _ => self.prev_span(),
            }
        } else {
            self.prev_span()
        };

        let span = start.merge(end_span);
        Statement::If {
            modifier,
            condition,
            then_stmt: Box::new(then_stmt),
            else_stmt,
            span,
        }
    }

    /// Parses a case/casex/casez statement with optional unique/priority modifier.
    fn parse_case_statement(&mut self, modifier: Option<CaseModifier>) -> Statement {
        let start = self.current_span();
        let kind = match self.current() {
            SvToken::Casex => {
                self.advance();
                CaseKind::Casex
            }
            SvToken::Casez => {
                self.advance();
                CaseKind::Casez
            }
            _ => {
                self.expect(SvToken::Case);
                CaseKind::Case
            }
        };

        self.expect(SvToken::LeftParen);
        let expr = self.parse_expr();
        self.expect(SvToken::RightParen);

        let mut arms = Vec::new();
        while !self.at(SvToken::Endcase) && !self.at_eof() {
            arms.push(self.parse_case_arm());
        }

        self.expect(SvToken::Endcase);
        let span = start.merge(self.prev_span());

        Statement::Case {
            modifier,
            kind,
            expr,
            arms,
            span,
        }
    }

    /// Parses a single case arm.
    fn parse_case_arm(&mut self) -> CaseArm {
        let start = self.current_span();

        if self.eat(SvToken::Default) {
            self.eat(SvToken::Colon);
            let body = self.parse_statement();
            let span = start.merge(self.prev_span());
            CaseArm {
                patterns: Vec::new(),
                is_default: true,
                body,
                span,
            }
        } else {
            let mut patterns = Vec::new();
            patterns.push(self.parse_expr());
            while self.eat(SvToken::Comma) {
                patterns.push(self.parse_expr());
            }
            self.expect(SvToken::Colon);
            let body = self.parse_statement();
            let span = start.merge(self.prev_span());
            CaseArm {
                patterns,
                is_default: false,
                body,
                span,
            }
        }
    }

    /// Parses a for loop (extended with optional `int` var declaration).
    fn parse_for_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::For);
        self.expect(SvToken::LeftParen);

        // Init: may be `int i = 0` or `i = 0`
        let init = if self.at(SvToken::Int)
            || self.at(SvToken::Integer)
            || self.at(SvToken::Logic)
            || self.at(SvToken::Bit)
        {
            // Local variable declaration
            self.parse_for_init_var_decl()
        } else {
            self.parse_blocking_assignment_stmt()
        };

        // Condition
        let condition = self.parse_expr();
        self.expect(SvToken::Semicolon);

        // Step: may be assignment, ++, --
        let step = self.parse_for_step();

        self.expect(SvToken::RightParen);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());

        Statement::For {
            init: Box::new(init),
            condition,
            step: Box::new(step),
            body: Box::new(body),
            span,
        }
    }

    /// Parses a for-loop init variable declaration (e.g., `int i = 0;`).
    fn parse_for_init_var_decl(&mut self) -> Statement {
        let start = self.current_span();
        let var_type = match self.current() {
            SvToken::Int => {
                self.advance();
                VarType::Int
            }
            SvToken::Integer => {
                self.advance();
                VarType::Integer
            }
            SvToken::Logic => {
                self.advance();
                VarType::Logic
            }
            SvToken::Bit => {
                self.advance();
                VarType::Bit
            }
            _ => {
                self.error("expected type");
                VarType::Int
            }
        };

        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let name_ident = self.expect_ident();
        self.expect(SvToken::Equals);
        let value = self.parse_expr();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        Statement::LocalVarDecl(VarDecl {
            var_type,
            signed,
            range,
            names: vec![DeclName {
                name: name_ident,
                dimensions: Vec::new(),
                init: Some(value),
                span,
            }],
            span,
        })
    }

    /// Parses the step part of a for loop (assignment, ++, or --).
    fn parse_for_step(&mut self) -> Statement {
        let start = self.current_span();

        // Handle prefix ++ and --
        if self.at(SvToken::PlusPlus) || self.at(SvToken::MinusMinus) {
            let increment = self.at(SvToken::PlusPlus);
            self.advance();
            let operand = self.parse_name_or_lvalue();
            let span = start.merge(self.prev_span());
            return Statement::IncrDecr {
                operand,
                increment,
                prefix: true,
                span,
            };
        }

        let target = self.parse_name_or_lvalue();

        // Check for postfix ++ and --
        if self.at(SvToken::PlusPlus) || self.at(SvToken::MinusMinus) {
            let increment = self.at(SvToken::PlusPlus);
            self.advance();
            let span = start.merge(self.prev_span());
            return Statement::IncrDecr {
                operand: target,
                increment,
                prefix: false,
                span,
            };
        }

        // Compound assignment
        if let Some(op) = self.eat_compound_op() {
            let value = self.parse_expr();
            let span = start.merge(self.prev_span());
            return Statement::CompoundAssign {
                target,
                op,
                value,
                span,
            };
        }

        // Regular assignment
        self.expect(SvToken::Equals);
        let value = self.parse_expr();
        let span = start.merge(self.prev_span());
        Statement::Blocking {
            target,
            value,
            span,
        }
    }

    /// Parses a blocking assignment as a statement (with semicolon).
    pub(crate) fn parse_blocking_assignment_stmt(&mut self) -> Statement {
        let start = self.current_span();
        let target = self.parse_expr();
        self.expect(SvToken::Equals);
        let value = self.parse_expr();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::Blocking {
            target,
            value,
            span,
        }
    }

    /// Parses a blocking assignment without trailing semicolon.
    pub(crate) fn parse_blocking_assignment_no_semi(&mut self) -> Statement {
        let start = self.current_span();
        let target = self.parse_expr();
        self.expect(SvToken::Equals);
        let value = self.parse_expr();
        let span = start.merge(self.prev_span());
        Statement::Blocking {
            target,
            value,
            span,
        }
    }

    /// Parses a while loop.
    fn parse_while_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::While);
        self.expect(SvToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(SvToken::RightParen);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        Statement::While {
            condition,
            body: Box::new(body),
            span,
        }
    }

    /// Parses a do ... while loop.
    fn parse_do_while_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Do);
        let body = self.parse_statement();
        self.expect(SvToken::While);
        self.expect(SvToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(SvToken::RightParen);
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::DoWhile {
            body: Box::new(body),
            condition,
            span,
        }
    }

    /// Parses a forever loop.
    fn parse_forever_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Forever);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        Statement::Forever {
            body: Box::new(body),
            span,
        }
    }

    /// Parses a repeat loop.
    fn parse_repeat_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Repeat);
        self.expect(SvToken::LeftParen);
        let count = self.parse_expr();
        self.expect(SvToken::RightParen);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        Statement::Repeat {
            count,
            body: Box::new(body),
            span,
        }
    }

    /// Parses a foreach loop.
    fn parse_foreach_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Foreach);
        self.expect(SvToken::LeftParen);
        let array = self.parse_expr_bp(23); // just the array name
        self.expect(SvToken::LeftBracket);
        let mut variables = Vec::new();
        if !self.at(SvToken::RightBracket) {
            variables.push(self.expect_ident());
            while self.eat(SvToken::Comma) {
                variables.push(self.expect_ident());
            }
        }
        self.expect(SvToken::RightBracket);
        self.expect(SvToken::RightParen);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        Statement::Foreach {
            array,
            variables,
            body: Box::new(body),
            span,
        }
    }

    /// Parses a wait statement.
    fn parse_wait_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Wait);
        self.expect(SvToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(SvToken::RightParen);

        let body = if !self.at(SvToken::Semicolon) {
            Some(Box::new(self.parse_statement()))
        } else {
            self.advance();
            None
        };

        let span = start.merge(self.prev_span());
        Statement::Wait {
            condition,
            body,
            span,
        }
    }

    /// Parses an event control: `@(sensitivity_list) stmt` or `@* stmt`.
    pub(crate) fn parse_event_control(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::At);

        // @* shorthand
        if self.eat(SvToken::Star) {
            let body = self.parse_statement();
            let span = start.merge(self.prev_span());
            return Statement::EventControl {
                sensitivity: SensitivityList::Star,
                body: Box::new(body),
                span,
            };
        }

        self.expect(SvToken::LeftParen);

        // @(*)
        if self.at(SvToken::Star) {
            self.advance();
            self.expect(SvToken::RightParen);
            let body = self.parse_statement();
            let span = start.merge(self.prev_span());
            return Statement::EventControl {
                sensitivity: SensitivityList::Star,
                body: Box::new(body),
                span,
            };
        }

        let sensitivity = self.parse_sensitivity_list();
        self.expect(SvToken::RightParen);

        let body = self.parse_statement();
        let span = start.merge(self.prev_span());

        Statement::EventControl {
            sensitivity,
            body: Box::new(body),
            span,
        }
    }

    /// Parses a sensitivity list.
    pub(crate) fn parse_sensitivity_list(&mut self) -> SensitivityList {
        let mut items = Vec::new();
        items.push(self.parse_sensitivity_item());
        while self.eat(SvToken::Or) || self.eat(SvToken::Comma) {
            items.push(self.parse_sensitivity_item());
        }
        SensitivityList::List(items)
    }

    /// Parses a single sensitivity list item.
    fn parse_sensitivity_item(&mut self) -> SensitivityItem {
        let start = self.current_span();
        let edge = if self.eat(SvToken::Posedge) {
            Some(EdgeKind::Posedge)
        } else if self.eat(SvToken::Negedge) {
            Some(EdgeKind::Negedge)
        } else {
            None
        };
        let signal = self.parse_expr();
        let span = start.merge(signal.span());
        SensitivityItem { edge, signal, span }
    }

    /// Parses a delay control: `#expr stmt`.
    fn parse_delay_control(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Hash);
        let delay = self.parse_expr_bp(23);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        Statement::Delay {
            delay,
            body: Box::new(body),
            span,
        }
    }

    /// Parses a disable statement.
    fn parse_disable_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Disable);
        let name = self.expect_ident();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::Disable { name, span }
    }

    /// Parses a return statement.
    fn parse_return_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(SvToken::Return);
        let value = if !self.at(SvToken::Semicolon) {
            Some(self.parse_expr())
        } else {
            None
        };
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::Return { value, span }
    }

    /// Parses a system task call (e.g., `$display("hello");`).
    fn parse_system_task_call(&mut self) -> Statement {
        let start = self.current_span();
        let text = self.current_text();
        let name = self.interner.get_or_intern(text);
        self.advance();

        let args = if self.at(SvToken::LeftParen) {
            self.parse_call_args()
        } else {
            Vec::new()
        };

        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::SystemTaskCall { name, args, span }
    }

    /// Parses a prefix increment/decrement statement.
    fn parse_incr_decr_statement(&mut self) -> Statement {
        let start = self.current_span();
        let increment = self.at(SvToken::PlusPlus);
        self.advance();
        let operand = self.parse_name_or_lvalue();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::IncrDecr {
            operand,
            increment,
            prefix: true,
            span,
        }
    }

    /// Parses a local variable declaration in a procedural block.
    fn parse_local_var_decl(&mut self) -> Statement {
        let start = self.current_span();
        let var_type = match self.current() {
            SvToken::Logic => {
                self.advance();
                VarType::Logic
            }
            SvToken::Bit => {
                self.advance();
                VarType::Bit
            }
            SvToken::Byte => {
                self.advance();
                VarType::Byte
            }
            SvToken::Shortint => {
                self.advance();
                VarType::Shortint
            }
            SvToken::Int => {
                self.advance();
                VarType::Int
            }
            SvToken::Longint => {
                self.advance();
                VarType::Longint
            }
            _ => {
                self.error("expected variable type");
                VarType::Logic
            }
        };

        let signed = self.eat(SvToken::Signed);
        let range = if self.at(SvToken::LeftBracket) {
            Some(self.parse_range())
        } else {
            None
        };

        let names = self.parse_decl_name_list();
        self.expect(SvToken::Semicolon);
        let span = start.merge(self.prev_span());

        Statement::LocalVarDecl(VarDecl {
            var_type,
            signed,
            range,
            names,
            span,
        })
    }

    /// Parses an immediate assertion statement.
    fn parse_assertion_stmt(&mut self) -> SvAssertion {
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

        // Optional pass statement
        let pass_stmt = if !self.at(SvToken::Else) && !self.at(SvToken::Semicolon) {
            Some(Box::new(self.parse_statement()))
        } else if self.at(SvToken::Semicolon) && !self.peek_is(SvToken::Else) {
            self.advance(); // eat ;
            None
        } else {
            None
        };

        // Optional else (fail) statement
        let fail_stmt = if self.eat(SvToken::Else) {
            Some(Box::new(self.parse_statement()))
        } else {
            None
        };

        let span = start.merge(self.prev_span());
        SvAssertion {
            kind,
            condition,
            pass_stmt,
            fail_stmt,
            span,
        }
    }

    /// Parses an assignment (blocking/non-blocking/compound) or task call.
    fn parse_assignment_or_task_call(&mut self) -> Statement {
        let start = self.current_span();
        let target = self.parse_name_or_lvalue();

        match self.current() {
            SvToken::Equals => {
                self.advance();
                let value = self.parse_expr();
                self.expect(SvToken::Semicolon);
                let span = start.merge(self.prev_span());
                Statement::Blocking {
                    target,
                    value,
                    span,
                }
            }
            SvToken::LessEquals => {
                self.advance();
                let value = self.parse_expr();
                self.expect(SvToken::Semicolon);
                let span = start.merge(self.prev_span());
                Statement::NonBlocking {
                    target,
                    value,
                    span,
                }
            }
            // Compound assignments
            _ if self.current().is_assignment_op() => {
                let op = self.eat_compound_op().unwrap();
                let value = self.parse_expr();
                self.expect(SvToken::Semicolon);
                let span = start.merge(self.prev_span());
                Statement::CompoundAssign {
                    target,
                    op,
                    value,
                    span,
                }
            }
            // Postfix ++ / --
            SvToken::PlusPlus | SvToken::MinusMinus => {
                let increment = self.at(SvToken::PlusPlus);
                self.advance();
                self.expect(SvToken::Semicolon);
                let span = start.merge(self.prev_span());
                Statement::IncrDecr {
                    operand: target,
                    increment,
                    prefix: false,
                    span,
                }
            }
            SvToken::Semicolon => {
                self.advance();
                let span = start.merge(self.prev_span());
                Statement::TaskCall {
                    name: target,
                    args: Vec::new(),
                    span,
                }
            }
            _ => {
                let span = self.current_span();
                self.error("expected '=', '<=', compound assignment, or ';' after expression");
                self.recover_to_semicolon();
                Statement::Error(start.merge(span))
            }
        }
    }

    /// Parses a name or LHS expression (identifier with optional indexing, hierarchical dots).
    /// This avoids entering the full Pratt parser which would consume `<=` as comparison.
    fn parse_name_or_lvalue(&mut self) -> Expr {
        let start = self.current_span();

        // Handle concatenation LHS: {a, b} = ...
        if self.at(SvToken::LeftBrace) {
            return self.parse_concat_or_repeat();
        }

        let ident = self.expect_ident();

        // Handle scoped ident: pkg::name
        if self.at(SvToken::ColonColon) {
            self.advance();
            let scoped_name = self.expect_ident();
            let span = start.merge(self.prev_span());
            let mut expr = Expr::ScopedIdent {
                scope: ident,
                name: scoped_name,
                span,
            };
            // Parse suffixes
            loop {
                match self.current() {
                    SvToken::Dot => {
                        expr = self.parse_dot_suffix(expr);
                    }
                    SvToken::LeftBracket => {
                        expr = self.parse_postfix_index(expr);
                    }
                    SvToken::LeftParen => {
                        let args = self.parse_call_args();
                        let span = start.merge(self.prev_span());
                        return Expr::FuncCall {
                            name: Box::new(expr),
                            args,
                            span,
                        };
                    }
                    _ => break,
                }
            }
            return expr;
        }

        let mut expr = Expr::Identifier {
            name: ident,
            span: start,
        };

        // Parse suffixes: dot, index, range
        loop {
            match self.current() {
                SvToken::Dot => {
                    expr = self.parse_dot_suffix(expr);
                }
                SvToken::LeftBracket => {
                    expr = self.parse_postfix_index(expr);
                }
                SvToken::LeftParen => {
                    // Task call with args
                    let args = self.parse_call_args();
                    let span = start.merge(self.prev_span());
                    return Expr::FuncCall {
                        name: Box::new(expr),
                        args,
                        span,
                    };
                }
                _ => break,
            }
        }

        expr
    }

    /// Tries to consume a compound assignment operator.
    pub(crate) fn eat_compound_op(&mut self) -> Option<CompoundOp> {
        let op = match self.current() {
            SvToken::PlusEquals => CompoundOp::Add,
            SvToken::MinusEquals => CompoundOp::Sub,
            SvToken::StarEquals => CompoundOp::Mul,
            SvToken::SlashEquals => CompoundOp::Div,
            SvToken::PercentEquals => CompoundOp::Mod,
            SvToken::AmpersandEquals => CompoundOp::BitAnd,
            SvToken::PipeEquals => CompoundOp::BitOr,
            SvToken::CaretEquals => CompoundOp::BitXor,
            SvToken::DoubleLessEquals => CompoundOp::Shl,
            SvToken::DoubleGreaterEquals => CompoundOp::Shr,
            SvToken::TripleLessEquals => CompoundOp::AShl,
            SvToken::TripleGreaterEquals => CompoundOp::AShr,
            _ => return None,
        };
        self.advance();
        Some(op)
    }

    /// Parses a comma-separated list of declaration names with optional dimensions and init.
    pub(crate) fn parse_decl_name_list(&mut self) -> Vec<DeclName> {
        let mut names = Vec::new();
        names.push(self.parse_decl_name());
        while self.eat(SvToken::Comma) {
            names.push(self.parse_decl_name());
        }
        names
    }

    /// Parses a single declaration name with optional array dimensions and init.
    pub(crate) fn parse_decl_name(&mut self) -> DeclName {
        let start = self.current_span();
        let name = self.expect_ident();
        let mut dimensions = Vec::new();
        while self.at(SvToken::LeftBracket) {
            dimensions.push(self.parse_range());
        }
        let init = if self.eat(SvToken::Equals) {
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
}

#[cfg(test)]
mod tests {
    use crate::ast::*;
    use crate::lexer;
    use crate::parser::SvParser;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::FileId;

    fn parse_module_items(source: &str) -> Vec<ModuleItem> {
        let full = format!("module test; {} endmodule", source);
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(&full, file, &sink);
        let mut parser = SvParser::new(tokens, &full, file, &interner, &sink);
        let ast = parser.parse_source_file();
        assert!(
            !sink.has_errors(),
            "unexpected errors: {:?}",
            sink.diagnostics()
        );
        match &ast.items[0] {
            SvItem::Module(m) => m.items.clone(),
            _ => panic!("expected module"),
        }
    }

    fn get_always_body(items: &[ModuleItem]) -> &Statement {
        for item in items {
            match item {
                ModuleItem::AlwaysBlock(ab) => return &ab.body,
                ModuleItem::AlwaysComb(ab) => return &ab.body,
                ModuleItem::AlwaysFf(ab) => return &ab.body,
                ModuleItem::AlwaysLatch(ab) => return &ab.body,
                _ => {}
            }
        }
        panic!("no always block found");
    }

    #[test]
    fn blocking_assignment() {
        let items = parse_module_items("always_comb begin a = b; end");
        let body = get_always_body(&items);
        match body {
            Statement::Block { stmts, .. } => {
                assert!(matches!(stmts[0], Statement::Blocking { .. }));
            }
            _ => panic!("expected block"),
        }
    }

    #[test]
    fn non_blocking_assignment() {
        let items = parse_module_items("always_ff @(posedge clk) q <= d;");
        let body = get_always_body(&items);
        // always_ff extracts sensitivity; body is the statement directly
        assert!(matches!(body, Statement::NonBlocking { .. }));
    }

    #[test]
    fn compound_assignment() {
        let items = parse_module_items("always_comb begin a += b; end");
        let body = get_always_body(&items);
        match body {
            Statement::Block { stmts, .. } => {
                assert!(matches!(
                    stmts[0],
                    Statement::CompoundAssign {
                        op: CompoundOp::Add,
                        ..
                    }
                ));
            }
            _ => panic!("expected block"),
        }
    }

    #[test]
    fn postfix_increment() {
        let items = parse_module_items("always_comb begin i++; end");
        let body = get_always_body(&items);
        match body {
            Statement::Block { stmts, .. } => match &stmts[0] {
                Statement::IncrDecr {
                    increment, prefix, ..
                } => {
                    assert!(*increment);
                    assert!(!*prefix);
                }
                _ => panic!("expected incr/decr"),
            },
            _ => panic!("expected block"),
        }
    }

    #[test]
    fn prefix_decrement() {
        let items = parse_module_items("always_comb begin --i; end");
        let body = get_always_body(&items);
        match body {
            Statement::Block { stmts, .. } => match &stmts[0] {
                Statement::IncrDecr {
                    increment, prefix, ..
                } => {
                    assert!(!*increment);
                    assert!(*prefix);
                }
                _ => panic!("expected incr/decr"),
            },
            _ => panic!("expected block"),
        }
    }

    #[test]
    fn if_else_statement() {
        let items = parse_module_items(
            "always_ff @(posedge clk)
                if (rst) q <= 0;
                else q <= d;",
        );
        let body = get_always_body(&items);
        // always_ff extracts sensitivity; body is the if statement directly
        assert!(matches!(body, Statement::If { .. }));
    }

    #[test]
    fn unique_if() {
        let items = parse_module_items(
            "always_comb begin
                unique if (a) y = 1;
                else if (b) y = 2;
                else y = 3;
            end",
        );
        let body = get_always_body(&items);
        match body {
            Statement::Block { stmts, .. } => match &stmts[0] {
                Statement::If { modifier, .. } => {
                    assert_eq!(*modifier, Some(CaseModifier::Unique));
                }
                _ => panic!("expected if"),
            },
            _ => panic!("expected block"),
        }
    }

    #[test]
    fn priority_case() {
        let items = parse_module_items(
            "always_comb begin
                priority case (sel)
                    2'b00: y = a;
                    default: y = b;
                endcase
            end",
        );
        let body = get_always_body(&items);
        match body {
            Statement::Block { stmts, .. } => match &stmts[0] {
                Statement::Case { modifier, .. } => {
                    assert_eq!(*modifier, Some(CaseModifier::Priority));
                }
                _ => panic!("expected case"),
            },
            _ => panic!("expected block"),
        }
    }

    #[test]
    fn case_statement() {
        let items = parse_module_items(
            "always_comb
                case (sel)
                    2'b00: y = a;
                    2'b01: y = b;
                    default: y = c;
                endcase",
        );
        let body = get_always_body(&items);
        match body {
            Statement::Case { arms, kind, .. } => {
                assert_eq!(*kind, CaseKind::Case);
                assert_eq!(arms.len(), 3);
                assert!(arms[2].is_default);
            }
            _ => panic!("expected case"),
        }
    }

    #[test]
    fn for_loop_with_int() {
        let items = parse_module_items(
            "initial begin
                for (int i = 0; i < 8; i++)
                    data[i] = 0;
            end",
        );
        match &items[0] {
            ModuleItem::InitialBlock(ib) => match &ib.body {
                Statement::Block { stmts, .. } => {
                    assert!(matches!(stmts[0], Statement::For { .. }));
                }
                _ => panic!("expected block"),
            },
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn while_loop() {
        let items = parse_module_items(
            "initial begin
                while (count < 10)
                    count = count + 1;
            end",
        );
        match &items[0] {
            ModuleItem::InitialBlock(ib) => match &ib.body {
                Statement::Block { stmts, .. } => {
                    assert!(matches!(stmts[0], Statement::While { .. }));
                }
                _ => panic!("expected block"),
            },
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn do_while_loop() {
        let items = parse_module_items(
            "initial begin
                do begin
                    count = count + 1;
                end while (count < 10);
            end",
        );
        match &items[0] {
            ModuleItem::InitialBlock(ib) => match &ib.body {
                Statement::Block { stmts, .. } => {
                    assert!(matches!(stmts[0], Statement::DoWhile { .. }));
                }
                _ => panic!("expected block"),
            },
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn forever_loop() {
        let items = parse_module_items("initial forever #5 clk = ~clk;");
        match &items[0] {
            ModuleItem::InitialBlock(ib) => {
                assert!(matches!(ib.body, Statement::Forever { .. }));
            }
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn return_statement() {
        let items = parse_module_items(
            "function int add;
                input int a;
                input int b;
                return a + b;
            endfunction",
        );
        match &items[0] {
            ModuleItem::FunctionDecl(f) => {
                assert!(matches!(f.body[0], Statement::Return { .. }));
            }
            _ => panic!("expected function"),
        }
    }

    #[test]
    fn break_continue() {
        let items = parse_module_items(
            "initial begin
                for (int i = 0; i < 10; i++) begin
                    if (i == 5) break;
                    if (i == 3) continue;
                end
            end",
        );
        match &items[0] {
            ModuleItem::InitialBlock(ib) => match &ib.body {
                Statement::Block { stmts, .. } => {
                    assert!(matches!(stmts[0], Statement::For { .. }));
                }
                _ => panic!("expected block"),
            },
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn event_control_posedge() {
        // always_ff extracts sensitivity into AlwaysFfBlock.sensitivity
        let items = parse_module_items("always_ff @(posedge clk) q <= d;");
        match &items[0] {
            ModuleItem::AlwaysFf(ab) => match &ab.sensitivity {
                SensitivityList::List(sens_items) => {
                    assert_eq!(sens_items.len(), 1);
                    assert_eq!(sens_items[0].edge, Some(EdgeKind::Posedge));
                }
                _ => panic!("expected sensitivity list"),
            },
            _ => panic!("expected always_ff"),
        }
    }

    #[test]
    fn system_task_call() {
        let items = parse_module_items("initial $display(\"hello\");");
        match &items[0] {
            ModuleItem::InitialBlock(ib) => {
                assert!(matches!(ib.body, Statement::SystemTaskCall { .. }));
            }
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn null_statement() {
        let items = parse_module_items("initial ;");
        match &items[0] {
            ModuleItem::InitialBlock(ib) => {
                assert!(matches!(ib.body, Statement::Null { .. }));
            }
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn local_var_decl_in_block() {
        let items = parse_module_items(
            "always_comb begin
                int temp;
                temp = a + b;
            end",
        );
        let body = get_always_body(&items);
        match body {
            Statement::Block { stmts, .. } => {
                assert!(matches!(stmts[0], Statement::LocalVarDecl(_)));
                assert!(matches!(stmts[1], Statement::Blocking { .. }));
            }
            _ => panic!("expected block"),
        }
    }
}
