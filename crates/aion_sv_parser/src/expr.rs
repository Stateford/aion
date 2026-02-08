//! Pratt expression parser for SystemVerilog-2017.
//!
//! Implements operator-precedence parsing extending IEEE 1364-2005 with
//! SystemVerilog operators:
//!
//! | BP (L,R) | Operators |
//! |----------|-----------|
//! | (1,2)    | `||` |
//! | (3,4)    | `&&` |
//! | (5,6)    | `|` |
//! | (7,8)    | `^` `~^` `^~` |
//! | (9,10)   | `&` |
//! | (11,12)  | `==` `!=` `===` `!==` `==?` `!=?` |
//! | (13,14)  | `<` `<=` `>` `>=` `inside` |
//! | (15,16)  | `<<` `>>` `<<<` `>>>` |
//! | (17,18)  | `+` `-` |
//! | (19,20)  | `*` `/` `%` |
//! | (22,21)  | `**` (right-assoc) |
//! | prefix 23 | `+` `-` `!` `~` `&` `~&` `|` `~|` `^` `~^` `++` `--` |
//!
//! Ternary `? :` is handled as a special case at min_bp=0 (right-associative).

use crate::ast::*;
use crate::parser::SvParser;
use crate::token::SvToken;

/// Binding power for binary operators. Returns (left_bp, right_bp).
fn infix_binding_power(op: &BinaryOp) -> (u8, u8) {
    match op {
        BinaryOp::LogOr => (1, 2),
        BinaryOp::LogAnd => (3, 4),
        BinaryOp::BitOr => (5, 6),
        BinaryOp::BitXor | BinaryOp::BitXnor => (7, 8),
        BinaryOp::BitAnd => (9, 10),
        BinaryOp::Eq
        | BinaryOp::Neq
        | BinaryOp::CaseEq
        | BinaryOp::CaseNeq
        | BinaryOp::WildEq
        | BinaryOp::WildNeq => (11, 12),
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => (13, 14),
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::AShl | BinaryOp::AShr => (15, 16),
        BinaryOp::Add | BinaryOp::Sub => (17, 18),
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => (19, 20),
        BinaryOp::Pow => (22, 21), // right-associative
    }
}

