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
use aion_diagnostics::code::{Category, DiagnosticCode};
use aion_diagnostics::Diagnostic;
use aion_source::Span;

/// Binding power of the postfix operators `++` and `--`.
///
/// Higher than every prefix and infix binding power, so `-a++` parses as
/// `-(a++)` and `a + b++` as `a + (b++)`.
const POSTFIX_BP: u8 = 25;

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
                if POSTFIX_BP < min_bp {
                    break;
                }
                let increment = self.at(SvToken::PlusPlus);
                let op_span = self.current_span();
                self.advance();
                self.check_incr_decr_operand(&lhs, op_span);
                let span = lhs.span().merge(op_span);
                lhs = Expr::Unary {
                    op: if increment {
                        UnaryOp::PostIncr
                    } else {
                        UnaryOp::PostDecr
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
            self.check_incr_decr_operand(&operand, start);
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
            self.check_incr_decr_operand(&operand, start);
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
            // Unbased unsized literal: '0, '1, 'x, 'z
            SvToken::UnbasedUnsizedLiteral => {
                let span = self.current_span();
                let value = match self.current_text().as_bytes().get(1) {
                    Some(b'1') => FillValue::One,
                    Some(b'x' | b'X') => FillValue::X,
                    Some(b'z' | b'Z') => FillValue::Z,
                    _ => FillValue::Zero,
                };
                self.advance();
                Expr::UnbasedUnsized { value, span }
            }
            // Assignment pattern: '{ ... }
            SvToken::Tick if self.peek_is(SvToken::LeftBrace) => self.parse_assignment_pattern(),
            // A tick that starts neither a fill literal nor a pattern (e.g. `'q`)
            SvToken::Tick => self.recover_bad_tick(),
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

    /// Parses a dot suffix: a hierarchical name or a member selection.
    ///
    /// A plain identifier or hierarchical name base absorbs every following
    /// `.ident` into one [`Expr::HierarchicalName`]. Any other base (index,
    /// range select, scoped name, call, ...) gets a single [`Expr::Member`]
    /// for one `.ident`; callers loop, so chains such as `a[1].b[2].c` nest.
    pub(crate) fn parse_dot_suffix(&mut self, base: Expr) -> Expr {
        let start = base.span();
        let mut parts = match base {
            Expr::Identifier { name, .. } => vec![name],
            Expr::HierarchicalName { parts, .. } => parts,
            _ => {
                self.advance(); // eat .
                let member = self.expect_ident();
                let span = start.merge(self.prev_span());
                return Expr::Member {
                    base: Box::new(base),
                    member,
                    span,
                };
            }
        };

        while self.eat(SvToken::Dot) {
            parts.push(self.expect_ident());
        }

        let span = start.merge(self.prev_span());
        Expr::HierarchicalName { parts, span }
    }

    /// Parses an assignment pattern `'{ item {, item} }`.
    ///
    /// Items are positional (`expr`), keyed (`key: expr`), or `default: expr`.
    /// Mixing positional items with keyed or `default` items, a repeated
    /// `default`, and an empty pattern are reported as errors; parsing then
    /// continues after the closing brace.
    fn parse_assignment_pattern(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // eat '
        self.advance(); // eat {

        if self.at(SvToken::RightBrace) {
            self.advance();
            let span = start.merge(self.prev_span());
            self.error_at("empty assignment pattern", span);
            return Expr::Error(span);
        }

        let mut items = Vec::new();
        let mut has_positional = false;
        let mut has_named = false;
        let mut has_default = false;
        loop {
            let item_start = self.current_span();
            if self.at(SvToken::Default) && self.peek_is(SvToken::Colon) {
                self.advance(); // eat default
                self.advance(); // eat :
                let value = self.parse_expr();
                let span = item_start.merge(self.prev_span());
                if has_default {
                    self.error_at("duplicate `default` in assignment pattern", span);
                }
                has_default = true;
                has_named = true;
                items.push(PatternItem::Default { value, span });
            } else {
                let first = self.parse_expr();
                if self.eat(SvToken::Colon) {
                    let value = self.parse_expr();
                    let span = item_start.merge(self.prev_span());
                    has_named = true;
                    items.push(PatternItem::Keyed {
                        key: first,
                        value,
                        span,
                    });
                } else {
                    has_positional = true;
                    items.push(PatternItem::Positional(first));
                }
            }
            if !self.eat(SvToken::Comma) || self.at_eof() {
                break;
            }
        }
        self.expect(SvToken::RightBrace);
        let span = start.merge(self.prev_span());
        if has_positional && has_named {
            self.error_at(
                "assignment pattern cannot mix positional items with keyed or `default` items",
                span,
            );
        }
        Expr::AssignmentPattern { items, span }
    }

    /// Recovers from a tick that starts neither an unbased unsized literal
    /// nor an assignment pattern (e.g. `'q`, `'10`).
    ///
    /// Reports an error and consumes the tick plus a directly adjacent
    /// identifier or number, so the rest of the statement still parses.
    fn recover_bad_tick(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // eat '
        let adjacent = self.current_span().start == start.end
            && matches!(
                self.current(),
                SvToken::Identifier | SvToken::IntLiteral | SvToken::RealLiteral
            );
        if adjacent {
            self.advance();
        }
        let span = start.merge(self.prev_span());
        self.error_at(
            "invalid literal after `'`: expected `'0`, `'1`, `'x`, `'z`, or an assignment pattern `'{...}`",
            span,
        );
        Expr::Error(span)
    }

    /// Reports an error if the operand of `++`/`--` is not a variable reference.
    fn check_incr_decr_operand(&self, operand: &Expr, op_span: Span) {
        let ok = matches!(
            operand,
            Expr::Identifier { .. }
                | Expr::HierarchicalName { .. }
                | Expr::ScopedIdent { .. }
                | Expr::Index { .. }
                | Expr::RangeSelect { .. }
                | Expr::PartSelect { .. }
                | Expr::Member { .. }
                | Expr::Error(_)
        );
        if !ok {
            self.error_at(
                "operand of `++`/`--` must be a variable",
                op_span.merge(operand.span()),
            );
        }
    }

    /// Emits a parse error (E101) at an explicit span.
    fn error_at(&self, msg: &str, span: Span) {
        self.sink.emit(Diagnostic::error(
            DiagnosticCode::new(Category::Error, 101),
            msg,
            span,
        ));
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

    // --- Issue #30: member selection, fill literals, patterns, postfix ++/-- ---

    /// Parses `source` as an expression and returns it with all diagnostics.
    fn parse_expr_diags(source: &str) -> (Expr, Vec<aion_diagnostics::Diagnostic>, Interner) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = SvParser::new(tokens, source, file, &interner, &sink);
        let expr = parser.parse_expr();
        (expr, sink.diagnostics(), interner)
    }

    /// Returns the member name of a `Member` node, or panics.
    fn member_name(expr: &Expr, interner: &Interner) -> String {
        match expr {
            Expr::Member { member, .. } => interner.resolve(*member).to_string(),
            other => panic!("expected member, got {other:?}"),
        }
    }

    #[test]
    fn member_on_index_keeps_member() {
        let (expr, diags, interner) = parse_expr_diags("arr[2].valid");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(member_name(&expr, &interner), "valid");
        match expr {
            Expr::Member { base, .. } => assert!(matches!(*base, Expr::Index { .. })),
            _ => unreachable!(),
        }
    }

    #[test]
    fn member_on_range_select_and_call() {
        let (expr, diags, interner) = parse_expr_diags("v[3:0].x");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(member_name(&expr, &interner), "x");
        let (expr, diags, interner) = parse_expr_diags("f(a).field");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(member_name(&expr, &interner), "field");
        let (expr, diags, interner) = parse_expr_diags("pkg::cfg.mode");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(member_name(&expr, &interner), "mode");
    }

    #[test]
    fn member_chain_nests() {
        // a[1].b[2].c == Member(Index(Member(Index(a, 1), b), 2), c)
        let (expr, diags, interner) = parse_expr_diags("a[1].b[2].c");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(member_name(&expr, &interner), "c");
        let Expr::Member { base, .. } = expr else {
            unreachable!()
        };
        let Expr::Index { base: inner, .. } = *base else {
            panic!("expected index")
        };
        assert_eq!(member_name(&inner, &interner), "b");
        let Expr::Member { base: inner2, .. } = *inner else {
            unreachable!()
        };
        assert!(matches!(*inner2, Expr::Index { .. }));
    }

    #[test]
    fn member_after_hierarchical_index() {
        let (expr, diags, interner) = parse_expr_diags("gen_blk[0].u.sig");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(member_name(&expr, &interner), "sig");
    }

    #[test]
    fn member_missing_name_reports_error() {
        let (_, diags, _) = parse_expr_diags("arr[0].");
        assert!(!diags.is_empty());
    }

    #[test]
    fn postfix_increment_is_post() {
        let expr = parse_expr_str("b++");
        assert!(matches!(
            expr,
            Expr::Unary {
                op: UnaryOp::PostIncr,
                ..
            }
        ));
        let expr = parse_expr_str("b--");
        assert!(matches!(
            expr,
            Expr::Unary {
                op: UnaryOp::PostDecr,
                ..
            }
        ));
    }

    #[test]
    fn postfix_binds_tighter_than_prefix_and_infix() {
        // -b++ is -(b++)
        match parse_expr_str("-b++") {
            Expr::Unary {
                op: UnaryOp::Minus,
                operand,
                ..
            } => assert!(matches!(
                *operand,
                Expr::Unary {
                    op: UnaryOp::PostIncr,
                    ..
                }
            )),
            other => panic!("expected minus, got {other:?}"),
        }
        // a + b++ is a + (b++)
        match parse_expr_str("a + b++") {
            Expr::Binary {
                op: BinaryOp::Add,
                right,
                ..
            } => assert!(matches!(
                *right,
                Expr::Unary {
                    op: UnaryOp::PostIncr,
                    ..
                }
            )),
            other => panic!("expected add, got {other:?}"),
        }
        // arr[i]++ applies to the select
        match parse_expr_str("arr[i]++") {
            Expr::Unary {
                op: UnaryOp::PostIncr,
                operand,
                ..
            } => assert!(matches!(*operand, Expr::Index { .. })),
            other => panic!("expected post-incr, got {other:?}"),
        }
    }

    #[test]
    fn incr_decr_on_non_variable_is_error() {
        let (_, diags, _) = parse_expr_diags("3++");
        assert_eq!(diags.len(), 1, "{diags:?}");
        let (_, diags, _) = parse_expr_diags("--(a + b)");
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn unbased_unsized_values() {
        for (src, want) in [
            ("'0", FillValue::Zero),
            ("'1", FillValue::One),
            ("'x", FillValue::X),
            ("'X", FillValue::X),
            ("'z", FillValue::Z),
            ("'Z", FillValue::Z),
        ] {
            match parse_expr_str(src) {
                Expr::UnbasedUnsized { value, .. } => assert_eq!(value, want, "{src}"),
                other => panic!("{src}: expected fill literal, got {other:?}"),
            }
        }
    }

    #[test]
    fn unbased_unsized_in_expressions() {
        match parse_expr_str("a == '1") {
            Expr::Binary { right, .. } => assert!(matches!(
                *right,
                Expr::UnbasedUnsized {
                    value: FillValue::One,
                    ..
                }
            )),
            other => panic!("expected binary, got {other:?}"),
        }
        assert!(matches!(
            parse_expr_str("rst ? '0 : d"),
            Expr::Ternary { .. }
        ));
    }

    #[test]
    fn bad_tick_literal_is_error_and_recovers() {
        for src in ["'q", "'10"] {
            let (expr, diags, _) = parse_expr_diags(src);
            assert!(matches!(expr, Expr::Error(_)), "{src}: {expr:?}");
            assert_eq!(diags.len(), 1, "{src}: {diags:?}");
            assert!(diags[0].message.contains("'{...}"), "{src}: {diags:?}");
        }
        // The adjacent identifier is consumed, so `'q + 1` reports only one error.
        let (expr, diags, _) = parse_expr_diags("'q + 1");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(matches!(expr, Expr::Binary { .. }));
    }

    #[test]
    fn assignment_pattern_positional() {
        match parse_expr_str("'{1, 2, 3}") {
            Expr::AssignmentPattern { items, .. } => {
                assert_eq!(items.len(), 3);
                assert!(items
                    .iter()
                    .all(|i| matches!(i, PatternItem::Positional(_))));
            }
            other => panic!("expected pattern, got {other:?}"),
        }
    }

    #[test]
    fn assignment_pattern_named_and_default() {
        match parse_expr_str("'{a: 1, b: 2}") {
            Expr::AssignmentPattern { items, .. } => {
                assert_eq!(items.len(), 2);
                assert!(items.iter().all(|i| matches!(i, PatternItem::Keyed { .. })));
            }
            other => panic!("expected pattern, got {other:?}"),
        }
        match parse_expr_str("'{default: 0}") {
            Expr::AssignmentPattern { items, .. } => {
                assert!(matches!(items[..], [PatternItem::Default { .. }]));
            }
            other => panic!("expected pattern, got {other:?}"),
        }
        match parse_expr_str("'{a: '1, default: '0}") {
            Expr::AssignmentPattern { items, .. } => {
                assert!(matches!(items[0], PatternItem::Keyed { .. }));
                assert!(matches!(
                    items[1],
                    PatternItem::Default {
                        value: Expr::UnbasedUnsized { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected pattern, got {other:?}"),
        }
    }

    #[test]
    fn assignment_pattern_nested() {
        match parse_expr_str("'{'{1, 2}, '{3, sel ? a : b}}") {
            Expr::AssignmentPattern { items, .. } => {
                assert_eq!(items.len(), 2);
                assert!(matches!(
                    items[1],
                    PatternItem::Positional(Expr::AssignmentPattern { .. })
                ));
            }
            other => panic!("expected pattern, got {other:?}"),
        }
    }

    #[test]
    fn assignment_pattern_unterminated_is_error() {
        let (expr, diags, _) = parse_expr_diags("'{1, 2");
        assert!(matches!(expr, Expr::AssignmentPattern { .. }));
        assert!(!diags.is_empty());
        let (_, diags, _) = parse_expr_diags("'{a: ");
        assert!(!diags.is_empty());
    }

    #[test]
    fn assignment_pattern_invalid_shapes_are_errors() {
        let (expr, diags, _) = parse_expr_diags("'{}");
        assert!(matches!(expr, Expr::Error(_)));
        assert_eq!(diags.len(), 1, "{diags:?}");
        let (_, diags, _) = parse_expr_diags("'{1, default: 0}");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("mix"));
        let (_, diags, _) = parse_expr_diags("'{default: 0, default: 1}");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("duplicate"));
    }

    #[test]
    fn casts_still_parse_after_tick_changes() {
        assert!(matches!(parse_expr_str("8'(x)"), Expr::Cast { .. }));
        assert!(matches!(parse_expr_str("WIDTH'(a + b)"), Expr::Cast { .. }));
        assert!(matches!(parse_expr_str("'hFF"), Expr::Literal { .. }));
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
