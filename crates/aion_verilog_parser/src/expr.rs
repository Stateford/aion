//! Pratt expression parser for Verilog-2005.
//!
//! Implements operator-precedence parsing following IEEE 1364-2005 Table 5-4:
//!
//! | BP (L,R) | Operators |
//! |----------|-----------|
//! | (1,2)    | `||` |
//! | (3,4)    | `&&` |
//! | (5,6)    | `|` |
//! | (7,8)    | `^` `~^` `^~` |
//! | (9,10)   | `&` |
//! | (11,12)  | `==` `!=` `===` `!==` |
//! | (13,14)  | `<` `<=` `>` `>=` |
//! | (15,16)  | `<<` `>>` `<<<` `>>>` |
//! | (17,18)  | `+` `-` |
//! | (19,20)  | `*` `/` `%` |
//! | (22,21)  | `**` (right-assoc) |
//! | prefix 23 | `+` `-` `!` `~` `&` `~&` `|` `~|` `^` `~^` |
//!
//! Ternary `? :` is handled as a special case at min_bp=0 (right-associative).

use crate::ast::*;
use crate::parser::VerilogParser;
use crate::token::VerilogToken;

/// Binding power for binary operators. Returns (left_bp, right_bp).
fn infix_binding_power(op: &BinaryOp) -> (u8, u8) {
    match op {
        BinaryOp::LogOr => (1, 2),
        BinaryOp::LogAnd => (3, 4),
        BinaryOp::BitOr => (5, 6),
        BinaryOp::BitXor | BinaryOp::BitXnor => (7, 8),
        BinaryOp::BitAnd => (9, 10),
        BinaryOp::Eq | BinaryOp::Neq | BinaryOp::CaseEq | BinaryOp::CaseNeq => (11, 12),
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => (13, 14),
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::AShl | BinaryOp::AShr => (15, 16),
        BinaryOp::Add | BinaryOp::Sub => (17, 18),
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => (19, 20),
        BinaryOp::Pow => (22, 21), // right-associative
    }
}

