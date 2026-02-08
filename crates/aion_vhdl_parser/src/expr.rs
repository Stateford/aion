//! Pratt expression parser for VHDL-2008.
//!
//! Implements operator-precedence parsing with the following binding powers
//! (lowest to highest):
//!
//! 1. `and/or/nand/nor/xor/xnor` — cannot mix without parentheses
//! 2. `=/≠/</≤/>/≥` and matching operators — non-associative
//! 3. `sll/srl/sla/sra/rol/ror` — shift operators
//! 4. `+/-/&` — additive and concatenation
//! 5. `*/÷/mod/rem` — multiplicative
//! 6. `**` — exponentiation (right-associative)
//! 7. `not/abs/??/+/-` — unary prefix operators

use crate::ast::*;
use crate::parser::VhdlParser;
use crate::token::VhdlToken;
#[allow(unused_imports)]
use aion_common::Ident;
use aion_source::Span;

/// Binding power for binary operators. Returns (left_bp, right_bp).
/// Left-associative: left_bp < right_bp. Right-associative: left_bp > right_bp.
fn infix_binding_power(op: &BinaryOp) -> (u8, u8) {
    match op {
        // Logical — BP (1, 2)
        BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Nand
        | BinaryOp::Nor
        | BinaryOp::Xor
        | BinaryOp::Xnor => (1, 2),
        // Relational — BP (3, 4), non-associative
        BinaryOp::Eq
        | BinaryOp::Neq
        | BinaryOp::Lt
        | BinaryOp::Le
        | BinaryOp::Gt
        | BinaryOp::Ge
        | BinaryOp::MatchEq
        | BinaryOp::MatchNeq
        | BinaryOp::MatchLt
        | BinaryOp::MatchLe
        | BinaryOp::MatchGt
        | BinaryOp::MatchGe => (3, 4),
        // Shift — BP (5, 6)
        BinaryOp::Sll
        | BinaryOp::Srl
        | BinaryOp::Sla
        | BinaryOp::Sra
        | BinaryOp::Rol
        | BinaryOp::Ror => (5, 6),
        // Additive — BP (7, 8)
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Concat => (7, 8),
        // Multiplicative — BP (9, 10)
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod | BinaryOp::Rem2 => (9, 10),
        // Exponentiation — right-associative BP (12, 11)
        BinaryOp::Pow => (12, 11),
    }
}

/// Prefix binding power for unary operators.
fn prefix_binding_power(_op: &UnaryOp) -> u8 {
    11
}

