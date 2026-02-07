//! Type indication, constraint, and range parsing for VHDL-2008.
//!
//! Handles type marks, range constraints (`7 downto 0`), index constraints,
//! and discrete ranges used in type declarations and subtype indications.

use crate::ast::*;
use crate::parser::VhdlParser;
use crate::token::VhdlToken;

impl<'src> VhdlParser<'src> {
    /// Parses a type indication (type mark with optional constraint).
    ///
    /// ```text
    /// type_indication ::= type_mark [constraint]
    /// ```
    pub fn parse_type_indication(&mut self) -> TypeIndication {
        let start = self.current_span();
        let type_mark = self.parse_selected_name();

        // Check for range constraint: `range expr direction expr`
        let constraint = if self.at(VhdlToken::Range) {
            self.advance();
            let range = self.parse_range_constraint();
            Some(Constraint::Range(range))
        } else if self.at(VhdlToken::LeftParen) {
            // Index constraint: (discrete_range {, discrete_range})
            Some(self.parse_index_constraint())
        } else {
            None
        };

        let span = if let Some(ref c) = constraint {
            let end = match c {
                Constraint::Range(r) => r.span,
                Constraint::Index(_, s) => *s,
            };
            start.merge(end)
        } else {
            type_mark.span
        };

        TypeIndication {
            type_mark,
            constraint,
            span,
        }
    }

    /// Parses a range constraint: `expr (to|downto) expr`.
    pub fn parse_range_constraint(&mut self) -> RangeConstraint {
        let left = self.parse_expr();
        let direction = if self.eat(VhdlToken::To) {
            RangeDirection::To
        } else if self.eat(VhdlToken::Downto) {
            RangeDirection::Downto
        } else {
            self.expected("'to' or 'downto'");
            RangeDirection::To
        };
        let right = self.parse_expr();
        let span = left.span().merge(right.span());
        RangeConstraint {
            left: Box::new(left),
            direction,
            right: Box::new(right),
            span,
        }
    }

    /// Parses an index constraint: `( discrete_range {, discrete_range} )`.
    fn parse_index_constraint(&mut self) -> Constraint {
        let start = self.current_span();
        self.expect(VhdlToken::LeftParen);
        let mut ranges = Vec::new();
        ranges.push(self.parse_discrete_range());
        while self.eat(VhdlToken::Comma) {
            ranges.push(self.parse_discrete_range());
        }
        self.expect(VhdlToken::RightParen);
        let span = start.merge(self.prev_span());
        Constraint::Index(ranges, span)
    }

    /// Parses a discrete range — either a range or a type indication.
    ///
    /// We try parsing as `expr (to|downto) expr` first. If that fails,
    /// we treat it as a type indication.
    pub fn parse_discrete_range(&mut self) -> DiscreteRange {
        // Parse the first expression / name
        let start = self.current_span();
        let first_expr = self.parse_expr();

        // If we see to/downto, it's an explicit range
        if self.at(VhdlToken::To) || self.at(VhdlToken::Downto) {
            let direction = if self.eat(VhdlToken::To) {
                RangeDirection::To
            } else {
                self.advance();
                RangeDirection::Downto
            };
            let right = self.parse_expr();
            let span = first_expr.span().merge(right.span());
            return DiscreteRange::Range(RangeConstraint {
                left: Box::new(first_expr),
                direction,
                right: Box::new(right),
                span,
            });
        }

        // If first_expr is a name and we see `range`, it's a type indication with range
        if self.at(VhdlToken::Range) {
            let type_mark = self.expr_to_selected_name(&first_expr);
            self.advance(); // eat 'range'
            let range = self.parse_range_constraint();
            let span = start.merge(range.span);
            return DiscreteRange::TypeIndication(TypeIndication {
                type_mark,
                constraint: Some(Constraint::Range(range)),
                span,
            });
        }

        // Otherwise, interpret the expression as a simple type indication
        let type_mark = self.expr_to_selected_name(&first_expr);
        let span = type_mark.span;
        DiscreteRange::TypeIndication(TypeIndication {
            type_mark,
            constraint: None,
            span,
        })
    }

    /// Converts an expression to a selected name (best-effort).
    pub(crate) fn expr_to_selected_name(&self, expr: &Expr) -> SelectedName {
        match expr {
            Expr::Name(name) => {
                let mut parts = vec![name.primary];
                for suffix in &name.parts {
                    if let NameSuffix::Selected(id, _) = suffix {
                        parts.push(*id);
                    }
                }
                SelectedName {
                    parts,
                    span: name.span,
                }
            }
            _ => SelectedName {
                parts: vec![self.interner.get_or_intern("<error>")],
                span: expr.span(),
            },
        }
    }
}
