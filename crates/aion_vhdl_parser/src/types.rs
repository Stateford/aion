//! Type indication, constraint, and range parsing for VHDL-2008.
//!
//! Handles subtype indications (with optional resolution indications), range
//! constraints (`7 downto 0`), index and VHDL-2008 array constraints
//! (`(0 to 3)(7 downto 0)`, `(open)`), and the discrete ranges used in type
//! declarations and subtype indications, including the unconstrained
//! `type_mark range <>` index of an array type definition.

use crate::ast::*;
use crate::parser::VhdlParser;
use crate::token::VhdlToken;
use aion_source::Span;

impl VhdlParser<'_> {
    /// Parses a subtype indication: an optional resolution indication, a
    /// type mark and an optional constraint.
    ///
    /// ```text
    /// subtype_indication    ::= [resolution_indication] type_mark [constraint]
    /// resolution_indication ::= resolution_function_name | ( element_resolution )
    /// constraint            ::= range_constraint | array_constraint
    /// ```
    ///
    /// A leading `(` starts an element resolution. A name directly followed
    /// by another identifier is a resolution function name: no other
    /// construct places an identifier right after a subtype indication's
    /// type mark.
    pub fn parse_type_indication(&mut self) -> TypeIndication {
        let start = self.current_span();

        let resolution = if self.at(VhdlToken::LeftParen) {
            Some(self.parse_element_resolution())
        } else {
            None
        };

        let mut type_mark = self.parse_selected_name();
        let resolution = match resolution {
            Some(r) => Some(r),
            None if self.at_identifier() => {
                // `resolved std_ulogic`: the first name was the resolution
                // function and the type mark follows.
                let function = std::mem::replace(&mut type_mark, self.parse_selected_name());
                Some(ResolutionIndication::Function(function))
            }
            None => None,
        };

        // Check for range constraint: `range expr direction expr`
        let constraint = if self.at(VhdlToken::Range) {
            self.advance();
            let range = self.parse_range_constraint();
            Some(Constraint::Range(range))
        } else if self.at(VhdlToken::LeftParen) {
            Some(self.parse_array_constraint())
        } else {
            None
        };

        let end = match &constraint {
            Some(c) => c.span(),
            None => type_mark.span,
        };

        TypeIndication {
            resolution,
            type_mark,
            constraint,
            span: start.merge(end),
        }
    }

    /// Parses an element resolution in parentheses, the current token being
    /// `(`.
    ///
    /// ```text
    /// array_element_resolution   ::= resolution_indication
    /// record_resolution          ::= record_element_resolution { , record_element_resolution }
    /// record_element_resolution  ::= record_element_simple_name resolution_indication
    /// ```
    ///
    /// `(f)` and `((f))` are array element resolutions; `(a f, b g)` is a
    /// record resolution.
    fn parse_element_resolution(&mut self) -> ResolutionIndication {
        let start = self.current_span();
        self.expect(VhdlToken::LeftParen);

        if self.at(VhdlToken::LeftParen) {
            // Nested array element resolution: `((resolved))`
            let inner = self.parse_element_resolution();
            self.expect(VhdlToken::RightParen);
            return ResolutionIndication::Element(Box::new(inner), start.merge(self.prev_span()));
        }

        let first = self.parse_selected_name();
        if self.at(VhdlToken::RightParen) || !self.at_record_resolution_continuation() {
            // Array element resolution: `(resolved)`
            self.expect(VhdlToken::RightParen);
            return ResolutionIndication::Element(
                Box::new(ResolutionIndication::Function(first)),
                start.merge(self.prev_span()),
            );
        }

        // Record resolution: `(a resolved, b (wired_or))`
        let mut elements = Vec::new();
        let mut element_name = first;
        loop {
            if element_name.parts.len() != 1 {
                self.error_at(
                    "a record element resolution must start with a simple element name",
                    element_name.span,
                );
            }
            let resolution = if self.at(VhdlToken::LeftParen) {
                self.parse_element_resolution()
            } else {
                ResolutionIndication::Function(self.parse_selected_name())
            };
            let span = element_name.span.merge(resolution.span());
            elements.push(RecordElementResolution {
                element: element_name.parts[0],
                resolution,
                span,
            });
            // Progress: each further iteration consumes a `,`.
            if !self.eat(VhdlToken::Comma) {
                break;
            }
            element_name = self.parse_selected_name();
        }
        self.expect(VhdlToken::RightParen);
        ResolutionIndication::Record(elements, start.merge(self.prev_span()))
    }

    /// Returns `true` if the current token can follow a record element name
    /// in a record resolution (`a resolved` or `a (resolved)`).
    fn at_record_resolution_continuation(&self) -> bool {
        self.at_identifier() || self.at(VhdlToken::LeftParen)
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

    /// Parses an array constraint, the current token being `(`.
    ///
    /// ```text
    /// array_constraint   ::= index_constraint [ element_constraint ]
    ///                      | ( open ) [ element_constraint ]
    /// element_constraint ::= array_constraint | record_constraint
    /// ```
    ///
    /// A single `( ... )` yields [`Constraint::Index`] or
    /// [`Constraint::Open`]; each further `( ... )` constrains the element
    /// subtype and wraps the result in [`Constraint::Array`].
    fn parse_array_constraint(&mut self) -> Constraint {
        let start = self.current_span();
        let index = self.parse_index_constraint();
        if !self.at(VhdlToken::LeftParen) {
            return index;
        }
        let element = self.parse_array_constraint();
        let span = start.merge(element.span());
        Constraint::Array {
            index: Box::new(index),
            element: Box::new(element),
            span,
        }
    }

    /// Parses one parenthesized index constraint,
    /// `( discrete_range {, discrete_range} )`, or the VHDL-2008 `( open )`.
    fn parse_index_constraint(&mut self) -> Constraint {
        let start = self.current_span();
        self.expect(VhdlToken::LeftParen);
        if self.at(VhdlToken::Open) {
            self.advance();
            if !self.at(VhdlToken::RightParen) {
                self.error("`open` must be the only element of an index constraint: `(open)`");
                // Recover: skip the rest of the constraint.
                while !self.at(VhdlToken::RightParen)
                    && !self.at(VhdlToken::Semicolon)
                    && !self.at_eof()
                {
                    self.advance();
                }
            }
            self.expect(VhdlToken::RightParen);
            return Constraint::Open(start.merge(self.prev_span()));
        }
        let mut ranges = Vec::new();
        ranges.push(self.parse_discrete_range());
        while self.eat(VhdlToken::Comma) {
            ranges.push(self.parse_discrete_range());
        }
        self.expect(VhdlToken::RightParen);
        let span = start.merge(self.prev_span());
        Constraint::Index(ranges, span)
    }

    /// Parses a discrete range: an explicit range, a range attribute name,
    /// or a type indication.
    ///
    /// ```text
    /// discrete_range ::= discrete_subtype_indication | range
    /// range          ::= range_attribute_name | simple_expression direction simple_expression
    /// ```
    ///
    /// The first operand is parsed as an expression. If `to`/`downto`
    /// follows, it is an explicit range. Otherwise the expression must be a
    /// name: a name ending in `'range` / `'reverse_range` becomes
    /// [`DiscreteRange::Attribute`], and a plain (possibly selected) type
    /// mark becomes [`DiscreteRange::TypeIndication`]. Anything else is
    /// reported and yields [`DiscreteRange::Error`].
    ///
    /// The unconstrained form `type_mark range <>` is only legal in an array
    /// type definition (see [`Self::parse_array_index`]); here it is
    /// reported and yields [`DiscreteRange::Error`].
    pub fn parse_discrete_range(&mut self) -> DiscreteRange {
        self.parse_discrete_range_or_box(false)
    }

    /// Parses one index of an array type definition: either an
    /// unconstrained index subtype definition `type_mark range <>`
    /// ([`DiscreteRange::Unconstrained`]) or a discrete range.
    ///
    /// ```text
    /// index_subtype_definition ::= type_mark range <>
    /// ```
    pub fn parse_array_index(&mut self) -> DiscreteRange {
        self.parse_discrete_range_or_box(true)
    }

    /// Shared implementation of [`Self::parse_discrete_range`] and
    /// [`Self::parse_array_index`]; `allow_box` says whether
    /// `type_mark range <>` is legal at this position.
    fn parse_discrete_range_or_box(&mut self, allow_box: bool) -> DiscreteRange {
        // A bare `<>` with no `type_mark range` before it.
        if self.at(VhdlToken::Box) {
            let span = self.current_span();
            self.advance();
            self.error_at(
                "`<>` must be preceded by an index subtype: write `natural range <>`",
                span,
            );
            return DiscreteRange::Error(span);
        }

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

        // `type_mark range <>`: an unconstrained index subtype definition
        if self.at(VhdlToken::Range) && self.peek_is(VhdlToken::Box) {
            let type_mark = self.expr_as_type_mark(&first_expr);
            self.advance(); // eat 'range'
            self.advance(); // eat '<>'
            let span = start.merge(self.prev_span());
            let Some(type_mark) = type_mark else {
                self.report_invalid_discrete_range(&first_expr, "a type mark before `range <>`");
                return DiscreteRange::Error(span);
            };
            if !allow_box {
                self.error_at(
                    "an unconstrained index range `range <>` is only allowed in an array \
                     type definition",
                    span,
                );
                return DiscreteRange::Error(span);
            }
            return DiscreteRange::Unconstrained { type_mark, span };
        }

        // `type_mark range L to R`: a subtype indication with a range constraint
        if self.at(VhdlToken::Range) {
            let type_mark = self.expr_as_type_mark(&first_expr);
            self.advance(); // eat 'range'
            let range = self.parse_range_constraint();
            let span = start.merge(range.span);
            let Some(type_mark) = type_mark else {
                self.report_invalid_discrete_range(&first_expr, "a type mark before `range`");
                return DiscreteRange::Error(span);
            };
            return DiscreteRange::TypeIndication(TypeIndication {
                resolution: None,
                type_mark,
                constraint: Some(Constraint::Range(range)),
                span,
            });
        }

        // A range attribute name: `prefix'range[(N)]` / `prefix'reverse_range[(N)]`
        if let Some(attr) = self.expr_as_range_attribute(&first_expr) {
            return DiscreteRange::Attribute(attr);
        }

        // Otherwise the expression must be a simple type mark
        match self.expr_as_type_mark(&first_expr) {
            Some(type_mark) => {
                let span = type_mark.span;
                DiscreteRange::TypeIndication(TypeIndication {
                    resolution: None,
                    type_mark,
                    constraint: None,
                    span,
                })
            }
            None => {
                self.report_invalid_discrete_range(
                    &first_expr,
                    "a range (`L to R`, `L downto R`, `x'range`, `x'reverse_range`) or a type mark",
                );
                DiscreteRange::Error(first_expr.span())
            }
        }
    }

    /// Reports that `expr` cannot be used as a discrete range.
    ///
    /// Error nodes are skipped: their diagnostic was already emitted when
    /// the expression failed to parse.
    fn report_invalid_discrete_range(&self, expr: &Expr, expected: &str) {
        if matches!(expr, Expr::Error(_)) {
            return;
        }
        let msg = match expr {
            Expr::Name(name) => match name.parts.last() {
                Some(NameSuffix::Attribute(attr, _, _)) => format!(
                    "attribute `'{}` does not denote a range; expected {expected}",
                    self.interner.resolve(*attr)
                ),
                _ => format!("name is not a valid discrete range; expected {expected}"),
            },
            _ => format!("expression is not a valid discrete range; expected {expected}"),
        };
        self.error_at(&msg, expr.span());
    }

    /// Interprets `expr` as a range attribute name (`x'range`,
    /// `x'reverse_range`, optionally with a dimension argument).
    ///
    /// Returns `None` if `expr` is not a name whose last suffix is one of
    /// those two attributes.
    fn expr_as_range_attribute(&self, expr: &Expr) -> Option<RangeAttributeName> {
        let Expr::Name(name) = expr else {
            return None;
        };
        let Some(NameSuffix::Attribute(attr, dimension, _)) = name.parts.last() else {
            return None;
        };
        let attr_text = self.interner.resolve(*attr);
        let kind = if attr_text.eq_ignore_ascii_case("range") {
            RangeAttributeKind::Range
        } else if attr_text.eq_ignore_ascii_case("reverse_range") {
            RangeAttributeKind::ReverseRange
        } else {
            return None;
        };

        // The prefix keeps every suffix before the attribute. Suffix spans
        // start at the beginning of the name, so the last remaining suffix's
        // span is the prefix's span; with no suffixes it is the primary.
        let prefix_parts = name.parts[..name.parts.len() - 1].to_vec();
        let prefix_span = match prefix_parts.last() {
            Some(suffix) => name_suffix_span(suffix),
            None => {
                let len = self.interner.resolve(name.primary).len() as u32;
                Span::new(
                    name.span.file,
                    name.span.start,
                    (name.span.start + len).min(name.span.end),
                )
            }
        };
        Some(RangeAttributeName {
            prefix: Name {
                primary: name.primary,
                parts: prefix_parts,
                span: prefix_span,
            },
            kind,
            dimension: dimension.clone(),
            span: name.span,
        })
    }

    /// Interprets `expr` as a type mark (a simple or selected name such as
    /// `state_t` or `work.pkg.word_t`).
    ///
    /// Returns `None` if `expr` is not a name, or if the name carries any
    /// suffix other than `.ident` (index, slice, attribute, `.all`), since
    /// dropping such a suffix would silently change the meaning.
    pub(crate) fn expr_as_type_mark(&self, expr: &Expr) -> Option<SelectedName> {
        let Expr::Name(name) = expr else {
            return None;
        };
        let mut parts = vec![name.primary];
        for suffix in &name.parts {
            match suffix {
                NameSuffix::Selected(id, _) => parts.push(*id),
                _ => return None,
            }
        }
        Some(SelectedName {
            parts,
            span: name.span,
        })
    }
}

