//! Pratt expression parser for VHDL-2008.
//!
//! Implements the expression grammar of IEEE 1076-2008 §9.1 with the operator
//! precedence of §9.2. Binding powers, lowest to highest:
//!
//! | Level | Operators | Binding power | Associativity |
//! |-------|-----------|---------------|---------------|
//! | logical | `and or nand nor xor xnor` | (1, 2) | same op only; `nand`/`nor` none |
//! | relational | `= /= < <= > >= ?= ?/= ?< ?<= ?> ?>=` | (3, 4) | none |
//! | shift | `sll srl sla sra rol ror` | (5, 6) | none |
//! | adding | binary `+ - &` | (7, 8) | left |
//! | sign | unary `+ -` | prefix 9 | — |
//! | multiplying | `* / mod rem` | (10, 11) | left |
//! | exponent | `**` | (12, 13) | none |
//! | primary prefix | `not abs ??`, reductions `and or nand nor xor xnor` | prefix 14 | — |
//!
//! Consequences of the grammar that the parser enforces (each violation is
//! reported as an error, and a tree is still built so parsing continues):
//!
//! - A sign applies to a whole term: `-a mod b` is `-(a mod b)`, while
//!   `-a + b` is `(-a) + b`.
//! - A sign may only begin a simple expression, so it cannot directly follow
//!   an adding, multiplying or `**` operator (`a + -b`, `a * -b`, `a ** -b`);
//!   the operand must be parenthesized: `a + (-b)`.
//! - Logical operators cannot be mixed without parentheses
//!   (`a and b or c`), and `nand`/`nor` cannot be chained (`a nand b nand c`);
//!   a chain of one associative operator (`a and b and c`) is legal.
//! - Relational, shift and `**` operators are non-associative:
//!   `a = b = c`, `a sll 1 sll 2` and `2 ** 3 ** 2` are rejected.
//! - `not`, `abs`, `??` and the reduction operators take a *primary* operand,
//!   so they bind tighter than `**` and cannot be combined with it
//!   (`abs a ** 2`, `a ** not b`) or nested (`not not a`) without parentheses.

use crate::ast::*;
use crate::parser::VhdlParser;
use crate::token::VhdlToken;
#[allow(unused_imports)]
use aion_common::Ident;
use aion_source::Span;

/// Precedence level of a binary operator (IEEE 1076-2008 §9.2), lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpLevel {
    /// `and or nand nor xor xnor`.
    Logical,
    /// `= /= < <= > >=` and the matching relational operators.
    Relational,
    /// `sll srl sla sra rol ror`.
    Shift,
    /// Binary `+ - &`.
    Adding,
    /// `* / mod rem`.
    Multiplying,
    /// `**`.
    Exponent,
}

/// Right binding power of shift operators. A sign is only legal where the
/// minimum binding power is at most this, i.e. at the start of a
/// `simple_expression` (top level or after a logical/relational/shift op).
const SHIFT_RBP: u8 = 6;

/// Prefix binding power of a sign: its operand is parsed as a `term`, so it
/// absorbs multiplying operators and `**` but stops at adding operators.
const SIGN_BP: u8 = 9;

/// Right binding power of multiplying operators. `not`/`abs`/`??`/reductions
/// are factors, so they are legal where the minimum binding power is at most
/// this, but not as the operand of `**` or of another primary-prefix operator.
const MULTIPLYING_RBP: u8 = 11;

/// Prefix binding power of `not`, `abs`, `??` and the reduction operators:
/// higher than every infix operator, so the operand is a single primary.
const PRIMARY_PREFIX_BP: u8 = 14;

/// Returns the precedence level of a binary operator.
fn op_level(op: BinaryOp) -> OpLevel {
    match op {
        BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Nand
        | BinaryOp::Nor
        | BinaryOp::Xor
        | BinaryOp::Xnor => OpLevel::Logical,
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
        | BinaryOp::MatchGe => OpLevel::Relational,
        BinaryOp::Sll
        | BinaryOp::Srl
        | BinaryOp::Sla
        | BinaryOp::Sra
        | BinaryOp::Rol
        | BinaryOp::Ror => OpLevel::Shift,
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Concat => OpLevel::Adding,
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod | BinaryOp::Rem2 => OpLevel::Multiplying,
        BinaryOp::Pow => OpLevel::Exponent,
    }
}

/// Binding power for binary operators. Returns (left_bp, right_bp).
///
/// Every level is parsed left-leaning (`left_bp < right_bp`); the levels that
/// the LRM makes non-associative (or only same-operator associative) are
/// policed separately by `VhdlParser::check_operator_chain`.
fn infix_binding_power(op: BinaryOp) -> (u8, u8) {
    match op_level(op) {
        OpLevel::Logical => (1, 2),
        OpLevel::Relational => (3, 4),
        OpLevel::Shift => (5, SHIFT_RBP),
        OpLevel::Adding => (7, 8),
        OpLevel::Multiplying => (10, MULTIPLYING_RBP),
        OpLevel::Exponent => (12, 13),
    }
}

/// Returns `true` for a sign (`+`/`-`), which applies to a whole term.
fn is_sign(op: UnaryOp) -> bool {
    matches!(op, UnaryOp::Pos | UnaryOp::Neg)
}

