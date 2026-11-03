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
//! | (21,22)  | `**` (left-assoc, like every binary operator) |
//! | prefix 23 | `+` `-` `!` `~` `&` `~&` `|` `~|` `^` `~^` `++` `--` |
//!
//! Ternary `? :` is handled as a special case at min_bp=0 (right-associative).
//!
//! Postfix selects (`[...]`, `.`) bind tighter than every prefix operator,
//! and prefix operators bind tighter than `**`, so `-a ** b` is
//! `(-a) ** b` and `~a[0]` is `~(a[0])`. Inside `[...]` the index is a full
//! expression; a `+` or `-` directly followed by `:` ends it, which is how
//! the `+:` / `-:` indexed part-select operators are recognised.

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
        // Left-associative per IEEE 1364-2005 §5.1.2 / IEEE 1800-2017 Table 11-2:
        // `a ** b ** c` is `(a ** b) ** c`.
        BinaryOp::Pow => (21, 22),
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

    /// Parses a postfix select: bit/element index `expr[i]`, range select
    /// `expr[m:l]`, or indexed part-select `expr[b+:w]` / `expr[b-:w]`.
    ///
    /// The first expression inside the brackets is parsed as a full expression
    /// (binding power 0), so every operator — shifts, bitwise, relational,
    /// equality, logical and the conditional operator — is allowed in an index
    /// or range bound. The `+:` / `-:` part-select operators are recognised
    /// because the binary-operator loop treats a `+` or `-` that is immediately
    /// followed by `:` as a terminator (see `at_part_select_op`), so
    /// `d[8*i+7 -: 8]` yields base `8*i+7` and width `8`.
    pub(crate) fn parse_postfix_index(&mut self, base: Expr) -> Expr {
        let start = base.span();
        self.expect(SvToken::LeftBracket);

        let first = self.parse_expr();

        if self.at_part_select_op() {
            // Indexed part-select: [base +: width] or [base -: width]
            let ascending = self.at(SvToken::Plus);
            self.advance(); // eat + or -
            self.advance(); // eat :
            let width = self.parse_expr();
            self.expect(SvToken::RightBracket);
            let span = start.merge(self.prev_span());
            Expr::PartSelect {
                base: Box::new(base),
                index: Box::new(first),
                ascending,
                width: Box::new(width),
                span,
            }
        } else if self.eat(SvToken::Colon) {
            // Range select: [msb : lsb]
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

    /// Returns `true` if the parser is at the `+:` or `-:` indexed part-select
    /// operator, i.e. a `+`/`-` token immediately followed by `:`.
    ///
    /// No expression can start with `:`, so a `+`/`-` in this position can
    /// never be a binary operator; the expression parser stops there and
    /// leaves the part-select operator for [`Self::parse_postfix_index`].
    fn at_part_select_op(&self) -> bool {
        (self.at(SvToken::Plus) || self.at(SvToken::Minus)) && self.peek_is(SvToken::Colon)
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
        if self.at_part_select_op() {
            // `+:` / `-:` terminate the index expression of a part-select.
            return None;
        }
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
    fn power_left_associative() {
        // IEEE 1364-2005 §5.1.2 / IEEE 1800-2017 Table 11-2: `**` is
        // left-associative, so `a ** b ** c` is `(a ** b) ** c`.
        let expr = parse_expr_str("a ** b ** c");
        match expr {
            Expr::Binary {
                op: BinaryOp::Pow,
                left,
                right,
                ..
            } => {
                assert!(matches!(
                    *left,
                    Expr::Binary {
                        op: BinaryOp::Pow,
                        ..
                    }
                ));
                assert!(matches!(*right, Expr::Identifier { .. }));
            }
            _ => panic!("expected pow at top"),
        }
    }

    #[test]
    fn unary_minus_binds_tighter_than_power() {
        // Unary operators are above `**` in the LRM table: `-a ** b` is `(-a) ** b`.
        let expr = parse_expr_str("-a ** b");
        match expr {
            Expr::Binary {
                op: BinaryOp::Pow,
                left,
                ..
            } => assert!(matches!(
                *left,
                Expr::Unary {
                    op: UnaryOp::Minus,
                    ..
                }
            )),
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

/// Table-driven operator-precedence regression tests.
///
/// Each case parses an expression and renders the AST as a compact
/// S-expression, which is compared against the grouping required by the
/// operator precedence table of IEEE 1800-2017 Table 11-2 (§11.3.2). From
/// highest to lowest: postfix selects, unary operators (including `++`/`--`),
/// `**`, `* / %`, binary `+ -`, shifts, relational and `inside`, equality
/// (including `==?`/`!=?`), binary `&`, binary `^ ~^ ^~`, binary `|`, `&&`,
/// `||`, `?:`. Every binary operator is left-associative; `?:` is
/// right-associative.
///
/// Rendering: `(op lhs rhs)` for binary operators, `(op operand)` for unary
/// operators, `(? c t e)` for the conditional operator, `(idx b i)` for a
/// bit-select, `(range b msb lsb)` for a range select, `(+: b i w)` /
/// `(-: b i w)` for indexed part-selects and `(paren e)` for parentheses.
#[cfg(test)]
mod precedence_matrix {
    use crate::lexer;
    use crate::parser::SvParser;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::FileId;

    use crate::ast::*;

    fn bin_sym(op: BinaryOp) -> &'static str {
        match op {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Pow => "**",
            BinaryOp::Eq => "==",
            BinaryOp::Neq => "!=",
            BinaryOp::CaseEq => "===",
            BinaryOp::CaseNeq => "!==",
            BinaryOp::WildEq => "==?",
            BinaryOp::WildNeq => "!=?",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::LogAnd => "&&",
            BinaryOp::LogOr => "||",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::BitXnor => "~^",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::AShl => "<<<",
            BinaryOp::AShr => ">>>",
        }
    }

    fn un_sym(op: UnaryOp) -> &'static str {
        match op {
            UnaryOp::Plus => "+",
            UnaryOp::Minus => "-",
            UnaryOp::LogNot => "!",
            UnaryOp::BitNot => "~",
            UnaryOp::RedAnd => "&",
            UnaryOp::RedNand => "~&",
            UnaryOp::RedOr => "|",
            UnaryOp::RedNor => "~|",
            UnaryOp::RedXor => "^",
            UnaryOp::RedXnor => "~^",
            UnaryOp::PreIncr => "++",
            UnaryOp::PreDecr => "--",
        }
    }

    fn list(head: &str, items: &[&Expr], src: &str, interner: &Interner) -> String {
        let mut out = format!("({head}");
        for item in items {
            out.push(' ');
            out.push_str(&sexpr(item, src, interner));
        }
        out.push(')');
        out
    }

    /// Renders an expression as an S-expression for structural comparison.
    fn sexpr(expr: &Expr, src: &str, interner: &Interner) -> String {
        let text =
            |span: aion_source::Span| src[span.start as usize..span.end as usize].to_string();
        match expr {
            Expr::Identifier { name, .. } => interner.resolve(*name).to_string(),
            Expr::HierarchicalName { parts, .. } => parts
                .iter()
                .map(|p| interner.resolve(*p))
                .collect::<Vec<_>>()
                .join("."),
            Expr::ScopedIdent { scope, name, .. } => {
                format!("{}::{}", interner.resolve(*scope), interner.resolve(*name))
            }
            Expr::Literal { span } | Expr::RealLiteral { span } | Expr::StringLiteral { span } => {
                text(*span)
            }
            Expr::Index { base, index, .. } => list("idx", &[base, index], src, interner),
            Expr::RangeSelect { base, msb, lsb, .. } => {
                list("range", &[base, msb, lsb], src, interner)
            }
            Expr::PartSelect {
                base,
                index,
                ascending,
                width,
                ..
            } => list(
                if *ascending { "+:" } else { "-:" },
                &[base, index, width],
                src,
                interner,
            ),
            Expr::Concat { elements, .. } => list(
                "concat",
                &elements.iter().collect::<Vec<_>>(),
                src,
                interner,
            ),
            Expr::Repeat {
                count, elements, ..
            } => {
                let mut items: Vec<&Expr> = vec![count];
                items.extend(elements.iter());
                list("repeat", &items, src, interner)
            }
            Expr::Unary { op, operand, .. } => list(un_sym(*op), &[operand], src, interner),
            Expr::Binary {
                left, op, right, ..
            } => list(bin_sym(*op), &[left, right], src, interner),
            Expr::Ternary {
                condition,
                then_expr,
                else_expr,
                ..
            } => list("?", &[condition, then_expr, else_expr], src, interner),
            Expr::Inside { expr, ranges, .. } => {
                let mut items: Vec<&Expr> = vec![expr];
                items.extend(ranges.iter());
                list("inside", &items, src, interner)
            }
            Expr::Cast {
                cast_type, expr, ..
            } => list("cast", &[cast_type, expr], src, interner),
            Expr::FuncCall { name, args, .. } => {
                let mut items: Vec<&Expr> = vec![name];
                items.extend(args.iter());
                list("call", &items, src, interner)
            }
            Expr::SystemCall { name, args, .. } => list(
                interner.resolve(*name),
                &args.iter().collect::<Vec<_>>(),
                src,
                interner,
            ),
            Expr::Paren { inner, .. } => list("paren", &[inner], src, interner),
            Expr::Error(_) => "<error>".to_string(),
        }
    }

    /// Parses `src` as one expression. Returns the S-expression and whether
    /// the parse was clean (no diagnostics and every token consumed).
    fn parse(src: &str) -> (String, bool) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(src, file, &sink);
        let mut parser = SvParser::new(tokens, src, file, &interner, &sink);
        let expr = parser.parse_expr();
        let clean = !sink.has_errors() && parser.at_eof();
        (sexpr(&expr, src, &interner), clean)
    }

    fn check_table(table: &[(&str, &str)]) {
        let mut failures = Vec::new();
        for (src, want) in table {
            let (got, clean) = parse(src);
            if !clean || got != *want {
                failures.push(format!(
                    "  {src:<28} want {want}\n  {:<28} got  {got} (clean: {clean})",
                    ""
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "precedence mismatches:\n{}",
            failures.join("\n")
        );
    }

    /// Asserts that `src` is rejected (diagnostic or unconsumed tokens)
    /// without panicking.
    fn check_rejected(src: &str) {
        let (got, clean) = parse(src);
        assert!(!clean, "expected `{src}` to be rejected, parsed as {got}");
    }

    #[test]
    fn power_and_unary() {
        check_table(&[
            // `**` is left-associative (IEEE 1800-2017 Table 11-2).
            ("a ** b ** c", "(** (** a b) c)"),
            ("2 ** 3 ** 2", "(** (** 2 3) 2)"),
            // Unary operators bind tighter than `**`.
            ("-a ** b", "(** (- a) b)"),
            ("+a ** b", "(** (+ a) b)"),
            ("~a ** b", "(** (~ a) b)"),
            ("!a ** b", "(** (! a) b)"),
            ("a ** -b", "(** a (- b))"),
            ("-a ** -b ** c", "(** (** (- a) (- b)) c)"),
            // `**` binds tighter than `* / %`.
            ("a * b ** c", "(* a (** b c))"),
            ("a ** b * c", "(* (** a b) c)"),
            ("a / b ** c", "(/ a (** b c))"),
            ("a ** b % c", "(% (** a b) c)"),
        ]);
    }

    #[test]
    fn arithmetic() {
        check_table(&[
            ("a * b / c % d", "(% (/ (* a b) c) d)"),
            ("a + b * c", "(+ a (* b c))"),
            ("a * b + c", "(+ (* a b) c)"),
            ("a - b / c", "(- a (/ b c))"),
            ("a % b - c", "(- (% a b) c)"),
            ("a - b - c", "(- (- a b) c)"),
            ("a - b + c", "(+ (- a b) c)"),
            ("-a + b", "(+ (- a) b)"),
            ("a - -b", "(- a (- b))"),
            ("(a + b) * c", "(* (paren (+ a b)) c)"),
        ]);
    }

    #[test]
    fn shifts() {
        check_table(&[
            ("a << b + c", "(<< a (+ b c))"),
            ("a + b >> c", "(>> (+ a b) c)"),
            ("a <<< b - c", "(<<< a (- b c))"),
            ("a * b >>> c", "(>>> (* a b) c)"),
            ("a << b >> c", "(>> (<< a b) c)"),
            ("a >>> b <<< c", "(<<< (>>> a b) c)"),
        ]);
    }

    #[test]
    fn relational_and_equality() {
        check_table(&[
            ("a < b << c", "(< a (<< b c))"),
            ("a >> b >= c", "(>= (>> a b) c)"),
            ("a <= b + c", "(<= a (+ b c))"),
            ("a > b <<< 1", "(> a (<<< b 1))"),
            ("a < b > c", "(> (< a b) c)"),
            ("a == b < c", "(== a (< b c))"),
            ("a <= b != c", "(!= (<= a b) c)"),
            ("a === b >= c", "(=== a (>= b c))"),
            ("a > b !== c", "(!== (> a b) c)"),
            ("a == b != c", "(!= (== a b) c)"),
            ("a === b !== c", "(!== (=== a b) c)"),
            // Wildcard equality shares the equality level.
            ("a ==? b < c", "(==? a (< b c))"),
            ("a !=? b == c", "(== (!=? a b) c)"),
            ("a & b ==? c", "(& a (==? b c))"),
            ("a ==? b && c !=? d", "(&& (==? a b) (!=? c d))"),
            // `inside` is at the relational level.
            ("a == b inside {c}", "(== a (inside b c))"),
            ("a + b inside {c, d}", "(inside (+ a b) c d)"),
            ("a < b inside {c}", "(inside (< a b) c)"),
            ("a inside {b} && c", "(&& (inside a b) c)"),
        ]);
    }

    #[test]
    fn bitwise() {
        check_table(&[
            ("a & b == c", "(& a (== b c))"),
            ("a !== b & c", "(& (!== a b) c)"),
            ("a ^ b & c", "(^ a (& b c))"),
            ("a & b ^ c", "(^ (& a b) c)"),
            ("a ~^ b & c", "(~^ a (& b c))"),
            ("a ^~ b & c", "(~^ a (& b c))"),
            ("a ^ b ~^ c", "(~^ (^ a b) c)"),
            ("a | b ^ c", "(| a (^ b c))"),
            ("a ~^ b | c", "(| (~^ a b) c)"),
            ("a | b | c", "(| (| a b) c)"),
            // `~&` / `~|` are unary reductions only (no binary form in 1800-2017).
            ("a & ~&b", "(& a (~& b))"),
            ("a | ~|b", "(| a (~| b))"),
            ("a ^ ~^b", "(^ a (~^ b))"),
            ("~&a | b", "(| (~& a) b)"),
            ("a & &b", "(& a (& b))"),
            ("a | |b", "(| a (| b))"),
        ]);
        check_rejected("a ~& b");
        check_rejected("a ~| b");
    }

    #[test]
    fn logical_and_conditional() {
        check_table(&[
            ("a && b | c", "(&& a (| b c))"),
            ("a | b && c", "(&& (| a b) c)"),
            ("a || b && c", "(|| a (&& b c))"),
            ("a && b || c", "(|| (&& a b) c)"),
            ("!a && b", "(&& (! a) b)"),
            ("a || b ? c : d", "(? (|| a b) c d)"),
            ("a ? b || c : d && e", "(? a (|| b c) (&& d e))"),
            // `?:` is right-associative.
            ("a ? b : c ? d : e", "(? a b (? c d e))"),
            ("a ? b ? c : d : e", "(? a (? b c d) e)"),
            ("a ? b : c ? d : e ? f : g", "(? a b (? c d (? e f g)))"),
            ("a + b ? c : d", "(? (+ a b) c d)"),
            ("-s ? a : b", "(? (- s) a b)"),
        ]);
        check_rejected("a ? b");
    }

    #[test]
    fn postfix_binds_tightest() {
        check_table(&[
            ("~a[0]", "(~ (idx a 0))"),
            ("-a[3:0]", "(- (range a 3 0))"),
            ("&a[i +: 4]", "(& (+: a i 4))"),
            ("a[1] ** 2", "(** (idx a 1) 2)"),
            ("m[i][j]", "(idx (idx m i) j)"),
            ("m[i][j +: 2]", "(+: (idx m i) j 2)"),
        ]);
    }

    /// PAR-6: every operator class must be allowed inside `[...]`.
    #[test]
    fn every_operator_inside_index() {
        check_table(&[
            ("x[i ** 2]", "(idx x (** i 2))"),
            ("x[i * 2]", "(idx x (* i 2))"),
            ("x[i / 2]", "(idx x (/ i 2))"),
            ("x[i % 2]", "(idx x (% i 2))"),
            ("x[i + 1]", "(idx x (+ i 1))"),
            ("x[i - 1]", "(idx x (- i 1))"),
            ("x[i << 2]", "(idx x (<< i 2))"),
            ("x[i >> 2]", "(idx x (>> i 2))"),
            ("x[i <<< 2]", "(idx x (<<< i 2))"),
            ("x[i >>> 2]", "(idx x (>>> i 2))"),
            ("x[i < 3]", "(idx x (< i 3))"),
            ("x[i <= 3]", "(idx x (<= i 3))"),
            ("x[i > 3]", "(idx x (> i 3))"),
            ("x[i >= 3]", "(idx x (>= i 3))"),
            ("x[i == 0]", "(idx x (== i 0))"),
            ("x[i != 0]", "(idx x (!= i 0))"),
            ("x[i === 0]", "(idx x (=== i 0))"),
            ("x[i !== 0]", "(idx x (!== i 0))"),
            ("x[i & 3]", "(idx x (& i 3))"),
            ("x[i ^ j]", "(idx x (^ i j))"),
            ("x[i ~^ j]", "(idx x (~^ i j))"),
            ("x[i | j]", "(idx x (| i j))"),
            ("x[a && b]", "(idx x (&& a b))"),
            ("x[a || b]", "(idx x (|| a b))"),
            ("x[s ? 1 : 0]", "(idx x (? s 1 0))"),
            ("x[s ? a : t ? b : c]", "(idx x (? s a (? t b c)))"),
            ("x[-i]", "(idx x (- i))"),
            ("x[~i]", "(idx x (~ i))"),
            ("x[&i]", "(idx x (& i))"),
            ("x[i ==? 0]", "(idx x (==? i 0))"),
            ("x[i !=? 0]", "(idx x (!=? i 0))"),
            ("x[i inside {1, 2}]", "(idx x (inside i 1 2))"),
            ("x[p::W - 1]", "(idx x (- p::W 1))"),
            ("x[i + j * 2 << 1]", "(idx x (<< (+ i (* j 2)) 1))"),
            ("x[y[i + 1] - 1]", "(idx x (- (idx y (+ i 1)) 1))"),
            ("x[{a, b} | c]", "(idx x (| (concat a b) c))"),
        ]);
    }

    #[test]
    fn range_selects() {
        check_table(&[
            ("x[7:0]", "(range x 7 0)"),
            ("x[W - 1 : 0]", "(range x (- W 1) 0)"),
            ("x[i + 7 : i]", "(range x (+ i 7) i)"),
            ("x[a << 1 : b & 3]", "(range x (<< a 1) (& b 3))"),
            ("x[s ? 7 : 3 : 0]", "(range x (? s 7 3) 0)"),
            ("x[7 : s ? 1 : 0]", "(range x 7 (? s 1 0))"),
        ]);
    }

    /// PAR-6: `+:` / `-:` with arithmetic (and other) base expressions.
    #[test]
    fn indexed_part_selects() {
        check_table(&[
            ("d[i +: 8]", "(+: d i 8)"),
            ("d[i -: 8]", "(-: d i 8)"),
            ("d[i+:4]", "(+: d i 4)"),
            ("d[i-:4]", "(-: d i 4)"),
            ("d[8*i+7 -: 8]", "(-: d (+ (* 8 i) 7) 8)"),
            ("d[8*i+7 +: 8]", "(+: d (+ (* 8 i) 7) 8)"),
            ("d[i+1 +: 8]", "(+: d (+ i 1) 8)"),
            ("d[i+1 -: 8]", "(-: d (+ i 1) 8)"),
            ("d[i-1 +: 8]", "(+: d (- i 1) 8)"),
            ("d[i-1 -: W+1]", "(-: d (- i 1) (+ W 1))"),
            ("d[i - -1 +: 2]", "(+: d (- i (- 1)) 2)"),
            ("d[i*W +: W]", "(+: d (* i W) W)"),
            ("d[i ** 2 -: 4]", "(-: d (** i 2) 4)"),
            ("d[i << 3 +: 8]", "(+: d (<< i 3) 8)"),
            ("d[i >> 1 -: 8]", "(-: d (>> i 1) 8)"),
            ("d[i & 3 +: 2]", "(+: d (& i 3) 2)"),
            ("d[i | j -: 2]", "(-: d (| i j) 2)"),
            ("d[a == b +: 1]", "(+: d (== a b) 1)"),
            ("d[a && b -: 1]", "(-: d (&& a b) 1)"),
            ("d[p::W * i +: p::W]", "(+: d (* p::W i) p::W)"),
            ("d[i ==? 1 +: 1]", "(+: d (==? i 1) 1)"),
            // The whole conditional is the base; `+:` follows the else arm.
            ("d[s ? a : b +: 4]", "(+: d (? s a b) 4)"),
            ("d[s ? a : b -: 4]", "(-: d (? s a b) 4)"),
            ("d[i +: s ? 4 : 8]", "(+: d i (? s 4 8))"),
            ("d[(i + 1) * 8 -: 8]", "(-: d (* (paren (+ i 1)) 8) 8)"),
            ("d[i +: 2] + e[j -: 2]", "(+ (+: d i 2) (-: e j 2))"),
            (
                "{d[i +: 4], e[j - 1 -: 4]}",
                "(concat (+: d i 4) (-: e (- j 1) 4))",
            ),
            ("{W - 1 {d[i -: 2]}}", "(repeat (- W 1) (-: d i 2))"),
            (
                "f(d[i+1 +: 8], x[i & 3])",
                "(call f (+: d (+ i 1) 8) (idx x (& i 3)))",
            ),
        ]);
    }

    #[test]
    fn malformed_selects_are_rejected() {
        // `s ? a` is not an expression, so it cannot be a part-select base.
        check_rejected("d[s ? a +: b]");
        check_rejected("d[i +: ]");
        check_rejected("d[i +: 2 +: 3]");
        check_rejected("d[i + ]");
        // Outside brackets `+:` is never valid; the `+` is not taken as
        // a binary operator, so the expression ends before it.
        check_rejected("a + : b");
    }

    /// Parses `body` inside a module and returns its items, asserting that no
    /// diagnostics were emitted.
    fn parse_module(body: &str) -> (Vec<ModuleItem>, String, Interner) {
        let src = format!("module m; {body} endmodule");
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(&src, file, &sink);
        let mut parser = SvParser::new(tokens, &src, file, &interner, &sink);
        let ast = parser.parse_source_file();
        assert!(
            !sink.has_errors(),
            "unexpected errors for `{body}`: {:?}",
            sink.diagnostics()
        );
        let items = match ast.items.into_iter().next() {
            Some(SvItem::Module(m)) => m.items,
            other => panic!("expected module, got {other:?}"),
        };
        (items, src, interner)
    }

    fn initial_body(items: &[ModuleItem]) -> &Statement {
        items
            .iter()
            .find_map(|i| match i {
                ModuleItem::InitialBlock(b) => Some(&b.body),
                _ => None,
            })
            .expect("initial block")
    }

    /// `<=` is a comparison inside an `if` condition and on the right-hand
    /// side, but a non-blocking assignment after the statement target.
    #[test]
    fn le_comparison_vs_nonblocking_assignment() {
        let (items, src, interner) = parse_module("initial if (a <= b) q <= c <= d;");
        match initial_body(&items) {
            Statement::If {
                condition,
                then_stmt,
                ..
            } => {
                assert_eq!(sexpr(condition, &src, &interner), "(<= a b)");
                match then_stmt.as_ref() {
                    Statement::NonBlocking { target, value, .. } => {
                        assert_eq!(sexpr(target, &src, &interner), "q");
                        assert_eq!(sexpr(value, &src, &interner), "(<= c d)");
                    }
                    other => panic!("expected non-blocking assignment, got {other:?}"),
                }
            }
            other => panic!("expected if, got {other:?}"),
        }
    }

    /// Statement targets go through `parse_name_or_lvalue`, which shares
    /// `parse_postfix_index` with expressions.
    #[test]
    fn lvalue_selects() {
        let (items, src, interner) = parse_module(
            "initial begin x[i << 2] <= a; d[8*i+7 -: 8] = b; d[i+1 +: 8] <= c; x[s ? 1 : 0] = e; end",
        );
        let stmts = match initial_body(&items) {
            Statement::Block { stmts, .. } => stmts,
            other => panic!("expected block, got {other:?}"),
        };
        let targets: Vec<String> = stmts
            .iter()
            .map(|s| match s {
                Statement::Blocking { target, .. } | Statement::NonBlocking { target, .. } => {
                    sexpr(target, &src, &interner)
                }
                other => panic!("expected assignment, got {other:?}"),
            })
            .collect();
        assert_eq!(
            targets,
            [
                "(idx x (<< i 2))",
                "(-: d (+ (* 8 i) 7) 8)",
                "(+: d (+ i 1) 8)",
                "(idx x (? s 1 0))",
            ]
        );
    }

    /// Continuous-assign targets, declaration ranges and port connections
    /// accept the same select forms.
    #[test]
    fn selects_in_other_contexts() {
        let (items, src, interner) = parse_module(
            "wire [(W << 1) - 1 : 0] w; wire [W*2-1:0] v; \
             assign w[i & 3] = v[8*i+7 -: 8]; \
             sub u (.p(x[i | j]), .q(d[i+1 +: 8]));",
        );
        let assign = items
            .iter()
            .find_map(|i| match i {
                ModuleItem::ContinuousAssign(a) => Some(a),
                _ => None,
            })
            .expect("continuous assign");
        assert_eq!(sexpr(&assign.target, &src, &interner), "(idx w (& i 3))");
        assert_eq!(
            sexpr(&assign.value, &src, &interner),
            "(-: v (+ (* 8 i) 7) 8)"
        );
    }
}