/// Returns the span stored on a name suffix.
fn name_suffix_span(suffix: &NameSuffix) -> Span {
    match suffix {
        NameSuffix::Selected(_, span)
        | NameSuffix::Index(_, span)
        | NameSuffix::Call(_, span)
        | NameSuffix::Slice(_, span)
        | NameSuffix::Attribute(_, _, span)
        | NameSuffix::All(span) => *span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;
    use aion_common::Interner;
    use aion_diagnostics::{Diagnostic, DiagnosticSink};
    use aion_source::FileId;

    fn parse_vhdl(source: &str) -> (VhdlDesignFile, Vec<Diagnostic>, Interner) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let mut parser = VhdlParser::new(tokens, source, file, &interner, &sink);
        let ast = parser.parse_design_file();
        (ast, sink.take_all(), interner)
    }

    fn architecture(ast: &VhdlDesignFile) -> &ArchitectureDecl {
        match &ast.units[0] {
            DesignUnit::ContextUnit {
                unit: DesignUnitKind::Architecture(a),
                ..
            } => a,
            other => panic!("expected architecture, got {other:?}"),
        }
    }

    /// Parses `for i in <range> loop null; end loop;` inside a process and
    /// returns the loop's discrete range, the diagnostics, the interner and
    /// the full source text.
    fn for_loop_range(range: &str) -> (DiscreteRange, Vec<Diagnostic>, Interner, String) {
        let src = format!(
            "architecture rtl of top is
            begin
                process
                begin
                    for i in {range} loop null; end loop;
                end process;
            end architecture rtl;"
        );
        let (ast, diags, interner) = parse_vhdl(&src);
        let arch = architecture(&ast);
        let ConcurrentStatement::Process(p) = &arch.stmts[0] else {
            panic!("expected process");
        };
        let SequentialStatement::ForLoop(fl) = &p.stmts[0] else {
            panic!("expected for loop, got {:?}", p.stmts[0]);
        };
        (fl.range.clone(), diags, interner, src)
    }

    fn expect_attr(range: &DiscreteRange) -> &RangeAttributeName {
        match range {
            DiscreteRange::Attribute(attr) => attr,
            other => panic!("expected DiscreteRange::Attribute, got {other:?}"),
        }
    }

    fn snippet(src: &str, span: Span) -> &str {
        &src[span.start as usize..span.end as usize]
    }

    #[test]
    fn for_loop_over_object_range_attribute() {
        let (range, diags, interner, src) = for_loop_range("v'range");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        let attr = expect_attr(&range);
        assert_eq!(attr.kind, RangeAttributeKind::Range);
        assert_eq!(interner.resolve(attr.prefix.primary), "v");
        assert!(attr.prefix.parts.is_empty());
        assert!(attr.dimension.is_none());
        assert_eq!(snippet(&src, attr.span), "v'range");
        assert_eq!(snippet(&src, attr.prefix.span), "v");
        assert_eq!(range.span(), attr.span);
    }

    #[test]
    fn for_loop_over_reverse_range_attribute() {
        let (range, diags, interner, _) = for_loop_range("data_bus'REVERSE_RANGE");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        let attr = expect_attr(&range);
        assert_eq!(attr.kind, RangeAttributeKind::ReverseRange);
        assert_eq!(interner.resolve(attr.prefix.primary), "data_bus");
    }

    #[test]
    fn type_mark_range_attribute() {
        // `T'range` is syntactically identical to `v'range`; the prefix is
        // resolved to a type or an object during elaboration.
        let (range, diags, interner, _) = for_loop_range("word_t'range");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        let attr = expect_attr(&range);
        assert_eq!(attr.kind, RangeAttributeKind::Range);
        assert_eq!(interner.resolve(attr.prefix.primary), "word_t");
    }

    #[test]
    fn range_attribute_with_dimension_argument() {
        let (range, diags, _, src) = for_loop_range("mem'range(2)");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        let attr = expect_attr(&range);
        let dim = attr.dimension.as_ref().expect("dimension argument kept");
        assert!(matches!(**dim, Expr::IntLiteral { .. }));
        assert_eq!(snippet(&src, dim.span()), "2");
        assert_eq!(snippet(&src, attr.span), "mem'range(2)");
    }

    #[test]
    fn range_attribute_keeps_selected_and_indexed_prefix() {
        let (range, diags, interner, src) = for_loop_range("regs.bank(1)'reverse_range");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        let attr = expect_attr(&range);
        assert_eq!(attr.kind, RangeAttributeKind::ReverseRange);
        assert_eq!(attr.prefix.parts.len(), 2);
        match &attr.prefix.parts[0] {
            NameSuffix::Selected(id, _) => assert_eq!(interner.resolve(*id), "bank"),
            other => panic!("expected selected suffix, got {other:?}"),
        }
        assert!(matches!(attr.prefix.parts[1], NameSuffix::Index(..)));
        assert_eq!(snippet(&src, attr.prefix.span), "regs.bank(1)");
    }

    #[test]
    fn plain_type_mark_stays_type_indication() {
        let (range, diags, interner, _) = for_loop_range("state_t");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        match range {
            DiscreteRange::TypeIndication(ti) => {
                assert_eq!(ti.type_mark.parts.len(), 1);
                assert_eq!(interner.resolve(ti.type_mark.parts[0]), "state_t");
                assert!(ti.constraint.is_none());
            }
            other => panic!("expected type indication, got {other:?}"),
        }
    }

    #[test]
    fn explicit_range_unchanged() {
        let (range, diags, _, _) = for_loop_range("7 downto 0");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        match range {
            DiscreteRange::Range(rc) => assert_eq!(rc.direction, RangeDirection::Downto),
            other => panic!("expected explicit range, got {other:?}"),
        }
    }

    #[test]
    fn subtype_with_range_constraint_still_parses() {
        let (range, diags, _, _) = for_loop_range("integer range 0 to 3");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        match range {
            DiscreteRange::TypeIndication(ti) => {
                assert!(matches!(ti.constraint, Some(Constraint::Range(_))));
            }
            other => panic!("expected type indication, got {other:?}"),
        }
    }

    #[test]
    fn non_name_expression_is_rejected() {
        let (range, diags, _, src) = for_loop_range("a + 1");
        assert!(matches!(range, DiscreteRange::Error(_)), "got {range:?}");
        assert_eq!(snippet(&src, range.span()), "a + 1");
        assert_eq!(diags.len(), 1, "diagnostics: {diags:?}");
        assert!(
            diags[0].message.contains("not a valid discrete range"),
            "{}",
            diags[0].message
        );
        assert_eq!(snippet(&src, diags[0].primary_span), "a + 1");
    }

    #[test]
    fn literal_is_rejected() {
        let (range, diags, _, _) = for_loop_range("8");
        assert!(matches!(range, DiscreteRange::Error(_)), "got {range:?}");
        assert_eq!(diags.len(), 1, "diagnostics: {diags:?}");
    }

    #[test]
    fn non_range_attribute_is_rejected() {
        let (range, diags, _, _) = for_loop_range("v'length");
        assert!(matches!(range, DiscreteRange::Error(_)), "got {range:?}");
        assert_eq!(diags.len(), 1, "diagnostics: {diags:?}");
        assert!(
            diags[0]
                .message
                .contains("`'length` does not denote a range"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn indexed_name_is_rejected() {
        // Previously `v(3)` silently collapsed to the type mark `v`.
        let (range, diags, _, _) = for_loop_range("v(3)");
        assert!(matches!(range, DiscreteRange::Error(_)), "got {range:?}");
        assert_eq!(diags.len(), 1, "diagnostics: {diags:?}");
    }

    #[test]
    fn non_type_mark_before_range_constraint_is_rejected() {
        let (range, diags, _, _) = for_loop_range("v'length range 0 to 3");
        assert!(matches!(range, DiscreteRange::Error(_)), "got {range:?}");
        assert_eq!(diags.len(), 1, "diagnostics: {diags:?}");
        assert!(diags[0].message.contains("type mark before `range`"));
    }

    #[test]
    fn for_generate_over_range_attribute() {
        let (ast, diags, interner) = parse_vhdl(
            "architecture rtl of top is
            begin
                gen : for i in v'reverse_range generate
                    y(i) <= v(i);
                end generate gen;
            end architecture rtl;",
        );
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        let ConcurrentStatement::ForGenerate(fg) = &architecture(&ast).stmts[0] else {
            panic!("expected for-generate");
        };
        let attr = expect_attr(&fg.range);
        assert_eq!(attr.kind, RangeAttributeKind::ReverseRange);
        assert_eq!(interner.resolve(attr.prefix.primary), "v");
    }

    #[test]
    fn index_constraint_with_range_attribute() {
        let (ast, diags, interner) = parse_vhdl(
            "architecture rtl of top is
                signal a : std_logic_vector(7 downto 0);
                signal b : std_logic_vector(a'range);
            begin
            end architecture rtl;",
        );
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        let Declaration::Signal(sd) = &architecture(&ast).decls[1] else {
            panic!("expected signal declaration");
        };
        let Some(Constraint::Index(ranges, _)) = &sd.ty.constraint else {
            panic!("expected index constraint, got {:?}", sd.ty.constraint);
        };
        assert_eq!(ranges.len(), 1);
        let attr = expect_attr(&ranges[0]);
        assert_eq!(interner.resolve(attr.prefix.primary), "a");
    }

    #[test]
    fn range_attribute_serde_roundtrip() {
        let (range, _, _, _) = for_loop_range("mem'reverse_range(2)");
        let json = serde_json::to_string(&range).unwrap();
        let back: DiscreteRange = serde_json::from_str(&json).unwrap();
        let attr = expect_attr(&back);
        assert_eq!(attr.kind, RangeAttributeKind::ReverseRange);
        assert!(attr.dimension.is_some());
        assert_eq!(back.span(), range.span());

        let err = DiscreteRange::Error(range.span());
        let json = serde_json::to_string(&err).unwrap();
        let back: DiscreteRange = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DiscreteRange::Error(s) if s == range.span()));
    }

    // ---- subtype indications: resolution, array constraints, box ----

    /// Parses `decls` inside a package; returns the package declarations,
    /// the diagnostic messages and the interner.
    fn package(decls: &str) -> (Vec<Declaration>, Vec<String>, Interner) {
        let src = format!("package p is {decls} end package;");
        let (ast, diags, interner) = parse_vhdl(&src);
        let DesignUnit::ContextUnit {
            unit: DesignUnitKind::Package(p),
            ..
        } = &ast.units[0]
        else {
            panic!("expected package, got {:?}", ast.units[0]);
        };
        let msgs = diags.into_iter().map(|d| d.message).collect();
        (p.decls.clone(), msgs, interner)
    }

    /// Returns the subtype indication of a subtype declaration.
    fn subtype_ind(decl: &Declaration) -> &TypeIndication {
        match decl {
            Declaration::Subtype(st) => &st.ty,
            other => panic!("expected subtype declaration, got {other:?}"),
        }
    }

    /// Returns the indices of an array type declaration.
    fn array_indices(decl: &Declaration) -> &[DiscreteRange] {
        match decl {
            Declaration::Type(TypeDecl {
                def: TypeDef::Array { indices, .. },
                ..
            }) => indices,
            other => panic!("expected array type declaration, got {other:?}"),
        }
    }

    #[test]
    fn resolution_function_before_type_mark() {
        let (decls, msgs, interner) = package(
            "subtype rsl is resolved std_ulogic;
             signal s : ieee.std_logic_1164.resolved std_ulogic_vector(3 downto 0);
             subtype plain is std_ulogic;",
        );
        assert!(msgs.is_empty(), "{msgs:?}");
        let ti = subtype_ind(&decls[0]);
        let Some(ResolutionIndication::Function(f)) = &ti.resolution else {
            panic!("expected resolution function, got {:?}", ti.resolution);
        };
        assert_eq!(interner.resolve(f.parts[0]), "resolved");
        assert_eq!(interner.resolve(ti.type_mark.parts[0]), "std_ulogic");

        let Declaration::Signal(sd) = &decls[1] else {
            panic!("expected signal");
        };
        let Some(ResolutionIndication::Function(f)) = &sd.ty.resolution else {
            panic!("expected resolution function");
        };
        assert_eq!(f.parts.len(), 3);
        assert!(matches!(sd.ty.constraint, Some(Constraint::Index(..))));
        assert!(subtype_ind(&decls[2]).resolution.is_none());
    }

    #[test]
    fn element_and_record_resolutions() {
        let (decls, msgs, interner) = package(
            "subtype rslv is (resolved) std_ulogic_vector;
             subtype rmat is ((resolved)) mat_t;
             subtype rrec is (a resolved, b (wired_or)) rec_t;",
        );
        assert!(msgs.is_empty(), "{msgs:?}");
        let ti = subtype_ind(&decls[0]);
        let Some(ResolutionIndication::Element(inner, span)) = &ti.resolution else {
            panic!("expected element resolution, got {:?}", ti.resolution);
        };
        assert!(matches!(**inner, ResolutionIndication::Function(_)));
        // The subtype indication starts at the resolution's `(`.
        assert_eq!(ti.span.start, span.start);

        let Some(ResolutionIndication::Element(inner, _)) = &subtype_ind(&decls[1]).resolution
        else {
            panic!("expected element resolution");
        };
        assert!(matches!(**inner, ResolutionIndication::Element(..)));

        let Some(ResolutionIndication::Record(elems, _)) = &subtype_ind(&decls[2]).resolution
        else {
            panic!("expected record resolution");
        };
        assert_eq!(elems.len(), 2);
        assert_eq!(interner.resolve(elems[0].element), "a");
        assert!(matches!(
            elems[1].resolution,
            ResolutionIndication::Element(..)
        ));
    }

    #[test]
    fn record_resolution_needs_simple_element_names() {
        let (decls, msgs, _) = package("subtype r is (x.y resolved) rec_t;");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("simple element name"), "{msgs:?}");
        assert!(matches!(
            subtype_ind(&decls[0]).resolution,
            Some(ResolutionIndication::Record(..))
        ));
    }

    #[test]
    fn multi_level_and_open_constraints() {
        let (decls, msgs, _) = package(
            "subtype m4 is mem_t(0 to 3)(7 downto 0);
             subtype mx is mem_t(open)(15 downto 0);
             subtype cube is cube_t(0 to 1)(open)(3 downto 0);
             subtype o is std_logic_vector(open);",
        );
        assert!(msgs.is_empty(), "{msgs:?}");
        let ti = subtype_ind(&decls[0]);
        let Some(Constraint::Array {
            index,
            element,
            span,
        }) = &ti.constraint
        else {
            panic!("expected array constraint, got {:?}", ti.constraint);
        };
        assert!(matches!(**index, Constraint::Index(ref r, _) if r.len() == 1));
        assert!(matches!(**element, Constraint::Index(..)));
        assert_eq!(ti.span.end, span.end);
        assert_eq!(ti.constraint.as_ref().unwrap().span(), *span);

        let Some(Constraint::Array { index, .. }) = &subtype_ind(&decls[1]).constraint else {
            panic!("expected array constraint");
        };
        assert!(matches!(**index, Constraint::Open(_)));

        let Some(Constraint::Array { element, .. }) = &subtype_ind(&decls[2]).constraint else {
            panic!("expected array constraint");
        };
        assert!(matches!(
            **element,
            Constraint::Array { ref index, .. } if matches!(**index, Constraint::Open(_))
        ));

        assert!(matches!(
            subtype_ind(&decls[3]).constraint,
            Some(Constraint::Open(_))
        ));
    }

    #[test]
    fn open_must_be_alone_in_index_constraint() {
        let (decls, msgs, _) = package(
            "subtype bad is mem_t(open, 0 to 3);
             subtype good is mem_t(0 to 3);",
        );
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("`open` must be the only"), "{msgs:?}");
        assert_eq!(decls.len(), 2);
        assert!(matches!(
            subtype_ind(&decls[0]).constraint,
            Some(Constraint::Open(_))
        ));
    }

    #[test]
    fn unconstrained_array_indices() {
        let (decls, msgs, interner) = package(
            "type m is array (natural range <>) of bit;
             type g is array (work.p.idx_t range <>, integer range <>) of bit;
             type c is array (0 to 3) of bit;",
        );
        assert!(msgs.is_empty(), "{msgs:?}");
        let [DiscreteRange::Unconstrained { type_mark, span }] = array_indices(&decls[0]) else {
            panic!("expected one unconstrained index");
        };
        assert_eq!(interner.resolve(type_mark.parts[0]), "natural");
        assert!(span.end > span.start);
        let idx = array_indices(&decls[1]);
        assert_eq!(idx.len(), 2);
        assert!(matches!(
            &idx[0],
            DiscreteRange::Unconstrained { type_mark, .. } if type_mark.parts.len() == 3
        ));
        assert!(matches!(
            array_indices(&decls[2])[0],
            DiscreteRange::Range(_)
        ));
    }

    #[test]
    fn box_errors() {
        // `range <>` outside an array type definition.
        let (_, msgs, _) = package("signal s : bit_vector(natural range <>);");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(
            msgs[0].contains("only allowed in an array type"),
            "{msgs:?}"
        );

        // A bare `<>`.
        let (_, msgs, _) = package("type m is array (<>) of bit;");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(
            msgs[0].contains("must be preceded by an index subtype"),
            "{msgs:?}"
        );

        // Something other than a type mark before `range <>`.
        let (_, msgs, _) = package("type m is array (3 range <>) of bit;");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("type mark before `range <>`"), "{msgs:?}");

        // Mixing unconstrained and constrained indices.
        let (_, msgs, _) = package("type m is array (natural range <>, 0 to 3) of bit;");
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("cannot mix"), "{msgs:?}");

        // A box in a for-loop range.
        let (range, diags, _, _) = for_loop_range("natural range <>");
        assert!(matches!(range, DiscreteRange::Error(_)), "got {range:?}");
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn new_type_nodes_serde_roundtrip() {
        let (decls, msgs, _) = package(
            "type m is array (natural range <>) of bit;
             subtype s is (resolved) mem_t(open)(7 downto 0);",
        );
        assert!(msgs.is_empty(), "{msgs:?}");
        let json = serde_json::to_string(&decls).unwrap();
        let back: Vec<Declaration> = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            array_indices(&back[0])[0],
            DiscreteRange::Unconstrained { .. }
        ));
        let ti = subtype_ind(&back[1]);
        assert!(matches!(
            ti.resolution,
            Some(ResolutionIndication::Element(..))
        ));
        assert!(matches!(ti.constraint, Some(Constraint::Array { .. })));
        assert_eq!(ti.span, subtype_ind(&decls[1]).span);
    }
}