/// Prefix binding power for unary operators.
///
/// A sign binds between the adding and multiplying levels
/// (`simple_expression ::= [sign] term {adding_operator term}`), while
/// `not`, `abs`, `??` and the reduction operators take a single primary.
fn prefix_binding_power(op: UnaryOp) -> u8 {
    if is_sign(op) {
        SIGN_BP
    } else {
        PRIMARY_PREFIX_BP
    }
}

/// Returns the VHDL spelling of a binary operator, for diagnostics.
fn binary_op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::And => "and",
        BinaryOp::Or => "or",
        BinaryOp::Nand => "nand",
        BinaryOp::Nor => "nor",
        BinaryOp::Xor => "xor",
        BinaryOp::Xnor => "xnor",
        BinaryOp::Eq => "=",
        BinaryOp::Neq => "/=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::MatchEq => "?=",
        BinaryOp::MatchNeq => "?/=",
        BinaryOp::MatchLt => "?<",
        BinaryOp::MatchLe => "?<=",
        BinaryOp::MatchGt => "?>",
        BinaryOp::MatchGe => "?>=",
        BinaryOp::Sll => "sll",
        BinaryOp::Srl => "srl",
        BinaryOp::Sla => "sla",
        BinaryOp::Sra => "sra",
        BinaryOp::Rol => "rol",
        BinaryOp::Ror => "ror",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Concat => "&",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "mod",
        BinaryOp::Rem2 => "rem",
        BinaryOp::Pow => "**",
    }
}

/// Returns the VHDL spelling of a unary operator, for diagnostics.
fn unary_op_text(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Not => "not",
        UnaryOp::Abs => "abs",
        UnaryOp::Pos => "+",
        UnaryOp::Neg => "-",
        UnaryOp::Condition => "??",
        UnaryOp::RedAnd => "and",
        UnaryOp::RedOr => "or",
        UnaryOp::RedNand => "nand",
        UnaryOp::RedNor => "nor",
        UnaryOp::RedXor => "xor",
        UnaryOp::RedXnor => "xnor",
    }
}

