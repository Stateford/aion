//! Statement parsing for Verilog-2005.
//!
//! Handles blocking and non-blocking assignments, if/case/for/while/forever/repeat/wait,
//! event control (`@`), delay control (`#`), begin/end blocks, task calls, system
//! task calls, and disable statements.
//!
//! **`<=` disambiguation:** In statement context, the LHS is parsed as a name
//! expression first. If `=` follows, it's a blocking assignment. If `<=` follows,
//! it's a non-blocking assignment. In expression context (inside conditions),
//! `<=` is a comparison operator handled by the Pratt parser.

use crate::ast::*;
use crate::parser::VerilogParser;
use crate::token::VerilogToken;

impl VerilogParser<'_> {
    /// Parses a single statement.
    pub fn parse_statement(&mut self) -> Statement {
        match self.current() {
            // begin ... end block
            VerilogToken::Begin => self.parse_begin_end_block(),
            // if statement
            VerilogToken::If => self.parse_if_statement(),
            // case/casex/casez
            VerilogToken::Case | VerilogToken::Casex | VerilogToken::Casez => {
                self.parse_case_statement()
            }
            // for loop
            VerilogToken::For => self.parse_for_statement(),
            // while loop
            VerilogToken::While => self.parse_while_statement(),
            // forever loop
            VerilogToken::Forever => self.parse_forever_statement(),
            // repeat loop
            VerilogToken::Repeat => self.parse_repeat_statement(),
            // wait statement
            VerilogToken::Wait => self.parse_wait_statement(),
            // event control: @(...)
            VerilogToken::At => self.parse_event_control(),
            // delay control: #expr
            VerilogToken::Hash => self.parse_delay_control(),
            // disable statement
            VerilogToken::Disable => self.parse_disable_statement(),
            // system task call: $display(...)
            VerilogToken::SystemIdentifier => self.parse_system_task_call(),
            // null statement: ;
            VerilogToken::Semicolon => {
                let span = self.current_span();
                self.advance();
                Statement::Null { span }
            }
            // Assignment or task call (starts with identifier)
            VerilogToken::Identifier
            | VerilogToken::EscapedIdentifier
            | VerilogToken::LeftBrace => self.parse_assignment_or_task_call(),
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
        self.expect(VerilogToken::Begin);

        // Optional label: begin : label
        let label = if self.eat(VerilogToken::Colon) {
            Some(self.expect_ident())
        } else {
            None
        };

        // Parse declarations and statements
        let mut decls = Vec::new();
        let mut stmts = Vec::new();

        // In named blocks, declarations can appear before statements
        if label.is_some() {
            while !self.at(VerilogToken::End) && !self.at_eof() {
                if self.is_at_declaration_start() {
                    if let Some(item) = self.parse_module_item_inner() {
                        decls.push(item);
                    }
                } else {
                    break;
                }
            }
        }

        while !self.at(VerilogToken::End) && !self.at_eof() {
            stmts.push(self.parse_statement());
        }

        self.expect(VerilogToken::End);
        let span = start.merge(self.prev_span());

        Statement::Block {
            label,
            decls,
            stmts,
            span,
        }
    }

    /// Parses an if statement.
    fn parse_if_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(VerilogToken::If);
        self.expect(VerilogToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(VerilogToken::RightParen);

        let then_stmt = self.parse_statement();

        let else_stmt = if self.eat(VerilogToken::Else) {
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
            condition,
            then_stmt: Box::new(then_stmt),
            else_stmt,
            span,
        }
    }

    /// Parses a case/casex/casez statement.
    fn parse_case_statement(&mut self) -> Statement {
        let start = self.current_span();
        let kind = match self.current() {
            VerilogToken::Casex => {
                self.advance();
                CaseKind::Casex
            }
            VerilogToken::Casez => {
                self.advance();
                CaseKind::Casez
            }
            _ => {
                self.expect(VerilogToken::Case);
                CaseKind::Case
            }
        };

        self.expect(VerilogToken::LeftParen);
        let expr = self.parse_expr();
        self.expect(VerilogToken::RightParen);

        let mut arms = Vec::new();
        while !self.at(VerilogToken::Endcase) && !self.at_eof() {
            arms.push(self.parse_case_arm());
        }

        self.expect(VerilogToken::Endcase);
        let span = start.merge(self.prev_span());

        Statement::Case {
            kind,
            expr,
            arms,
            span,
        }
    }

    /// Parses a single case arm.
    fn parse_case_arm(&mut self) -> CaseArm {
        let start = self.current_span();

        if self.eat(VerilogToken::Default) {
            self.eat(VerilogToken::Colon);
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
            while self.eat(VerilogToken::Comma) {
                patterns.push(self.parse_expr());
            }
            self.expect(VerilogToken::Colon);
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

    /// Parses a for loop.
    fn parse_for_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(VerilogToken::For);
        self.expect(VerilogToken::LeftParen);

        // Init: assignment
        let init = self.parse_blocking_assignment_stmt();
        // Don't consume extra semicolon — it was consumed in parse_blocking_assignment_stmt

        // Condition
        let condition = self.parse_expr();
        self.expect(VerilogToken::Semicolon);

        // Step: assignment (no trailing semicolon)
        let step = self.parse_blocking_assignment_no_semi();

        self.expect(VerilogToken::RightParen);
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

    /// Parses a blocking assignment as a statement (with semicolon).
    pub(crate) fn parse_blocking_assignment_stmt(&mut self) -> Statement {
        let start = self.current_span();
        let target = self.parse_expr();
        self.expect(VerilogToken::Equals);
        let value = self.parse_expr();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::Blocking {
            target,
            value,
            span,
        }
    }

    /// Parses a blocking assignment without trailing semicolon (for `for` step).
    pub(crate) fn parse_blocking_assignment_no_semi(&mut self) -> Statement {
        let start = self.current_span();
        let target = self.parse_expr();
        self.expect(VerilogToken::Equals);
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
        self.expect(VerilogToken::While);
        self.expect(VerilogToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(VerilogToken::RightParen);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        Statement::While {
            condition,
            body: Box::new(body),
            span,
        }
    }

    /// Parses a forever loop.
    fn parse_forever_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(VerilogToken::Forever);
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
        self.expect(VerilogToken::Repeat);
        self.expect(VerilogToken::LeftParen);
        let count = self.parse_expr();
        self.expect(VerilogToken::RightParen);
        let body = self.parse_statement();
        let span = start.merge(self.prev_span());
        Statement::Repeat {
            count,
            body: Box::new(body),
            span,
        }
    }

    /// Parses a wait statement.
    fn parse_wait_statement(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(VerilogToken::Wait);
        self.expect(VerilogToken::LeftParen);
        let condition = self.parse_expr();
        self.expect(VerilogToken::RightParen);

        let body = if !self.at(VerilogToken::Semicolon) {
            Some(Box::new(self.parse_statement()))
        } else {
            self.advance(); // eat ;
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
    fn parse_event_control(&mut self) -> Statement {
        let start = self.current_span();
        self.expect(VerilogToken::At);

        // @* shorthand
        if self.eat(VerilogToken::Star) {
            let body = self.parse_statement();
            let span = start.merge(self.prev_span());
            return Statement::EventControl {
                sensitivity: SensitivityList::Star,
                body: Box::new(body),
                span,
            };
        }

        self.expect(VerilogToken::LeftParen);

        // @(*)
        if self.at(VerilogToken::Star) {
            self.advance();
            self.expect(VerilogToken::RightParen);
            let body = self.parse_statement();
            let span = start.merge(self.prev_span());
            return Statement::EventControl {
                sensitivity: SensitivityList::Star,
                body: Box::new(body),
                span,
            };
        }

        // Parse sensitivity items
        let mut items = Vec::new();
        items.push(self.parse_sensitivity_item());
        while self.eat(VerilogToken::Or) || self.eat(VerilogToken::Comma) {
            items.push(self.parse_sensitivity_item());
        }

        self.expect(VerilogToken::RightParen);

        let body = self.parse_statement();
        let span = start.merge(self.prev_span());

        Statement::EventControl {
            sensitivity: SensitivityList::List(items),
            body: Box::new(body),
            span,
        }
    }

    /// Parses a single sensitivity list item.
    fn parse_sensitivity_item(&mut self) -> SensitivityItem {
        let start = self.current_span();
        let edge = if self.eat(VerilogToken::Posedge) {
            Some(EdgeKind::Posedge)
        } else if self.eat(VerilogToken::Negedge) {
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
        self.expect(VerilogToken::Hash);
        let delay = self.parse_expr_bp(23); // high binding power to avoid eating too much
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
        self.expect(VerilogToken::Disable);
        let name = self.expect_ident();
        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::Disable { name, span }
    }

    /// Parses a system task call (e.g., `$display("hello");`).
    fn parse_system_task_call(&mut self) -> Statement {
        let start = self.current_span();
        let text = self.current_text();
        let name = self.interner.get_or_intern(text);
        self.advance();

        let args = if self.at(VerilogToken::LeftParen) {
            self.parse_call_args()
        } else {
            Vec::new()
        };

        self.expect(VerilogToken::Semicolon);
        let span = start.merge(self.prev_span());
        Statement::SystemTaskCall { name, args, span }
    }

    /// Parses an assignment (blocking/non-blocking) or task call.
    ///
    /// Disambiguation: parse LHS as expression, then check:
    /// - `=` → blocking assignment
    /// - `<=` → non-blocking assignment
    /// - `(` → task call with arguments (already parsed in expression as FuncCall)
    /// - `;` → task call with no arguments
    fn parse_assignment_or_task_call(&mut self) -> Statement {
        let start = self.current_span();
        let target = self.parse_name_or_lvalue();

        match self.current() {
            VerilogToken::Equals => {
                self.advance();
                let value = self.parse_expr();
                self.expect(VerilogToken::Semicolon);
                let span = start.merge(self.prev_span());
                Statement::Blocking {
                    target,
                    value,
                    span,
                }
            }
            VerilogToken::LessEquals => {
                self.advance();
                let value = self.parse_expr();
                self.expect(VerilogToken::Semicolon);
                let span = start.merge(self.prev_span());
                Statement::NonBlocking {
                    target,
                    value,
                    span,
                }
            }
            VerilogToken::Semicolon => {
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
                self.error("expected '=', '<=', or ';' after expression");
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
        if self.at(VerilogToken::LeftBrace) {
            return self.parse_concat_or_repeat();
        }

        let ident = self.expect_ident();
        let mut expr = Expr::Identifier {
            name: ident,
            span: start,
        };

        // Parse suffixes: dot, index, range
        loop {
            match self.current() {
                VerilogToken::Dot => {
                    expr = self.parse_dot_suffix(expr);
                }
                VerilogToken::LeftBracket => {
                    expr = self.parse_postfix_index(expr);
                }
                VerilogToken::LeftParen => {
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
}

#[cfg(test)]
mod tests {
    use crate::ast::*;
    use crate::lexer;
    use crate::parser::VerilogParser;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::FileId;

    fn parse_module_items(source: &str) -> Vec<ModuleItem> {
        let full = format!("module test; {} endmodule", source);
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(&full, file, &sink);
        let mut parser = VerilogParser::new(tokens, &full, file, &interner, &sink);
        let ast = parser.parse_source_file();
        assert!(
            !sink.has_errors(),
            "unexpected errors: {:?}",
            sink.diagnostics()
        );
        match &ast.items[0] {
            VerilogItem::Module(m) => m.items.clone(),
            _ => panic!("expected module"),
        }
    }

    fn get_always_body(items: &[ModuleItem]) -> &Statement {
        for item in items {
            if let ModuleItem::AlwaysBlock(ab) = item {
                return &ab.body;
            }
        }
        panic!("no always block found");
    }

    #[test]
    fn blocking_assignment() {
        let items = parse_module_items("always @(*) begin a = b; end");
        let body = get_always_body(&items);
        match body {
            Statement::EventControl { body, .. } => match body.as_ref() {
                Statement::Block { stmts, .. } => {
                    assert!(matches!(stmts[0], Statement::Blocking { .. }));
                }
                _ => panic!("expected block"),
            },
            _ => panic!("expected event control"),
        }
    }

    #[test]
    fn non_blocking_assignment() {
        let items = parse_module_items("always @(posedge clk) q <= d;");
        let body = get_always_body(&items);
        match body {
            Statement::EventControl { body, .. } => {
                assert!(matches!(body.as_ref(), Statement::NonBlocking { .. }));
            }
            _ => panic!("expected event control"),
        }
    }

    #[test]
    fn if_else_statement() {
        let items = parse_module_items(
            "always @(posedge clk)
                if (rst) q <= 0;
                else q <= d;",
        );
        let body = get_always_body(&items);
        match body {
            Statement::EventControl { body, .. } => {
                assert!(matches!(body.as_ref(), Statement::If { .. }));
                if let Statement::If { else_stmt, .. } = body.as_ref() {
                    assert!(else_stmt.is_some());
                }
            }
            _ => panic!("expected event control"),
        }
    }

    #[test]
    fn case_statement() {
        let items = parse_module_items(
            "always @(*)
                case (sel)
                    2'b00: y = a;
                    2'b01: y = b;
                    default: y = c;
                endcase",
        );
        let body = get_always_body(&items);
        match body {
            Statement::EventControl { body, .. } => match body.as_ref() {
                Statement::Case { arms, kind, .. } => {
                    assert_eq!(*kind, CaseKind::Case);
                    assert_eq!(arms.len(), 3);
                    assert!(arms[2].is_default);
                }
                _ => panic!("expected case"),
            },
            _ => panic!("expected event control"),
        }
    }

    #[test]
    fn for_loop() {
        let items = parse_module_items(
            "initial begin
                for (i = 0; i < 8; i = i + 1)
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
    fn repeat_loop() {
        let items = parse_module_items("initial repeat (10) @(posedge clk) ;");
        match &items[0] {
            ModuleItem::InitialBlock(ib) => {
                assert!(matches!(ib.body, Statement::Repeat { .. }));
            }
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn wait_statement() {
        let items = parse_module_items("initial wait (ready) data = 1;");
        match &items[0] {
            ModuleItem::InitialBlock(ib) => {
                assert!(matches!(ib.body, Statement::Wait { .. }));
            }
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn event_control_posedge() {
        let items = parse_module_items("always @(posedge clk) q <= d;");
        let body = get_always_body(&items);
        match body {
            Statement::EventControl {
                sensitivity: SensitivityList::List(items),
                ..
            } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].edge, Some(EdgeKind::Posedge));
            }
            _ => panic!("expected event control with posedge"),
        }
    }

    #[test]
    fn event_control_multiple() {
        let items = parse_module_items("always @(posedge clk or negedge rst) q <= d;");
        let body = get_always_body(&items);
        match body {
            Statement::EventControl {
                sensitivity: SensitivityList::List(items),
                ..
            } => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].edge, Some(EdgeKind::Posedge));
                assert_eq!(items[1].edge, Some(EdgeKind::Negedge));
            }
            _ => panic!("expected event control"),
        }
    }

    #[test]
    fn event_control_star() {
        let items = parse_module_items("always @(*) y = a;");
        let body = get_always_body(&items);
        match body {
            Statement::EventControl {
                sensitivity: SensitivityList::Star,
                ..
            } => {}
            _ => panic!("expected @(*)"),
        }
    }

    #[test]
    fn event_control_at_star() {
        let items = parse_module_items("always @* y = a;");
        let body = get_always_body(&items);
        match body {
            Statement::EventControl {
                sensitivity: SensitivityList::Star,
                ..
            } => {}
            _ => panic!("expected @*"),
        }
    }

    #[test]
    fn delay_control() {
        let items = parse_module_items("initial #10 clk = 1;");
        match &items[0] {
            ModuleItem::InitialBlock(ib) => {
                assert!(matches!(ib.body, Statement::Delay { .. }));
            }
            _ => panic!("expected initial block"),
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
    fn disable_statement() {
        let items = parse_module_items("initial begin disable my_block; end");
        match &items[0] {
            ModuleItem::InitialBlock(ib) => match &ib.body {
                Statement::Block { stmts, .. } => {
                    assert!(matches!(stmts[0], Statement::Disable { .. }));
                }
                _ => panic!("expected block"),
            },
            _ => panic!("expected initial block"),
        }
    }

    #[test]
    fn begin_end_block_labeled() {
        let items = parse_module_items(
            "initial begin : my_block
                reg [7:0] temp;
                temp = 0;
            end",
        );
        match &items[0] {
            ModuleItem::InitialBlock(ib) => match &ib.body {
                Statement::Block {
                    label,
                    decls,
                    stmts,
                    ..
                } => {
                    assert!(label.is_some());
                    assert_eq!(decls.len(), 1);
                    assert_eq!(stmts.len(), 1);
                }
                _ => panic!("expected block"),
            },
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
    fn casex_statement() {
        let items = parse_module_items(
            "always @(*)
                casex (sel)
                    2'b1x: y = a;
                    default: y = b;
                endcase",
        );
        let body = get_always_body(&items);
        match body {
            Statement::EventControl { body, .. } => match body.as_ref() {
                Statement::Case { kind, .. } => {
                    assert_eq!(*kind, CaseKind::Casex);
                }
                _ => panic!("expected case"),
            },
            _ => panic!("expected event control"),
        }
    }
}