impl VhdlParser<'_> {
    /// Parses an expression.
    pub fn parse_expr(&mut self) -> Expr {
        self.parse_expr_bp(0)
    }

    /// Parses an expression with minimum binding power.
    pub fn parse_expr_bp(&mut self, min_bp: u8) -> Expr {
        let mut lhs = self.parse_prefix_expr();

        loop {
            if self.at_eof() {
                break;
            }

            // Check for infix operator
            let op = match self.current_as_binary_op() {
                Some(op) => op,
                None => break,
            };

            let (l_bp, r_bp) = infix_binding_power(&op);
            if l_bp < min_bp {
                break;
            }

            let op_span = self.current_span();
            self.advance(); // consume operator token

            let rhs = self.parse_expr_bp(r_bp);
            let span = lhs.span().merge(rhs.span()).merge(op_span);
            lhs = Expr::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
                span,
            };
        }

        lhs
    }

    /// Parses a prefix expression (unary operators, literals, names, parens, aggregates).
    fn parse_prefix_expr(&mut self) -> Expr {
        let start = self.current_span();

        // Unary operators
        if let Some(op) = self.current_as_unary_op() {
            let bp = prefix_binding_power(&op);
            self.advance();
            let operand = self.parse_expr_bp(bp);
            let span = start.merge(operand.span());
            return Expr::Unary {
                op,
                operand: Box::new(operand),
                span,
            };
        }

        match self.current() {
            // Integer literal (possibly followed by a unit name for physical literals)
            VhdlToken::IntLiteral => {
                let span = self.current_span();
                self.advance();
                let lit = Expr::IntLiteral { span };
                self.maybe_physical_literal(lit, span)
            }
            // Real literal (possibly followed by a unit name for physical literals)
            VhdlToken::RealLiteral => {
                let span = self.current_span();
                self.advance();
                let lit = Expr::RealLiteral { span };
                self.maybe_physical_literal(lit, span)
            }
            // Character literal
            VhdlToken::CharLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::CharLiteral { span }
            }
            // String literal
            VhdlToken::StringLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::StringLiteral { span }
            }
            // Bit string literal
            VhdlToken::BitStringLiteral => {
                let span = self.current_span();
                self.advance();
                Expr::BitStringLiteral { span }
            }
            // Others keyword
            VhdlToken::Others => {
                let span = self.current_span();
                self.advance();
                Expr::Others { span }
            }
            // Open keyword
            VhdlToken::Open => {
                let span = self.current_span();
                self.advance();
                Expr::Open { span }
            }
            // Null keyword (used in some contexts)
            VhdlToken::Null => {
                let span = self.current_span();
                self.advance();
                Expr::Name(Name {
                    primary: self.interner.get_or_intern("null"),
                    parts: Vec::new(),
                    span,
                })
            }
            // Parenthesized expression or aggregate
            VhdlToken::LeftParen => self.parse_paren_or_aggregate(),
            // Name (identifier)
            VhdlToken::Identifier | VhdlToken::ExtendedIdentifier => self.parse_name_expr(),
            _ => {
                let span = self.current_span();
                self.error("expected expression");
                self.advance();
                Expr::Error(span)
            }
        }
    }

    /// Parses a parenthesized expression or aggregate.
    fn parse_paren_or_aggregate(&mut self) -> Expr {
        let start = self.current_span();
        self.expect(VhdlToken::LeftParen);

        // Check for empty parens (error)
        if self.at(VhdlToken::RightParen) {
            let end = self.current_span();
            self.advance();
            return Expr::Aggregate {
                elements: Vec::new(),
                span: start.merge(end),
            };
        }

        // Parse first element
        let first_expr = self.parse_expr();

        // If we see `=>`, this is an aggregate with named associations
        if self.at(VhdlToken::Arrow) {
            return self.parse_aggregate_after_first_choice(start, first_expr);
        }

        // If we see a comma, this could be an aggregate with positional elements
        if self.at(VhdlToken::Comma) {
            return self.parse_aggregate_positional(start, first_expr);
        }

        // Otherwise it's a parenthesized expression
        self.expect(VhdlToken::RightParen);
        let span = start.merge(self.prev_span());
        Expr::Paren {
            inner: Box::new(first_expr),
            span,
        }
    }

    /// Finishes parsing an aggregate after seeing the first choice and `=>`.
    fn parse_aggregate_after_first_choice(&mut self, start: Span, first_choice: Expr) -> Expr {
        let mut elements = Vec::new();

        // First element: first_choice => value
        self.expect(VhdlToken::Arrow);
        let value = self.parse_expr();
        let elem_span = first_choice.span().merge(value.span());
        let choices = vec![self.expr_to_choice(first_choice)];
        elements.push(AggregateElement {
            choices,
            value,
            span: elem_span,
        });

        // Remaining elements
        while self.eat(VhdlToken::Comma) {
            let elem = self.parse_aggregate_element();
            elements.push(elem);
        }

        self.expect(VhdlToken::RightParen);
        let span = start.merge(self.prev_span());
        Expr::Aggregate { elements, span }
    }

    /// Parses a positional aggregate (no `=>` associations).
    fn parse_aggregate_positional(&mut self, start: Span, first_expr: Expr) -> Expr {
        let mut elements = Vec::new();
        let first_span = first_expr.span();
        elements.push(AggregateElement {
            choices: Vec::new(),
            value: first_expr,
            span: first_span,
        });

        while self.eat(VhdlToken::Comma) {
            let value = self.parse_expr();
            let span = value.span();
            elements.push(AggregateElement {
                choices: Vec::new(),
                value,
                span,
            });
        }

        self.expect(VhdlToken::RightParen);
        let span = start.merge(self.prev_span());
        Expr::Aggregate { elements, span }
    }

    /// Parses a single aggregate element (choices => value or just value).
    fn parse_aggregate_element(&mut self) -> AggregateElement {
        let expr = self.parse_expr();

        if self.at(VhdlToken::Arrow) {
            self.advance();
            let value = self.parse_expr();
            let span = expr.span().merge(value.span());
            let choices = vec![self.expr_to_choice(expr)];
            AggregateElement {
                choices,
                value,
                span,
            }
        } else {
            let span = expr.span();
            AggregateElement {
                choices: Vec::new(),
                value: expr,
                span,
            }
        }
    }

    /// Converts an expression to a choice (for aggregates/case statements).
    pub(crate) fn expr_to_choice(&self, expr: Expr) -> Choice {
        match &expr {
            Expr::Others { span } => Choice::Others(*span),
            _ => Choice::Expr(expr),
        }
    }

    /// Parses a name expression (identifier with optional suffixes).
    pub(crate) fn parse_name_expr(&mut self) -> Expr {
        let start = self.current_span();
        let primary = self.expect_ident();

        let mut parts = Vec::new();

        loop {
            match self.current() {
                // Dot selection: name.field or name.all
                VhdlToken::Dot => {
                    self.advance();
                    if self.at(VhdlToken::All) {
                        let span = self.current_span();
                        self.advance();
                        parts.push(NameSuffix::All(start.merge(span)));
                    } else {
                        let field = self.expect_ident();
                        let span = start.merge(self.prev_span());
                        parts.push(NameSuffix::Selected(field, span));
                    }
                }
                // Tick: name'attribute or qualified expression
                VhdlToken::Tick => {
                    // Look ahead to distinguish attribute from qualified expression
                    // name'attr vs type_mark'(expr)
                    if self.peek_is(VhdlToken::LeftParen) {
                        // Qualified expression: type_mark'(expr)
                        self.advance(); // eat tick
                        let type_mark = self.make_selected_name(start, primary, &parts);
                        let expr = self.parse_paren_or_aggregate();
                        let span = start.merge(expr.span());
                        return Expr::Qualified {
                            type_mark,
                            expr: Box::new(expr),
                            span,
                        };
                    } else if self.peek_is_ident_or_keyword() {
                        self.advance(); // eat tick
                        let attr = self.expect_ident_or_keyword();
                        let arg = if self.at(VhdlToken::LeftParen) {
                            self.advance();
                            let arg = self.parse_expr();
                            self.expect(VhdlToken::RightParen);
                            Some(Box::new(arg))
                        } else {
                            None
                        };
                        let span = start.merge(self.prev_span());
                        parts.push(NameSuffix::Attribute(attr, arg, span));
                    } else {
                        break;
                    }
                }
                // Parenthesized: name(args) — could be index, slice, or function call
                VhdlToken::LeftParen => {
                    self.advance(); // eat (

                    // Try to detect slice: expr direction expr
                    let first = self.parse_expr();
                    if self.at(VhdlToken::To) || self.at(VhdlToken::Downto) {
                        // This is a slice
                        let direction = if self.eat(VhdlToken::To) {
                            RangeDirection::To
                        } else {
                            self.advance(); // eat downto
                            RangeDirection::Downto
                        };
                        let right = self.parse_expr();
                        self.expect(VhdlToken::RightParen);
                        let span = start.merge(self.prev_span());
                        let range = RangeConstraint {
                            left: Box::new(first),
                            direction,
                            right: Box::new(right),
                            span,
                        };
                        parts.push(NameSuffix::Slice(range, span));
                    } else if self.at(VhdlToken::Comma) {
                        // Multiple indices
                        let mut args = vec![first];
                        while self.eat(VhdlToken::Comma) {
                            args.push(self.parse_expr());
                        }
                        self.expect(VhdlToken::RightParen);
                        let span = start.merge(self.prev_span());
                        parts.push(NameSuffix::Index(args, span));
                    } else {
                        // Single index
                        self.expect(VhdlToken::RightParen);
                        let span = start.merge(self.prev_span());
                        parts.push(NameSuffix::Index(vec![first], span));
                    }
                }
                _ => break,
            }
        }

        let span = if parts.is_empty() {
            start
        } else {
            start.merge(self.prev_span())
        };

        Expr::Name(Name {
            primary,
            parts,
            span,
        })
    }

    /// Creates a SelectedName from a primary ident and name parts parsed so far.
    fn make_selected_name(
        &self,
        start: Span,
        primary: Ident,
        parts: &[NameSuffix],
    ) -> SelectedName {
        let mut names = vec![primary];
        for part in parts {
            if let NameSuffix::Selected(id, _) = part {
                names.push(*id);
            }
        }
        SelectedName {
            parts: names,
            span: start.merge(self.prev_span()),
        }
    }

    /// Maps the current token to a binary operator, if applicable.
    fn current_as_binary_op(&self) -> Option<BinaryOp> {
        match self.current() {
            VhdlToken::And => Some(BinaryOp::And),
            VhdlToken::Or => Some(BinaryOp::Or),
            VhdlToken::Nand => Some(BinaryOp::Nand),
            VhdlToken::Nor => Some(BinaryOp::Nor),
            VhdlToken::Xor => Some(BinaryOp::Xor),
            VhdlToken::Xnor => Some(BinaryOp::Xnor),
            VhdlToken::Equals => Some(BinaryOp::Eq),
            VhdlToken::SlashEquals => Some(BinaryOp::Neq),
            VhdlToken::LessThan => Some(BinaryOp::Lt),
            VhdlToken::LessEquals => Some(BinaryOp::Le),
            VhdlToken::GreaterThan => Some(BinaryOp::Gt),
            VhdlToken::GreaterEquals => Some(BinaryOp::Ge),
            VhdlToken::MatchEquals => Some(BinaryOp::MatchEq),
            VhdlToken::MatchSlashEquals => Some(BinaryOp::MatchNeq),
            VhdlToken::MatchLess => Some(BinaryOp::MatchLt),
            VhdlToken::MatchLessEquals => Some(BinaryOp::MatchLe),
            VhdlToken::MatchGreater => Some(BinaryOp::MatchGt),
            VhdlToken::MatchGreaterEquals => Some(BinaryOp::MatchGe),
            VhdlToken::Sll => Some(BinaryOp::Sll),
            VhdlToken::Srl => Some(BinaryOp::Srl),
            VhdlToken::Sla => Some(BinaryOp::Sla),
            VhdlToken::Sra => Some(BinaryOp::Sra),
            VhdlToken::Rol => Some(BinaryOp::Rol),
            VhdlToken::Ror => Some(BinaryOp::Ror),
            VhdlToken::Plus => Some(BinaryOp::Add),
            VhdlToken::Minus => Some(BinaryOp::Sub),
            VhdlToken::Ampersand => Some(BinaryOp::Concat),
            VhdlToken::Star => Some(BinaryOp::Mul),
            VhdlToken::Slash => Some(BinaryOp::Div),
            VhdlToken::Mod => Some(BinaryOp::Mod),
            VhdlToken::Rem => Some(BinaryOp::Rem2),
            VhdlToken::DoubleStar => Some(BinaryOp::Pow),
            _ => None,
        }
    }

    /// Maps the current token to a unary operator, if applicable.
    fn current_as_unary_op(&self) -> Option<UnaryOp> {
        match self.current() {
            VhdlToken::Not => Some(UnaryOp::Not),
            VhdlToken::Abs => Some(UnaryOp::Abs),
            VhdlToken::ConditionOp => Some(UnaryOp::Condition),
            VhdlToken::Plus => Some(UnaryOp::Pos),
            VhdlToken::Minus => Some(UnaryOp::Neg),
            _ => None,
        }
    }

    /// If the current token is an identifier after a numeric literal, treat it
    /// as a VHDL physical literal unit (e.g., `10 ns`, `5.0 MHz`).
    ///
    /// Returns a `Binary(Mul, literal, unit_name)` to represent the physical value.
    fn maybe_physical_literal(&mut self, lit: Expr, lit_span: Span) -> Expr {
        if self.at(VhdlToken::Identifier) {
            // Only treat as physical literal if this looks like a unit name
            // (simple identifier, not followed by operators that would make it a name expr)
            let unit_span = self.current_span();
            let unit_name = self.expect_ident();
            let span = lit_span.merge(unit_span);
            Expr::Binary {
                left: Box::new(lit),
                op: BinaryOp::Mul,
                right: Box::new(Expr::Name(Name {
                    primary: unit_name,
                    parts: Vec::new(),
                    span: unit_span,
                })),
                span,
            }
        } else {
            lit
        }
    }
}