impl VhdlParser<'_> {
    /// Parses an expression.
    pub fn parse_expr(&mut self) -> Expr {
        self.parse_expr_bp(0)
    }

    /// Parses an expression whose infix operators all have a left binding
    /// power of at least `min_bp`, enforcing the LRM §9.1 restrictions on
    /// mixing and chaining operators of one precedence level.
    pub fn parse_expr_bp(&mut self, min_bp: u8) -> Expr {
        let mut lhs = self.parse_prefix_expr(min_bp);
        // `not a`, `abs a`, `?? a` and reductions are complete factors and
        // cannot be the left operand of `**` (`factor ::= abs primary | ...`).
        let lhs_primary_prefix = match &lhs {
            Expr::Unary { op, .. } if !is_sign(*op) => Some(*op),
            _ => None,
        };
        // The operator that built the current `lhs` in this invocation.
        let mut prev_op: Option<BinaryOp> = None;

        loop {
            if self.at_eof() {
                break;
            }

            // Check for infix operator
            let op = match self.current_as_binary_op() {
                Some(op) => op,
                None => break,
            };

            let (l_bp, r_bp) = infix_binding_power(op);
            if l_bp < min_bp {
                break;
            }

            match (prev_op, lhs_primary_prefix) {
                (Some(prev), _) => self.check_operator_chain(prev, op),
                (None, Some(un)) if op == BinaryOp::Pow => self.error(&format!(
                    "`**` cannot follow a factor built with `{t}`: `{t}` takes a primary operand and \
                     has the same precedence as `**`; write `({t} x) ** y` or `{t} (x ** y)`",
                    t = unary_op_text(un)
                )),
                _ => {}
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
            prev_op = Some(op);
        }

        lhs
    }

    /// Reports an error when binary operator `next` (the current token)
    /// follows an expression built with `prev` at the same precedence level in
    /// a combination the LRM forbids without parentheses.
    ///
    /// Logical operators may only repeat the same associative operator
    /// (`a and b and c`); `nand`/`nor` may not repeat at all. Relational,
    /// shift and `**` operators are non-associative. Adding and multiplying
    /// operators are freely left-associative.
    fn check_operator_chain(&self, prev: BinaryOp, next: BinaryOp) {
        let level = op_level(next);
        if op_level(prev) != level {
            return;
        }
        let (p, n) = (binary_op_text(prev), binary_op_text(next));
        match level {
            OpLevel::Logical if prev != next => self.error(&format!(
                "cannot mix logical operators `{p}` and `{n}` without parentheses; \
                 write `(a {p} b) {n} c` or `a {p} (b {n} c)`"
            )),
            OpLevel::Logical if matches!(next, BinaryOp::Nand | BinaryOp::Nor) => {
                self.error(&format!(
                    "`{n}` is not associative and cannot be chained; \
                     write `(a {n} b) {n} c` or `a {n} (b {n} c)`"
                ))
            }
            OpLevel::Relational => self.error(&format!(
                "relational operators are not associative: `{n}` cannot follow a `{p}` \
                 comparison without parentheses; write `(a {p} b) {n} c`"
            )),
            OpLevel::Shift => self.error(&format!(
                "shift operators are not associative: `{n}` cannot follow a `{p}` \
                 shift without parentheses; write `(a {p} b) {n} c`"
            )),
            OpLevel::Exponent => self.error(
                "`**` is not associative and cannot be chained; \
                 write `(a ** b) ** c` or `a ** (b ** c)`",
            ),
            _ => {}
        }
    }

    /// Parses a prefix expression (unary operators, literals, names, parens,
    /// aggregates) in a context whose minimum binding power is `min_bp`.
    ///
    /// `min_bp` identifies the grammar position the operand occupies, so a
    /// sign outside a `simple_expression` start (`a + -b`) and a
    /// primary-prefix operator in a primary-only position (`a ** not b`) are
    /// reported. The unary node is still built so the rest of the expression
    /// parses.
    fn parse_prefix_expr(&mut self, min_bp: u8) -> Expr {
        let start = self.current_span();

        // Unary operators
        if let Some(op) = self.current_as_unary_op() {
            let text = unary_op_text(op);
            let bp = if is_sign(op) {
                if min_bp > SHIFT_RBP {
                    self.error(&format!(
                        "a sign `{text}` may only begin a simple expression and cannot \
                         directly follow an adding, multiplying, `**` or prefix operator; \
                         parenthesize the operand, e.g. `a * ({text}b)`"
                    ));
                }
                // Recovery: never let the operand swallow operators that the
                // enclosing context would have stopped at.
                prefix_binding_power(op).max(min_bp)
            } else {
                if min_bp > MULTIPLYING_RBP {
                    self.error(&format!(
                        "`{text}` cannot be the operand of `**` or of another prefix \
                         operator, which require a primary; parenthesize it, \
                         e.g. `a ** ({text} b)`"
                    ));
                }
                prefix_binding_power(op)
            };
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
    pub(crate) fn parse_paren_or_aggregate(&mut self) -> Expr {
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

        // Parse the first element: either a lone expression (a parenthesized
        // expression or the first positional element) or `choices => value`.
        let first = self.parse_aggregate_element();

        if first.choices.is_empty() && !self.at(VhdlToken::Comma) {
            // A parenthesized expression
            self.expect(VhdlToken::RightParen);
            let span = start.merge(self.prev_span());
            return Expr::Paren {
                inner: Box::new(first.value),
                span,
            };
        }

        let mut elements = vec![first];
        while self.eat(VhdlToken::Comma) {
            elements.push(self.parse_aggregate_element());
        }

        self.expect(VhdlToken::RightParen);
        self.check_aggregate_element_order(&elements);
        let span = start.merge(self.prev_span());
        Expr::Aggregate { elements, span }
    }

    /// Parses a single aggregate element: `choices => value` or a positional
    /// `value`.
    ///
    /// ```text
    /// element_association ::= [ choices => ] expression
    /// ```
    ///
    /// The element starts with an expression; if it is followed by `|`,
    /// `to`/`downto` or `=>`, that expression was the first choice and the
    /// rest of the choice list is parsed by [`Self::finish_choices`].
    fn parse_aggregate_element(&mut self) -> AggregateElement {
        let expr = self.parse_expr();

        if self.at(VhdlToken::Arrow)
            || self.at(VhdlToken::Bar)
            || self.at(VhdlToken::To)
            || self.at(VhdlToken::Downto)
        {
            let start = expr.span();
            let first = self.finish_choice(expr);
            let choices = self.finish_choices(first);
            let value = if self.eat(VhdlToken::Arrow) {
                self.parse_expr()
            } else {
                // `(7 downto 0)` with no `=>`: report once and leave the
                // closing `)` for the caller instead of cascading.
                self.expected("`=>` after aggregate choices");
                Expr::Error(self.prev_span())
            };
            let span = start.merge(value.span());
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

    /// Reports aggregate element orderings the LRM forbids (§9.3.3.1): a
    /// positional element after a named one, and an `others` choice in any
    /// element but the last.
    fn check_aggregate_element_order(&self, elements: &[AggregateElement]) {
        let mut seen_named = false;
        for (i, elem) in elements.iter().enumerate() {
            if elem.choices.is_empty() {
                if seen_named {
                    self.error_at(
                        "positional element cannot follow a named element in an aggregate; \
                         put positional elements first",
                        elem.span,
                    );
                }
            } else {
                seen_named = true;
            }
            if i + 1 < elements.len() && elem.choices.iter().any(|c| matches!(c, Choice::Others(_)))
            {
                self.error_at(
                    "the choice `others` is only allowed in the last element of an aggregate",
                    elem.span,
                );
            }
        }
    }

    /// Parses a choice list `choice { | choice }`, as used by aggregates,
    /// case alternatives and selected assignments (LRM §9.3.3.1, §10.9).
    ///
    /// ```text
    /// choices ::= choice { | choice }
    /// choice  ::= simple_expression | discrete_range | others
    /// ```
    pub(crate) fn parse_choices(&mut self) -> Vec<Choice> {
        let first = self.parse_choice();
        self.finish_choices(first)
    }

    /// Parses the `{ | choice }` tail of a choice list whose first choice has
    /// already been parsed, and reports `others` combined with other
    /// choices (it must be the only choice of its element or alternative).
    fn finish_choices(&mut self, first: Choice) -> Vec<Choice> {
        let mut choices = vec![first];
        // Progress: each iteration consumes a `|`.
        while self.eat(VhdlToken::Bar) {
            choices.push(self.parse_choice());
        }
        if choices.len() > 1 {
            for choice in &choices {
                if let Choice::Others(span) = choice {
                    self.error_at(
                        "the choice `others` cannot be combined with other choices",
                        *span,
                    );
                }
            }
        }
        choices
    }

    /// Parses a single choice: `others`, a range `L to R` / `L downto R`, or
    /// an expression.
    pub(crate) fn parse_choice(&mut self) -> Choice {
        if self.at(VhdlToken::Others) {
            let span = self.current_span();
            self.advance();
            Choice::Others(span)
        } else if self.at(VhdlToken::Arrow)
            || self.at(VhdlToken::Bar)
            || self.at(VhdlToken::Comma)
            || self.at(VhdlToken::RightParen)
        {
            // A missing choice (`1 | => x`): report it without consuming the
            // delimiter, so the rest of the list still parses.
            let span = self.current_span();
            self.expected("choice");
            Choice::Expr(Expr::Error(span))
        } else {
            let expr = self.parse_expr();
            self.finish_choice(expr)
        }
    }

    /// Turns an already parsed expression into a choice, consuming a
    /// following `to`/`downto` bound when the choice is a range.
    fn finish_choice(&mut self, expr: Expr) -> Choice {
        if self.at(VhdlToken::To) || self.at(VhdlToken::Downto) {
            let direction = if self.eat(VhdlToken::To) {
                RangeDirection::To
            } else {
                self.advance();
                RangeDirection::Downto
            };
            let right = self.parse_expr();
            let span = expr.span().merge(right.span());
            Choice::Range(RangeConstraint {
                left: Box::new(expr),
                direction,
                right: Box::new(right),
                span,
            })
        } else {
            self.expr_to_choice(expr)
        }
    }

    /// Converts an expression to a choice (for aggregates/case statements).
    pub(crate) fn expr_to_choice(&self, expr: Expr) -> Choice {
        match &expr {
            Expr::Others { span } => Choice::Others(*span),
            _ => Choice::Expr(expr),
        }
    }

    /// Completes an association element whose first expression has already
    /// been parsed: `formal => actual` if `=>` follows, otherwise a
    /// positional element whose actual is `expr`. The actual may be `open`.
    pub(crate) fn finish_association_element(&mut self, expr: Expr) -> AssociationElement {
        if self.eat(VhdlToken::Arrow) {
            let actual = self.parse_expr();
            let span = expr.span().merge(actual.span());
            AssociationElement {
                formal: Some(expr),
                actual,
                span,
            }
        } else {
            let span = expr.span();
            AssociationElement {
                formal: None,
                actual: expr,
                span,
            }
        }
    }

    /// Reports positional association elements that follow a named one,
    /// which the LRM forbids (§6.5.7.1): positional associations must come
    /// first.
    pub(crate) fn check_association_order(&self, elements: &[AssociationElement]) {
        let mut seen_named = false;
        for elem in elements {
            if elem.formal.is_some() {
                seen_named = true;
            } else if seen_named {
                self.error_at(
                    "positional association cannot follow a named association; \
                     put positional associations first",
                    elem.span,
                );
            }
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
                    let paren_start = self.current_span();
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
                    } else {
                        // Association list: indices, or call / conversion
                        // arguments, each `actual` or `formal => actual`.
                        let mut elements = vec![self.finish_association_element(first)];
                        // Progress: each iteration consumes a `,`.
                        while self.eat(VhdlToken::Comma) {
                            let expr = self.parse_expr();
                            elements.push(self.finish_association_element(expr));
                        }
                        self.expect(VhdlToken::RightParen);
                        let span = start.merge(self.prev_span());
                        if elements.iter().all(|e| e.formal.is_none()) {
                            let args = elements.into_iter().map(|e| e.actual).collect();
                            parts.push(NameSuffix::Index(args, span));
                        } else {
                            self.check_association_order(&elements);
                            let list = AssociationList {
                                elements,
                                span: paren_start.merge(self.prev_span()),
                            };
                            parts.push(NameSuffix::Call(list, span));
                        }
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
            // VHDL-2008 unary reductions: `factor ::= logical_operator primary`
            VhdlToken::And => Some(UnaryOp::RedAnd),
            VhdlToken::Or => Some(UnaryOp::RedOr),
            VhdlToken::Nand => Some(UnaryOp::RedNand),
            VhdlToken::Nor => Some(UnaryOp::RedNor),
            VhdlToken::Xor => Some(UnaryOp::RedXor),
            VhdlToken::Xnor => Some(UnaryOp::RedXnor),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::FileId;

    /// Renders an expression as an S-expression so tests can assert tree shape.
    fn render(e: &Expr, src: &str, interner: &Interner) -> String {
        let text = |s: Span| src[s.start as usize..s.end as usize].to_string();
        match e {
            Expr::Name(n) if n.parts.is_empty() => interner.resolve(n.primary).to_string(),
            Expr::Name(n) => text(n.span),
            Expr::IntLiteral { span }
            | Expr::RealLiteral { span }
            | Expr::CharLiteral { span }
            | Expr::StringLiteral { span }
            | Expr::BitStringLiteral { span } => text(*span),
            Expr::Binary {
                left, op, right, ..
            } => format!(
                "({} {} {})",
                binary_op_text(*op),
                render(left, src, interner),
                render(right, src, interner)
            ),
            Expr::Unary { op, operand, .. } => {
                let tag = match op {
                    UnaryOp::Pos => "pos".to_string(),
                    UnaryOp::Neg => "neg".to_string(),
                    UnaryOp::RedAnd
                    | UnaryOp::RedOr
                    | UnaryOp::RedNand
                    | UnaryOp::RedNor
                    | UnaryOp::RedXor
                    | UnaryOp::RedXnor => format!("red-{}", unary_op_text(*op)),
                    _ => unary_op_text(*op).to_string(),
                };
                format!("({tag} {})", render(operand, src, interner))
            }
            Expr::Paren { inner, .. } => format!("[{}]", render(inner, src, interner)),
            other => format!("{other:?}"),
        }
    }

    /// Parses `src` as one expression; returns its shape and diagnostic messages.
    fn parse(src: &str) -> (String, Vec<String>) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(src, file, &sink);
        let mut parser = VhdlParser::new(tokens, src, file, &interner, &sink);
        let expr = parser.parse_expr();
        assert!(parser.at_eof(), "trailing tokens after `{src}`");
        let shape = render(&expr, src, &interner);
        let msgs = sink.take_all().into_iter().map(|d| d.message).collect();
        (shape, msgs)
    }

    /// Parses a legal expression, asserting no diagnostics, and returns its shape.
    fn shape(src: &str) -> String {
        let (shape, msgs) = parse(src);
        assert!(msgs.is_empty(), "unexpected errors for `{src}`: {msgs:?}");
        shape
    }

    /// Parses an illegal expression, asserting exactly one diagnostic that
    /// contains `needle`, and returns the recovered tree's shape.
    fn rejected(src: &str, needle: &str) -> String {
        let (shape, msgs) = parse(src);
        assert_eq!(
            msgs.len(),
            1,
            "expected one error for `{src}`, got {msgs:?}"
        );
        assert!(
            msgs[0].contains(needle),
            "error for `{src}` should mention `{needle}`: {:?}",
            msgs[0]
        );
        shape
    }

    // ========================================================================
    // Precedence table: each level boundary, both orders
    // ========================================================================

    #[test]
    fn precedence_table_level_boundaries() {
        let cases = [
            // logical < relational
            ("a and b = c", "(and a (= b c))"),
            ("a = b or c", "(or (= a b) c)"),
            ("a xor b ?= c", "(xor a (?= b c))"),
            // relational < shift
            ("a = b sll 1", "(= a (sll b 1))"),
            ("a srl 1 < b", "(< (srl a 1) b)"),
            // shift < adding
            ("a sll b + c", "(sll a (+ b c))"),
            ("a & b ror c", "(ror (& a b) c)"),
            // adding < sign: a sign covers a single term only
            ("-a + b", "(+ (neg a) b)"),
            ("+a - b", "(- (pos a) b)"),
            ("a = -b + c", "(= a (+ (neg b) c))"),
            // sign < multiplying: the sign applies to the whole term (VHDL-3)
            ("-7 mod 3", "(neg (mod 7 3))"),
            ("-a * b", "(neg (* a b))"),
            ("-a * b + c", "(+ (neg (* a b)) c)"),
            // multiplying < exponent
            ("a * b ** c", "(* a (** b c))"),
            ("a ** b rem c", "(rem (** a b) c)"),
            ("-a ** 2", "(neg (** a 2))"),
            // multiplying / adding / logical < primary prefix
            ("not a * b", "(* (not a) b)"),
            ("a * not b", "(* a (not b))"),
            ("abs a + b", "(+ (abs a) b)"),
            ("not a and b", "(and (not a) b)"),
            ("-abs a", "(neg (abs a))"),
            ("and v = '1'", "(= (red-and v) '1')"),
            ("?? a", "(?? a)"),
        ];
        for (src, expected) in cases {
            assert_eq!(shape(src), expected, "shape of `{src}`");
        }
    }

    #[test]
    fn left_associative_levels() {
        assert_eq!(shape("a - b - c"), "(- (- a b) c)");
        assert_eq!(shape("a & b & c"), "(& (& a b) c)");
        assert_eq!(shape("a + b - c"), "(- (+ a b) c)");
        assert_eq!(shape("a / b / c"), "(/ (/ a b) c)");
        assert_eq!(shape("a * b mod c"), "(mod (* a b) c)");
    }

    #[test]
    fn same_associative_logical_operator_chains() {
        assert_eq!(shape("a and b and c"), "(and (and a b) c)");
        assert_eq!(shape("a or b or c or d"), "(or (or (or a b) c) d)");
        assert_eq!(shape("a xor b xor c"), "(xor (xor a b) c)");
        assert_eq!(shape("a xnor b xnor c"), "(xnor (xnor a b) c)");
    }

    #[test]
    fn parenthesized_forms_are_legal() {
        assert_eq!(shape("(a and b) or c"), "(or [(and a b)] c)");
        assert_eq!(shape("a nand (b nand c)"), "(nand a [(nand b c)])");
        assert_eq!(shape("(a = b) = c"), "(= [(= a b)] c)");
        assert_eq!(shape("(a sll 1) sll 2"), "(sll [(sll a 1)] 2)");
        assert_eq!(shape("(2 ** 3) ** 2"), "(** [(** 2 3)] 2)");
        assert_eq!(shape("2 ** (3 ** 2)"), "(** 2 [(** 3 2)])");
        assert_eq!(shape("a + (-b)"), "(+ a [(neg b)])");
        assert_eq!(shape("(abs a) ** 2"), "(** [(abs a)] 2)");
        assert_eq!(shape("a ** (not b)"), "(** a [(not b)])");
    }

    #[test]
    fn sign_allowed_at_simple_expression_start() {
        assert_eq!(shape("a = -b"), "(= a (neg b))");
        assert_eq!(shape("a and -b"), "(and a (neg b))");
        assert_eq!(shape("a sll -1"), "(sll a (neg 1))");
        assert_eq!(shape("-10 ns"), "(neg (* 10 ns))");
        assert_eq!(shape("f(-a)"), "f(-a)");
    }

    #[test]
    fn different_levels_mix_freely() {
        assert_eq!(shape("a = b and c /= d"), "(and (= a b) (/= c d))");
        assert_eq!(shape("a sll 1 = b srl 2"), "(= (sll a 1) (srl b 2))");
        assert_eq!(shape("a ** 2 * b ** 3"), "(* (** a 2) (** b 3))");
    }

    // ========================================================================
    // Rejections (VHDL-14): mixed logical, nand/nor chains, non-associative
    // ========================================================================

    #[test]
    fn rejects_mixed_logical_operators() {
        assert_eq!(
            rejected(
                "a and b or c",
                "cannot mix logical operators `and` and `or`"
            ),
            "(or (and a b) c)"
        );
        rejected(
            "a or b and c",
            "cannot mix logical operators `or` and `and`",
        );
        rejected("a xor b xnor c", "cannot mix logical operators");
        rejected("a and b nand c", "cannot mix logical operators");
    }

    #[test]
    fn rejects_nand_nor_chains() {
        assert_eq!(
            rejected("a nand b nand c", "`nand` is not associative"),
            "(nand (nand a b) c)"
        );
        rejected("a nor b nor c", "`nor` is not associative");
    }

    #[test]
    fn rejects_chained_relational_operators() {
        assert_eq!(
            rejected("a = b = c", "relational operators are not associative"),
            "(= (= a b) c)"
        );
        rejected("a < b /= c", "relational operators are not associative");
        rejected("a ?= b ?= c", "relational operators are not associative");
        // Inside the right operand of a logical operator too.
        rejected(
            "x and a = b = c",
            "relational operators are not associative",
        );
    }

    #[test]
    fn rejects_chained_shift_operators() {
        assert_eq!(
            rejected("a sll 1 sll 2", "shift operators are not associative"),
            "(sll (sll a 1) 2)"
        );
        rejected("a sll 1 srl 2", "shift operators are not associative");
    }

    #[test]
    fn rejects_chained_exponent() {
        assert_eq!(
            rejected("2 ** 3 ** 2", "`**` is not associative"),
            "(** (** 2 3) 2)"
        );
        rejected("a * b ** c ** d", "`**` is not associative");
    }

    // ========================================================================
    // Rejections (VHDL-3): misplaced signs and primary-only operands
    // ========================================================================

    #[test]
    fn rejects_sign_after_binary_operator() {
        // Recovery still builds the natural tree.
        assert_eq!(
            rejected("a + -b", "may only begin a simple expression"),
            "(+ a (neg b))"
        );
        assert_eq!(
            rejected("a * -b * c", "may only begin a simple expression"),
            "(* (* a (neg b)) c)"
        );
        assert_eq!(
            rejected("a + -b + c", "may only begin a simple expression"),
            "(+ (+ a (neg b)) c)"
        );
        rejected("a ** -b", "may only begin a simple expression");
        rejected("a & -b", "may only begin a simple expression");
        rejected("a mod +b", "may only begin a simple expression");
        rejected("- -a", "may only begin a simple expression");
        rejected("abs -a", "may only begin a simple expression");
    }

    #[test]
    fn rejects_primary_prefix_combined_with_exponent() {
        assert_eq!(
            rejected("abs a ** 2", "`**` cannot follow a factor built with `abs`"),
            "(** (abs a) 2)"
        );
        rejected("not a ** b", "`**` cannot follow a factor built with `not`");
        assert_eq!(
            rejected("a ** not b", "`not` cannot be the operand of `**`"),
            "(** a (not b))"
        );
        rejected("a ** abs b", "`abs` cannot be the operand of `**`");
        rejected("not not a", "`not` cannot be the operand");
        rejected(
            "-abs a ** 2",
            "`**` cannot follow a factor built with `abs`",
        );
    }

    #[test]
    fn multiple_violations_each_reported() {
        let (_, msgs) = parse("a and b or c = d = e");
        assert_eq!(msgs.len(), 2, "{msgs:?}");
    }

    // ========================================================================
    // VHDL-2008 unary reduction operators
    // ========================================================================

    #[test]
    fn reduction_operators_parse_as_factors() {
        assert_eq!(shape("and v"), "(red-and v)");
        assert_eq!(shape("or v"), "(red-or v)");
        assert_eq!(shape("nand v"), "(red-nand v)");
        assert_eq!(shape("nor v"), "(red-nor v)");
        assert_eq!(shape("xor v"), "(red-xor v)");
        assert_eq!(shape("xnor v"), "(red-xnor v)");
    }

    #[test]
    fn reduction_operand_is_a_primary() {
        assert_eq!(shape("xor v(3 downto 0)"), "(red-xor v(3 downto 0))");
        assert_eq!(shape("or v and x"), "(and (red-or v) x)");
        assert_eq!(shape("a and and b"), "(and a (red-and b))");
        assert_eq!(shape("nand (a & b)"), "(red-nand [(& a b)])");
        assert_eq!(shape("a & xor v"), "(& a (red-xor v))");
    }

    #[test]
    fn reduction_operator_misuse_rejected() {
        rejected("a ** and v", "`and` cannot be the operand of `**`");
        rejected("not or v", "`or` cannot be the operand");
        rejected("xor v ** 2", "`**` cannot follow a factor built with `xor`");
    }

    #[test]
    fn reduction_in_full_design_parses() {
        let src = "entity e is end entity;
            architecture rtl of e is
                signal v : std_logic_vector(7 downto 0);
                signal p, z : std_logic;
            begin
                p <= xor v;
                z <= nor v;
            end architecture;";
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(src, file, &sink);
        let mut parser = VhdlParser::new(tokens, src, file, &interner, &sink);
        let ast = parser.parse_design_file();
        assert_eq!(ast.units.len(), 2);
        let msgs: Vec<_> = sink.take_all().into_iter().map(|d| d.message).collect();
        assert!(msgs.is_empty(), "{msgs:?}");
    }

    #[test]
    fn illegal_forms_in_full_design_report_and_recover() {
        // Each illegal statement is reported once and parsing continues to
        // the end of the architecture.
        let src = "entity e is end entity;
            architecture rtl of e is
            begin
                process
                    variable x : integer;
                begin
                    x := 2 ** 3 ** 2;
                    x := x + -1;
                    x := -7 mod 3;
                    wait;
                end process;
            end architecture;";
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(src, file, &sink);
        let mut parser = VhdlParser::new(tokens, src, file, &interner, &sink);
        let ast = parser.parse_design_file();
        assert_eq!(ast.units.len(), 2);
        let msgs: Vec<_> = sink.take_all().into_iter().map(|d| d.message).collect();
        assert_eq!(msgs.len(), 2, "{msgs:?}");
    }

    // ---- aggregates and association lists ----

    /// Parses `src` as one expression; returns the tree, the source and the
    /// diagnostic messages.
    fn parse_tree(src: &str) -> (Expr, Vec<String>) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(src, file, &sink);
        let mut parser = VhdlParser::new(tokens, src, file, &interner, &sink);
        let expr = parser.parse_expr();
        assert!(parser.at_eof(), "trailing tokens after `{src}`");
        let msgs = sink.take_all().into_iter().map(|d| d.message).collect();
        (expr, msgs)
    }

    /// Parses a legal aggregate and returns its elements.
    fn aggregate(src: &str) -> Vec<AggregateElement> {
        let (expr, msgs) = parse_tree(src);
        assert!(msgs.is_empty(), "unexpected errors for `{src}`: {msgs:?}");
        match expr {
            Expr::Aggregate { elements, .. } => elements,
            other => panic!("expected aggregate for `{src}`, got {other:?}"),
        }
    }

    /// Parses a legal name expression and returns its suffixes.
    fn name_parts(src: &str) -> Vec<NameSuffix> {
        let (expr, msgs) = parse_tree(src);
        assert!(msgs.is_empty(), "unexpected errors for `{src}`: {msgs:?}");
        match expr {
            Expr::Name(name) => name.parts,
            other => panic!("expected name for `{src}`, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_choice_lists() {
        let elems = aggregate("(1 | 3 => '1', others => '0')");
        assert_eq!(elems.len(), 2);
        assert!(matches!(
            elems[0].choices[..],
            [Choice::Expr(_), Choice::Expr(_)]
        ));
        assert!(matches!(elems[1].choices[..], [Choice::Others(_)]));

        let elems = aggregate("(7 downto 4 => '1', 3 | 1 to 2 => x, others => '0')");
        assert!(
            matches!(&elems[0].choices[..], [Choice::Range(r)] if r.direction == RangeDirection::Downto)
        );
        assert!(matches!(
            &elems[1].choices[..],
            [Choice::Expr(_), Choice::Range(r)] if r.direction == RangeDirection::To
        ));
        // The element span covers the choices and the value.
        assert_eq!(elems[0].span.start, 1);
    }

    #[test]
    fn aggregate_positional_then_named_and_parens() {
        let elems = aggregate("('1', '0', others => '1')");
        assert_eq!(elems.len(), 3);
        assert!(elems[0].choices.is_empty() && elems[1].choices.is_empty());
        assert!(matches!(elems[2].choices[..], [Choice::Others(_)]));

        // A single named element is still an aggregate; a lone expression is
        // a parenthesized expression.
        assert_eq!(aggregate("(0 => x)").len(), 1);
        let (expr, msgs) = parse_tree("(a + b)");
        assert!(msgs.is_empty());
        assert!(matches!(expr, Expr::Paren { .. }));
    }

    #[test]
    fn aggregate_choice_errors() {
        let (_, msgs) = parse_tree("(others => '0', 1 => '1')");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("last element"));

        let (_, msgs) = parse_tree("(1 | others => '0')");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("cannot be combined"));

        let (_, msgs) = parse_tree("(0 => '1', '0')");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("positional element"));

        // A choice list with no `=>` is one error and still yields a tree.
        let (expr, msgs) = parse_tree("(3 | 4)");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(matches!(expr, Expr::Aggregate { .. }));

        // A dangling `|` reports the missing choice without panicking.
        let (expr, msgs) = parse_tree("(1 | => '0')");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("expected choice"), "{msgs:?}");
        let Expr::Aggregate { elements, .. } = expr else {
            panic!("expected aggregate");
        };
        assert_eq!(elements[0].choices.len(), 2);
    }

    #[test]
    fn named_association_in_name_suffix() {
        let parts = name_parts("to_unsigned(arg => 3, size => 8)");
        let [NameSuffix::Call(list, span)] = &parts[..] else {
            panic!("expected call suffix, got {parts:?}");
        };
        assert_eq!(list.elements.len(), 2);
        assert!(list.elements.iter().all(|e| e.formal.is_some()));
        // The suffix span starts at the name; the list span at `(`.
        assert_eq!(span.start, 0);
        assert_eq!(list.span.start, 11);
        assert_eq!(list.span.end, span.end);

        let parts = name_parts("resize(v, new_size => 16)");
        let [NameSuffix::Call(list, _)] = &parts[..] else {
            panic!("expected call suffix, got {parts:?}");
        };
        assert!(list.elements[0].formal.is_none());
        assert!(list.elements[1].formal.is_some());

        let parts = name_parts("f(a => open)");
        let [NameSuffix::Call(list, _)] = &parts[..] else {
            panic!("expected call suffix");
        };
        assert!(matches!(list.elements[0].actual, Expr::Open { .. }));
    }

    #[test]
    fn positional_suffixes_are_unchanged() {
        assert!(matches!(
            &name_parts("f(a, b)")[..],
            [NameSuffix::Index(args, _)] if args.len() == 2
        ));
        assert!(matches!(
            &name_parts("v(7 downto 0)")[..],
            [NameSuffix::Slice(..)]
        ));
        // Further suffixes after a call still apply.
        assert!(matches!(
            &name_parts("f(x => 1)(3)")[..],
            [NameSuffix::Call(..), NameSuffix::Index(..)]
        ));
    }

    #[test]
    fn positional_after_named_in_call_is_rejected() {
        let (expr, msgs) = parse_tree("f(a => 1, 2)");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("positional association"));
        // The call is still built.
        let Expr::Name(name) = expr else {
            panic!("expected name");
        };
        assert!(matches!(name.parts[..], [NameSuffix::Call(..)]));
    }

    #[test]
    fn call_suffix_serde_roundtrip() {
        let (expr, _) = parse_tree("resize(arg => v, new_size => 16)");
        let json = serde_json::to_string(&expr).unwrap();
        let back: Expr = serde_json::from_str(&json).unwrap();
        let Expr::Name(name) = back else {
            panic!("expected name");
        };
        assert!(matches!(&name.parts[..], [NameSuffix::Call(l, _)] if l.elements.len() == 2));
    }

    #[test]
    fn binding_power_table_is_ordered() {
        let levels = [
            BinaryOp::And,
            BinaryOp::Eq,
            BinaryOp::Sll,
            BinaryOp::Add,
            BinaryOp::Mul,
            BinaryOp::Pow,
        ];
        let bps: Vec<_> = levels.iter().map(|op| infix_binding_power(*op)).collect();
        for w in bps.windows(2) {
            assert!(w[0].1 < w[1].0, "levels must be strictly ordered: {bps:?}");
        }
        // Sign sits strictly between adding and multiplying.
        assert!(infix_binding_power(BinaryOp::Add).1 < SIGN_BP);
        assert!(SIGN_BP < infix_binding_power(BinaryOp::Mul).0);
        // Primary prefix operators bind tighter than every infix operator.
        assert!(PRIMARY_PREFIX_BP > infix_binding_power(BinaryOp::Pow).1);
        assert_eq!(prefix_binding_power(UnaryOp::Neg), SIGN_BP);
        assert_eq!(prefix_binding_power(UnaryOp::RedXor), PRIMARY_PREFIX_BP);
    }
}