impl SvParser<'_> {
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

            // Check for ternary `? :` at binding power 0
            if self.at(SvToken::Question) && min_bp == 0 {
                let op_span = self.current_span();
                self.advance(); // eat ?
                let then_expr = self.parse_expr_bp(0);
                self.expect(SvToken::Colon);
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
            if self.at(SvToken::LeftBracket) {
                lhs = self.parse_postfix_index(lhs);
                continue;
            }

            // Postfix: dot selection (hierarchical name)
            if self.at(SvToken::Dot) {
                lhs = self.parse_dot_suffix(lhs);
                continue;
            }

            // Postfix: ++ and --
            if self.at(SvToken::PlusPlus) || self.at(SvToken::MinusMinus) {
                let increment = self.at(SvToken::PlusPlus);
                let op_span = self.current_span();
                self.advance();
                let span = lhs.span().merge(op_span);
                lhs = Expr::Unary {
                    op: if increment {
                        UnaryOp::PreIncr
                    } else {
                        UnaryOp::PreDecr
                    },
                    operand: Box::new(lhs),
                    span,
                };
                continue;
            }

            // Postfix: cast `expr'(expr)` — tick followed by open paren
            if self.at(SvToken::Tick) && self.peek_is(SvToken::LeftParen) {
                self.advance(); // eat '
                self.advance(); // eat (
                let inner = self.parse_expr();
                self.expect(SvToken::RightParen);
                let span = lhs.span().merge(self.prev_span());
                lhs = Expr::Cast {
                    cast_type: Box::new(lhs),
                    expr: Box::new(inner),
                    span,
                };
                continue;
            }

            // `inside` as an infix operator at relational precedence
            if self.at(SvToken::Inside) && 13 >= min_bp {
                self.advance(); // eat inside
                self.expect(SvToken::LeftBrace);
                let mut ranges = Vec::new();
                if !self.at(SvToken::RightBrace) {
                    ranges.push(self.parse_expr());
                    while self.eat(SvToken::Comma) {
                        ranges.push(self.parse_expr());
                    }
                }
                self.expect(SvToken::RightBrace);
                let span = lhs.span().merge(self.prev_span());
                lhs = Expr::Inside {
                    expr: Box::new(lhs),
                    ranges,
                    span,
                };
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
    /// to continue parsing the rest with a different binding power.
    fn continue_expr_bp(&mut self, mut lhs: Expr, min_bp: u8) -> Expr {
        loop {
            if self.at_eof() {
                break;
            }

            if self.at(SvToken::Question) && min_bp == 0 {
                let op_span = self.current_span();
                self.advance();
                let then_expr = self.parse_expr_bp(0);
                self.expect(SvToken::Colon);
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

            if self.at(SvToken::LeftBracket) {
                lhs = self.parse_postfix_index(lhs);
                continue;
            }

            if self.at(SvToken::Dot) {
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

        // Prefix ++ and --
        if self.at(SvToken::PlusPlus) {
            self.advance();
            let operand = self.parse_expr_bp(23);
            let span = start.merge(operand.span());
            return Expr::Unary {
                op: UnaryOp::PreIncr,
                operand: Box::new(operand),
                span,
            };
        }
        if self.at(SvToken::MinusMinus) {
            self.advance();
            let operand = self.parse_expr_bp(23);
            let span = start.merge(operand.span());
            return Expr::Unary {
                op: UnaryOp::PreDecr,
                operand: Box::new(operand),
                span,
            };
        }

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
            SvToken::IntLiteral | SvToken::SizedLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::Literal { span }
            }
            // Real literal
            SvToken::RealLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::RealLiteral { span }
            }
            // String literal
            SvToken::StringLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::StringLiteral { span }
            }
            // Parenthesized expression
            SvToken::LeftParen => {
                self.advance();
                let inner = self.parse_expr();
                self.expect(SvToken::RightParen);
                let span = start.merge(self.prev_span());
                Expr::Paren {
                    inner: Box::new(inner),
                    span,
                }
            }
            // Concatenation or replication: { ... }
            SvToken::LeftBrace => self.parse_concat_or_repeat(),
            // System function call: $clog2(...)
            SvToken::SystemIdentifier => {
                let text = self.current_text();
                let name = self.interner.get_or_intern(text);
                self.advance();
                let args = if self.at(SvToken::LeftParen) {
                    self.parse_call_args()
                } else {
                    Vec::new()
                };
                let span = start.merge(self.prev_span());
                Expr::SystemCall { name, args, span }
            }
            // Identifier (possibly with scope ::, function call, indexing, etc.)
            SvToken::Identifier | SvToken::EscapedIdentifier => {
                let ident = self.expect_ident();

                // Check for scoped identifier: ident::ident
                if self.at(SvToken::ColonColon) {
                    self.advance(); // eat ::
                    let scoped_name = self.expect_ident();
                    let span = start.merge(self.prev_span());
                    let expr = Expr::ScopedIdent {
                        scope: ident,
                        name: scoped_name,
                        span,
                    };

                    // Check for function call: pkg::func(...)
                    if self.at(SvToken::LeftParen) {
                        let args = self.parse_call_args();
                        let span = start.merge(self.prev_span());
                        return Expr::FuncCall {
                            name: Box::new(expr),
                            args,
                            span,
                        };
                    }
                    return expr;
                }

                let expr = Expr::Identifier {
                    name: ident,
                    span: start,
                };

                // Check for function call: ident(...)
                if self.at(SvToken::LeftParen) {
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
        self.expect(SvToken::LeftBrace);

        if self.at(SvToken::RightBrace) {
            self.advance();
            self.error("empty concatenation");
            return Expr::Error(start);
        }

        let first = self.parse_expr();

        // Check for replication: {count{elem, ...}}
        if self.at(SvToken::LeftBrace) {
            self.advance();
            let mut elements = Vec::new();
            elements.push(self.parse_expr());
            while self.eat(SvToken::Comma) {
                elements.push(self.parse_expr());
            }
            self.expect(SvToken::RightBrace);
            self.expect(SvToken::RightBrace);
            let span = start.merge(self.prev_span());
            return Expr::Repeat {
                count: Box::new(first),
                elements,
                span,
            };
        }

        // Regular concatenation
        let mut elements = vec![first];
        while self.eat(SvToken::Comma) {
            elements.push(self.parse_expr());
        }
        self.expect(SvToken::RightBrace);
        let span = start.merge(self.prev_span());
        Expr::Concat { elements, span }
    }

    /// Parses postfix index/range/part-select.
    pub(crate) fn parse_postfix_index(&mut self, base: Expr) -> Expr {
        let start = base.span();
        self.expect(SvToken::LeftBracket);

        let first = self.parse_expr_bp(18);

        // Check for part-select: [expr +: width] or [expr -: width]
        if self.at(SvToken::Plus) && self.peek_is(SvToken::Colon) {
            self.advance(); // eat +
            self.advance(); // eat :
            let width = self.parse_expr();
            self.expect(SvToken::RightBracket);
            let span = start.merge(self.prev_span());
            Expr::PartSelect {
                base: Box::new(base),
                index: Box::new(first),
                ascending: true,
                width: Box::new(width),
                span,
            }
        } else if self.at(SvToken::Minus) && self.peek_is(SvToken::Colon) {
            self.advance(); // eat -
            self.advance(); // eat :
            let width = self.parse_expr();
            self.expect(SvToken::RightBracket);
            let span = start.merge(self.prev_span());
            Expr::PartSelect {
                base: Box::new(base),
                index: Box::new(first),
                ascending: false,
                width: Box::new(width),
                span,
            }
        } else if self.at(SvToken::Plus) || self.at(SvToken::Minus) {
            let full_first = self.continue_expr_bp(first, 0);
            if self.at(SvToken::Colon) {
                self.advance();
                let second = self.parse_expr();
                self.expect(SvToken::RightBracket);
                let span = start.merge(self.prev_span());
                Expr::RangeSelect {
                    base: Box::new(base),
                    msb: Box::new(full_first),
                    lsb: Box::new(second),
                    span,
                }
            } else {
                self.expect(SvToken::RightBracket);
                let span = start.merge(self.prev_span());
                Expr::Index {
                    base: Box::new(base),
                    index: Box::new(full_first),
                    span,
                }
            }
        } else if self.at(SvToken::Colon) {
            self.advance();
            let second = self.parse_expr();
            self.expect(SvToken::RightBracket);
            let span = start.merge(self.prev_span());
            Expr::RangeSelect {
                base: Box::new(base),
                msb: Box::new(first),
                lsb: Box::new(second),
                span,
            }
        } else {
            self.expect(SvToken::RightBracket);
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
        let mut parts = match base {
            Expr::Identifier { name, .. } => vec![name],
            Expr::HierarchicalName { parts, .. } => parts,
            _ => {
                self.advance(); // eat .
                let _ = self.expect_ident();
                return base;
            }
        };

        while self.eat(SvToken::Dot) {
            parts.push(self.expect_ident());
        }

        let span = start.merge(self.prev_span());
        Expr::HierarchicalName { parts, span }
    }

    /// Parses function/task call arguments: `( expr {, expr} )`.
    pub(crate) fn parse_call_args(&mut self) -> Vec<Expr> {
        self.expect(SvToken::LeftParen);
        let mut args = Vec::new();
        if !self.at(SvToken::RightParen) {
            args.push(self.parse_expr());
            while self.eat(SvToken::Comma) {
                args.push(self.parse_expr());
            }
        }
        self.expect(SvToken::RightParen);
        args
    }

    /// Maps the current token to a binary operator, if applicable.
    fn current_as_binary_op(&self) -> Option<BinaryOp> {
        match self.current() {
            SvToken::DoublePipe => Some(BinaryOp::LogOr),
            SvToken::DoubleAmpersand => Some(BinaryOp::LogAnd),
            SvToken::Pipe => Some(BinaryOp::BitOr),
            SvToken::Caret => Some(BinaryOp::BitXor),
            SvToken::TildeCaret => Some(BinaryOp::BitXnor),
            SvToken::Ampersand => Some(BinaryOp::BitAnd),
            SvToken::DoubleEquals => Some(BinaryOp::Eq),
            SvToken::BangEquals => Some(BinaryOp::Neq),
            SvToken::TripleEquals => Some(BinaryOp::CaseEq),
            SvToken::BangDoubleEquals => Some(BinaryOp::CaseNeq),
            SvToken::WildcardEq => Some(BinaryOp::WildEq),
            SvToken::WildcardNeq => Some(BinaryOp::WildNeq),
            SvToken::LessThan => Some(BinaryOp::Lt),
            SvToken::LessEquals => Some(BinaryOp::Le),
            SvToken::GreaterThan => Some(BinaryOp::Gt),
            SvToken::GreaterEquals => Some(BinaryOp::Ge),
            SvToken::DoubleLess => Some(BinaryOp::Shl),
            SvToken::DoubleGreater => Some(BinaryOp::Shr),
            SvToken::TripleLess => Some(BinaryOp::AShl),
            SvToken::TripleGreater => Some(BinaryOp::AShr),
            SvToken::Plus => Some(BinaryOp::Add),
            SvToken::Minus => Some(BinaryOp::Sub),
            SvToken::Star => Some(BinaryOp::Mul),
            SvToken::Slash => Some(BinaryOp::Div),
            SvToken::Percent => Some(BinaryOp::Mod),
            SvToken::DoubleStar => Some(BinaryOp::Pow),
            _ => None,
        }
    }

    /// Maps the current token to a unary operator, if applicable.
    fn current_as_unary_op(&self) -> Option<UnaryOp> {
        match self.current() {
            SvToken::Plus => Some(UnaryOp::Plus),
            SvToken::Minus => Some(UnaryOp::Minus),
            SvToken::Bang => Some(UnaryOp::LogNot),
            SvToken::Tilde => Some(UnaryOp::BitNot),
            SvToken::Ampersand => Some(UnaryOp::RedAnd),
            SvToken::TildeAmpersand => Some(UnaryOp::RedNand),
            SvToken::Pipe => Some(UnaryOp::RedOr),
            SvToken::TildePipe => Some(UnaryOp::RedNor),
            SvToken::Caret => Some(UnaryOp::RedXor),
            SvToken::TildeCaret => Some(UnaryOp::RedXnor),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lexer;
    use crate::parser::SvParser;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::FileId;

    use super::*;

    fn parse_expr_str(source: &str) -> Expr {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = SvParser::new(tokens, source, file, &interner, &sink);
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
    fn ternary_expression() {
        let expr = parse_expr_str("sel ? a : b");
        assert!(matches!(expr, Expr::Ternary { .. }));
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
    fn hierarchical_name() {
        let expr = parse_expr_str("u1.data");
        assert!(matches!(expr, Expr::HierarchicalName { .. }));
    }

    #[test]
    fn scoped_identifier() {
        let expr = parse_expr_str("pkg::WIDTH");
        match expr {
            Expr::ScopedIdent { .. } => {}
            _ => panic!("expected scoped ident, got {:?}", expr),
        }
    }

    #[test]
    fn scoped_function_call() {
        let expr = parse_expr_str("pkg::func(a)");
        match expr {
            Expr::FuncCall { name, args, .. } => {
                assert!(matches!(*name, Expr::ScopedIdent { .. }));
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected scoped func call"),
        }
    }

    #[test]
    fn wildcard_equality() {
        let expr = parse_expr_str("a ==? b");
        match expr {
            Expr::Binary { op, .. } => assert_eq!(op, BinaryOp::WildEq),
            _ => panic!("expected wildcard eq"),
        }
    }

    #[test]
    fn wildcard_inequality() {
        let expr = parse_expr_str("a !=? b");
        match expr {
            Expr::Binary { op, .. } => assert_eq!(op, BinaryOp::WildNeq),
            _ => panic!("expected wildcard neq"),
        }
    }

    #[test]
    fn unary_negation() {
        let expr = parse_expr_str("-a");
        assert!(matches!(
            expr,
            Expr::Unary {
                op: UnaryOp::Minus,
                ..
            }
        ));
    }

    #[test]
    fn unary_logical_not() {
        let expr = parse_expr_str("!a");
        assert!(matches!(
            expr,
            Expr::Unary {
                op: UnaryOp::LogNot,
                ..
            }
        ));
    }

    #[test]
    fn reduction_and() {
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
    fn prefix_increment() {
        let expr = parse_expr_str("++i");
        assert!(matches!(
            expr,
            Expr::Unary {
                op: UnaryOp::PreIncr,
                ..
            }
        ));
    }

    #[test]
    fn prefix_decrement() {
        let expr = parse_expr_str("--i");
        assert!(matches!(
            expr,
            Expr::Unary {
                op: UnaryOp::PreDecr,
                ..
            }
        ));
    }

    #[test]
    fn power_right_associative() {
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
    fn parenthesized_expr() {
        let expr = parse_expr_str("(a + b)");
        assert!(matches!(expr, Expr::Paren { .. }));
    }

    #[test]
    fn string_literal_expr() {
        let expr = parse_expr_str("\"hello\"");
        assert!(matches!(expr, Expr::StringLiteral { .. }));
    }

    #[test]
    fn complex_expression() {
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