impl VerilogParser<'_> {
    /// Parses an expression.
    pub fn parse_expr(&mut self) -> Expr {
        self.parse_expr_bp(0)
    }

    /// Parses an expression with minimum binding power.
    pub(crate) fn parse_expr_bp(&mut self, min_bp: u8) -> Expr {
        let mut lhs = self.parse_prefix_expr();

        loop {
            if self.at_eof() {
                break;
            }

            // Check for ternary `? :`  at binding power 0
            if self.at(VerilogToken::Question) && min_bp == 0 {
                let op_span = self.current_span();
                self.advance(); // eat ?
                let then_expr = self.parse_expr_bp(0); // right-associative
                self.expect(VerilogToken::Colon);
                let else_expr = self.parse_expr_bp(0);
                let span = lhs.span().merge(else_expr.span()).merge(op_span);
                lhs = Expr::Ternary {
                    condition: Box::new(lhs),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                    span,
                };
                continue;
            }

            // Postfix: indexing `[i]`, range `[m:l]`, part-select `[i+:w]`/`[i-:w]`
            if self.at(VerilogToken::LeftBracket) {
                lhs = self.parse_postfix_index(lhs);
                continue;
            }

            // Postfix: dot selection (hierarchical name)
            if self.at(VerilogToken::Dot) {
                lhs = self.parse_dot_suffix(lhs);
                continue;
            }

            // Infix binary operator
            let op = match self.current_as_binary_op() {
                Some(op) => op,
                None => break,
            };

            let (l_bp, r_bp) = infix_binding_power(&op);
            if l_bp < min_bp {
                break;
            }

            self.advance(); // consume operator token

            let rhs = self.parse_expr_bp(r_bp);
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
                span,
            };
        }

        lhs
    }

    /// Continues Pratt parsing from an existing LHS expression.
    ///
    /// Used when we parsed an expression with restricted binding power and need
    /// to continue parsing the rest with a different binding power (e.g., for
    /// bracket index disambiguation).
    fn continue_expr_bp(&mut self, mut lhs: Expr, min_bp: u8) -> Expr {
        loop {
            if self.at_eof() {
                break;
            }

            if self.at(VerilogToken::Question) && min_bp == 0 {
                let op_span = self.current_span();
                self.advance();
                let then_expr = self.parse_expr_bp(0);
                self.expect(VerilogToken::Colon);
                let else_expr = self.parse_expr_bp(0);
                let span = lhs.span().merge(else_expr.span()).merge(op_span);
                lhs = Expr::Ternary {
                    condition: Box::new(lhs),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                    span,
                };
                continue;
            }

            if self.at(VerilogToken::LeftBracket) {
                lhs = self.parse_postfix_index(lhs);
                continue;
            }

            if self.at(VerilogToken::Dot) {
                lhs = self.parse_dot_suffix(lhs);
                continue;
            }

            let op = match self.current_as_binary_op() {
                Some(op) => op,
                None => break,
            };

            let (l_bp, r_bp) = infix_binding_power(&op);
            if l_bp < min_bp {
                break;
            }

            self.advance();

            let rhs = self.parse_expr_bp(r_bp);
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
                span,
            };
        }

        lhs
    }

    /// Parses a prefix expression (unary operators, literals, names, braces).
    fn parse_prefix_expr(&mut self) -> Expr {
        let start = self.current_span();

        // Unary operators
        if let Some(op) = self.current_as_unary_op() {
            self.advance();
            let operand = self.parse_expr_bp(23);
            let span = start.merge(operand.span());
            return Expr::Unary {
                op,
                operand: Box::new(operand),
                span,
            };
        }

        match self.current() {
            // Integer literal
            VerilogToken::IntLiteral | VerilogToken::SizedLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::Literal { span }
            }
            // Real literal
            VerilogToken::RealLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::RealLiteral { span }
            }
            // String literal
            VerilogToken::StringLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::StringLiteral { span }
            }
            // Parenthesized expression
            VerilogToken::LeftParen => {
                self.advance();
                let inner = self.parse_expr();
                self.expect(VerilogToken::RightParen);
                let span = start.merge(self.prev_span());
                Expr::Paren {
                    inner: Box::new(inner),
                    span,
                }
            }
            // Concatenation or replication: { ... }
            VerilogToken::LeftBrace => self.parse_concat_or_repeat(),
            // System function call: $clog2(...)
            VerilogToken::SystemIdentifier => {
                let text = self.current_text();
                let name = self.interner.get_or_intern(text);
                self.advance();
                let args = if self.at(VerilogToken::LeftParen) {
                    self.parse_call_args()
                } else {
                    Vec::new()
                };
                let span = start.merge(self.prev_span());
                Expr::SystemCall { name, args, span }
            }
            // Identifier (possibly followed by function call, indexing, etc.)
            VerilogToken::Identifier | VerilogToken::EscapedIdentifier => {
                let ident = self.expect_ident();
                let expr = Expr::Identifier {
                    name: ident,
                    span: start,
                };

                // Check for function call: ident(...)
                if self.at(VerilogToken::LeftParen) {
                    let args = self.parse_call_args();
                    let span = start.merge(self.prev_span());
                    Expr::FuncCall {
                        name: Box::new(expr),
                        args,
                        span,
                    }
                } else {
                    expr
                }
            }
            _ => {
                let span = self.current_span();
                self.error("expected expression");
                self.advance();
                Expr::Error(span)
            }
        }
    }

    /// Parses a concatenation `{a, b}` or replication `{3{a, b}}`.
    pub(crate) fn parse_concat_or_repeat(&mut self) -> Expr {
        let start = self.current_span();
        self.expect(VerilogToken::LeftBrace);

        // Empty braces — error
        if self.at(VerilogToken::RightBrace) {
            self.advance();
            self.error("empty concatenation");
            return Expr::Error(start);
        }

        // Parse first expression
        let first = self.parse_expr();

        // Check for replication: {count{elem, ...}}
        if self.at(VerilogToken::LeftBrace) {
            self.advance();
            let mut elements = Vec::new();
            elements.push(self.parse_expr());
            while self.eat(VerilogToken::Comma) {
                elements.push(self.parse_expr());
            }
            self.expect(VerilogToken::RightBrace);
            self.expect(VerilogToken::RightBrace);
            let span = start.merge(self.prev_span());
            return Expr::Repeat {
                count: Box::new(first),
                elements,
                span,
            };
        }

        // Regular concatenation
        let mut elements = vec![first];
        while self.eat(VerilogToken::Comma) {
            elements.push(self.parse_expr());
        }
        self.expect(VerilogToken::RightBrace);
        let span = start.merge(self.prev_span());
        Expr::Concat { elements, span }
    }

    /// Parses postfix index/range/part-select: `expr[i]`, `expr[m:l]`, `expr[i+:w]`
    ///
    /// For part-selects (`[i+:w]`, `[i-:w]`), the index expression is parsed with
    /// a binding power that stops before `+`/`-` so the `+:`/`-:` pattern is
    /// detected before the Pratt parser consumes the operator.
    pub(crate) fn parse_postfix_index(&mut self, base: Expr) -> Expr {
        let start = base.span();
        self.expect(VerilogToken::LeftBracket);

        // Parse the first expression, but stop before +/- (bp 18) so we can
        // detect +: and -: part-select patterns. If it turns out to be a regular
        // range or index, the expression is still correct for simple cases.
        let first = self.parse_expr_bp(18);

        // Check for part-select: [expr +: width] or [expr -: width]
        if self.at(VerilogToken::Plus) && self.peek_is(VerilogToken::Colon) {
            self.advance(); // eat +
            self.advance(); // eat :
            let width = self.parse_expr();
            self.expect(VerilogToken::RightBracket);
            let span = start.merge(self.prev_span());
            Expr::PartSelect {
                base: Box::new(base),
                index: Box::new(first),
                ascending: true,
                width: Box::new(width),
                span,
            }
        } else if self.at(VerilogToken::Minus) && self.peek_is(VerilogToken::Colon) {
            self.advance(); // eat -
            self.advance(); // eat :
            let width = self.parse_expr();
            self.expect(VerilogToken::RightBracket);
            let span = start.merge(self.prev_span());
            Expr::PartSelect {
                base: Box::new(base),
                index: Box::new(first),
                ascending: false,
                width: Box::new(width),
                span,
            }
        } else if self.at(VerilogToken::Plus) || self.at(VerilogToken::Minus) {
            // Not a part-select; re-parse as a full expression by continuing from
            // where we stopped. The `first` expression is the LHS, and we continue
            // parsing with min_bp=0 to get the full expression including +/-.
            let full_first = self.continue_expr_bp(first, 0);
            if self.at(VerilogToken::Colon) {
                self.advance();
                let second = self.parse_expr();
                self.expect(VerilogToken::RightBracket);
                let span = start.merge(self.prev_span());
                Expr::RangeSelect {
                    base: Box::new(base),
                    msb: Box::new(full_first),
                    lsb: Box::new(second),
                    span,
                }
            } else {
                self.expect(VerilogToken::RightBracket);
                let span = start.merge(self.prev_span());
                Expr::Index {
                    base: Box::new(base),
                    index: Box::new(full_first),
                    span,
                }
            }
        } else if self.at(VerilogToken::Colon) {
            self.advance();
            let second = self.parse_expr();
            self.expect(VerilogToken::RightBracket);
            let span = start.merge(self.prev_span());
            Expr::RangeSelect {
                base: Box::new(base),
                msb: Box::new(first),
                lsb: Box::new(second),
                span,
            }
        } else {
            self.expect(VerilogToken::RightBracket);
            let span = start.merge(self.prev_span());
            Expr::Index {
                base: Box::new(base),
                index: Box::new(first),
                span,
            }
        }
    }

    /// Parses a dot suffix for hierarchical names.
    pub(crate) fn parse_dot_suffix(&mut self, base: Expr) -> Expr {
        let start = base.span();
        // Collect all parts of the hierarchical name
        let mut parts = match base {
            Expr::Identifier { name, .. } => vec![name],
            Expr::HierarchicalName { parts, .. } => parts,
            _ => {
                // Can't dot-select on a non-name
                self.advance(); // eat .
                let _ = self.expect_ident();
                return base;
            }
        };

        while self.eat(VerilogToken::Dot) {
            parts.push(self.expect_ident());
        }

        let span = start.merge(self.prev_span());
        Expr::HierarchicalName { parts, span }
    }

    /// Parses function/task call arguments: `( expr {, expr} )`.
    pub(crate) fn parse_call_args(&mut self) -> Vec<Expr> {
        self.expect(VerilogToken::LeftParen);
        let mut args = Vec::new();
        if !self.at(VerilogToken::RightParen) {
            args.push(self.parse_expr());
            while self.eat(VerilogToken::Comma) {
                args.push(self.parse_expr());
            }
        }
        self.expect(VerilogToken::RightParen);
        args
    }

    /// Maps the current token to a binary operator, if applicable.
    ///
    /// Note: `<=` is NOT included here — it is handled by the statement parser
    /// as non-blocking assignment, not as a comparison in expression context.
    /// The statement parser calls `parse_expr()` for the LHS, and if it sees
    /// `<=` after, treats it as non-blocking assign. In pure expression contexts
    /// (e.g., inside `if()` conditions), `<=` IS a comparison — the caller must
    /// handle this by calling `parse_expr_with_le()` or using the ternary context.
    fn current_as_binary_op(&self) -> Option<BinaryOp> {
        match self.current() {
            VerilogToken::DoublePipe => Some(BinaryOp::LogOr),
            VerilogToken::DoubleAmpersand => Some(BinaryOp::LogAnd),
            VerilogToken::Pipe => Some(BinaryOp::BitOr),
            VerilogToken::Caret => Some(BinaryOp::BitXor),
            VerilogToken::TildeCaret => Some(BinaryOp::BitXnor),
            VerilogToken::Ampersand => Some(BinaryOp::BitAnd),
            VerilogToken::DoubleEquals => Some(BinaryOp::Eq),
            VerilogToken::BangEquals => Some(BinaryOp::Neq),
            VerilogToken::TripleEquals => Some(BinaryOp::CaseEq),
            VerilogToken::BangDoubleEquals => Some(BinaryOp::CaseNeq),
            VerilogToken::LessThan => Some(BinaryOp::Lt),
            VerilogToken::LessEquals => Some(BinaryOp::Le),
            VerilogToken::GreaterThan => Some(BinaryOp::Gt),
            VerilogToken::GreaterEquals => Some(BinaryOp::Ge),
            VerilogToken::DoubleLess => Some(BinaryOp::Shl),
            VerilogToken::DoubleGreater => Some(BinaryOp::Shr),
            VerilogToken::TripleLess => Some(BinaryOp::AShl),
            VerilogToken::TripleGreater => Some(BinaryOp::AShr),
            VerilogToken::Plus => Some(BinaryOp::Add),
            VerilogToken::Minus => Some(BinaryOp::Sub),
            VerilogToken::Star => Some(BinaryOp::Mul),
            VerilogToken::Slash => Some(BinaryOp::Div),
            VerilogToken::Percent => Some(BinaryOp::Mod),
            VerilogToken::DoubleStar => Some(BinaryOp::Pow),
            _ => None,
        }
    }

    /// Maps the current token to a unary operator, if applicable.
    fn current_as_unary_op(&self) -> Option<UnaryOp> {
        match self.current() {
            VerilogToken::Plus => Some(UnaryOp::Plus),
            VerilogToken::Minus => Some(UnaryOp::Minus),
            VerilogToken::Bang => Some(UnaryOp::LogNot),
            VerilogToken::Tilde => Some(UnaryOp::BitNot),
            VerilogToken::Ampersand => Some(UnaryOp::RedAnd),
            VerilogToken::TildeAmpersand => Some(UnaryOp::RedNand),
            VerilogToken::Pipe => Some(UnaryOp::RedOr),
            VerilogToken::TildePipe => Some(UnaryOp::RedNor),
            VerilogToken::Caret => Some(UnaryOp::RedXor),
            VerilogToken::TildeCaret => Some(UnaryOp::RedXnor),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lexer;
    use crate::parser::VerilogParser;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::FileId;

    use super::*;

    fn parse_expr_str(source: &str) -> Expr {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = VerilogParser::new(tokens, source, file, &interner, &sink);
        let expr = parser.parse_expr();
        assert!(
            !sink.has_errors(),
            "unexpected errors: {:?}",
            sink.diagnostics()
        );
        expr
    }

    #[test]
    fn simple_identifier() {
        let expr = parse_expr_str("clk");
        assert!(matches!(expr, Expr::Identifier { .. }));
    }

    #[test]
    fn integer_literal() {
        let expr = parse_expr_str("42");
        assert!(matches!(expr, Expr::Literal { .. }));
    }

    #[test]
    fn sized_literal() {
        let expr = parse_expr_str("4'b1010");
        assert!(matches!(expr, Expr::Literal { .. }));
    }

    #[test]
    fn binary_add() {
        let expr = parse_expr_str("a + b");
        match expr {
            Expr::Binary { op, .. } => assert_eq!(op, BinaryOp::Add),
            _ => panic!("expected binary"),
        }
    }

    #[test]
    fn precedence_add_mul() {
        // a + b * c should be a + (b * c)
        let expr = parse_expr_str("a + b * c");
        match expr {
            Expr::Binary {
                op: BinaryOp::Add,
                right,
                ..
            } => {
                assert!(matches!(
                    *right,
                    Expr::Binary {
                        op: BinaryOp::Mul,
                        ..
                    }
                ));
            }
            _ => panic!("expected add at top"),
        }
    }

    #[test]
    fn precedence_logical_vs_comparison() {
        // a == b && c == d should be (a == b) && (c == d)
        let expr = parse_expr_str("a == b && c == d");
        match expr {
            Expr::Binary {
                op: BinaryOp::LogAnd,
                left,
                right,
                ..
            } => {
                assert!(matches!(
                    *left,
                    Expr::Binary {
                        op: BinaryOp::Eq,
                        ..
                    }
                ));
                assert!(matches!(
                    *right,
                    Expr::Binary {
                        op: BinaryOp::Eq,
                        ..
                    }
                ));
            }
            _ => panic!("expected logical and at top"),
        }
    }

    #[test]
    fn power_right_associative() {
        // a ** b ** c should be a ** (b ** c)
        let expr = parse_expr_str("a ** b ** c");
        match expr {
            Expr::Binary {
                op: BinaryOp::Pow,
                right,
                ..
            } => {
                assert!(matches!(
                    *right,
                    Expr::Binary {
                        op: BinaryOp::Pow,
                        ..
                    }
                ));
            }
            _ => panic!("expected pow at top"),
        }
    }

    #[test]
    fn unary_negation() {
        let expr = parse_expr_str("-a");
        match expr {
            Expr::Unary {
                op: UnaryOp::Minus, ..
            } => {}
            _ => panic!("expected unary minus"),
        }
    }

    #[test]
    fn unary_logical_not() {
        let expr = parse_expr_str("!a");
        match expr {
            Expr::Unary {
                op: UnaryOp::LogNot,
                ..
            } => {}
            _ => panic!("expected unary logical not"),
        }
    }

    #[test]
    fn unary_bitwise_not() {
        let expr = parse_expr_str("~a");
        match expr {
            Expr::Unary {
                op: UnaryOp::BitNot,
                ..
            } => {}
            _ => panic!("expected unary bitwise not"),
        }
    }

    #[test]
    fn reduction_operators() {
        let expr = parse_expr_str("&a");
        assert!(matches!(
            expr,
            Expr::Unary {
                op: UnaryOp::RedAnd,
                ..
            }
        ));
    }

    #[test]
    fn ternary_expression() {
        let expr = parse_expr_str("sel ? a : b");
        match expr {
            Expr::Ternary {
                condition,
                then_expr,
                else_expr,
                ..
            } => {
                assert!(matches!(*condition, Expr::Identifier { .. }));
                assert!(matches!(*then_expr, Expr::Identifier { .. }));
                assert!(matches!(*else_expr, Expr::Identifier { .. }));
            }
            _ => panic!("expected ternary"),
        }
    }

    #[test]
    fn concatenation() {
        let expr = parse_expr_str("{a, b, c}");
        match expr {
            Expr::Concat { elements, .. } => assert_eq!(elements.len(), 3),
            _ => panic!("expected concat"),
        }
    }

    #[test]
    fn replication() {
        let expr = parse_expr_str("{3{a}}");
        match expr {
            Expr::Repeat {
                count, elements, ..
            } => {
                assert!(matches!(*count, Expr::Literal { .. }));
                assert_eq!(elements.len(), 1);
            }
            _ => panic!("expected repeat"),
        }
    }

    #[test]
    fn index_expression() {
        let expr = parse_expr_str("data[7]");
        assert!(matches!(expr, Expr::Index { .. }));
    }

    #[test]
    fn range_select_expression() {
        let expr = parse_expr_str("data[7:0]");
        assert!(matches!(expr, Expr::RangeSelect { .. }));
    }

    #[test]
    fn part_select_ascending() {
        let expr = parse_expr_str("data[i+:4]");
        match expr {
            Expr::PartSelect { ascending, .. } => assert!(ascending),
            _ => panic!("expected part select"),
        }
    }

    #[test]
    fn part_select_descending() {
        let expr = parse_expr_str("data[i-:4]");
        match expr {
            Expr::PartSelect { ascending, .. } => assert!(!ascending),
            _ => panic!("expected part select"),
        }
    }

    #[test]
    fn function_call() {
        let expr = parse_expr_str("clog2(WIDTH)");
        match expr {
            Expr::FuncCall { args, .. } => assert_eq!(args.len(), 1),
            _ => panic!("expected func call"),
        }
    }

    #[test]
    fn system_call() {
        let expr = parse_expr_str("$clog2(8)");
        match expr {
            Expr::SystemCall { args, .. } => assert_eq!(args.len(), 1),
            _ => panic!("expected system call"),
        }
    }

    #[test]
    fn parenthesized_expr() {
        let expr = parse_expr_str("(a + b)");
        assert!(matches!(expr, Expr::Paren { .. }));
    }

    #[test]
    fn hierarchical_name() {
        let expr = parse_expr_str("u1.data");
        assert!(matches!(expr, Expr::HierarchicalName { .. }));
    }

    #[test]
    fn string_literal() {
        let expr = parse_expr_str("\"hello\"");
        assert!(matches!(expr, Expr::StringLiteral { .. }));
    }

    #[test]
    fn complex_expression() {
        // (a + b) * c - {d, e}
        let expr = parse_expr_str("(a + b) * c - {d, e}");
        assert!(matches!(
            expr,
            Expr::Binary {
                op: BinaryOp::Sub,
                ..
            }
        ));
    }
}
