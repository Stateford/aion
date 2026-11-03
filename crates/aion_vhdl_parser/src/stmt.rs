//! Concurrent and sequential statement parsing for VHDL-2008.
//!
//! Concurrent statements appear in architecture bodies and include processes,
//! signal assignments, component instantiations, and generate statements.
//! Sequential statements appear in process bodies and subprogram bodies.

use crate::ast::*;
use crate::parser::VhdlParser;
use crate::token::VhdlToken;
use aion_common::Ident;
use aion_source::Span;

/// The right-hand side of a non-selected signal assignment, as parsed by
/// [`VhdlParser::parse_signal_assignment_rhs`].
enum SignalRhs {
    /// A plain waveform list: a simple signal assignment.
    Simple(Vec<Waveform>),
    /// `when`/`else` alternatives (or a lone `unaffected`): a conditional
    /// signal assignment.
    Conditional(Vec<ConditionalWaveform>),
}

/// The result of [`VhdlParser::parse_selected_assignment`].
enum SelectedParse {
    /// `with ... select target <= ...;`
    Signal(SelectedSignalAssignment),
    /// `with ... select target := ...;` (sequential only)
    Variable(SelectedVariableAssignment),
    /// `with ... select target <= force ...;` (sequential only)
    Force(SelectedForceAssignment),
    /// A malformed statement; a diagnostic has already been reported.
    Error(Span),
}

impl VhdlParser<'_> {
    // ========================================================================
    // Concurrent statements
    // ========================================================================

    /// Parses concurrent statements until `end` or EOF.
    pub fn parse_concurrent_statements(&mut self) -> Vec<ConcurrentStatement> {
        let mut stmts = Vec::new();
        loop {
            let iter_start = self.pos;
            match self.current() {
                VhdlToken::End | VhdlToken::Eof => break,
                _ => {
                    if let Some(stmt) = self.parse_concurrent_statement() {
                        stmts.push(stmt);
                    }
                }
            }
            self.ensure_progress(iter_start, "concurrent statement part");
        }
        stmts
    }

    /// Parses a single concurrent statement.
    fn parse_concurrent_statement(&mut self) -> Option<ConcurrentStatement> {
        match self.current() {
            VhdlToken::Process => Some(self.parse_process_statement(None)),
            VhdlToken::Assert => Some(self.parse_concurrent_assert(None)),
            VhdlToken::With => {
                let start = self.current_span();
                Some(self.parse_concurrent_selected_assignment(None, start))
            }
            VhdlToken::LeftParen => {
                // Aggregate target: `(a, b) <= ...;`
                let start = self.current_span();
                Some(self.parse_concurrent_signal_assignment(None, start))
            }
            VhdlToken::Identifier | VhdlToken::ExtendedIdentifier => {
                // Could be: label: process/component/generate, or signal assignment
                self.parse_labeled_or_signal_assignment()
            }
            _ => {
                let span = self.current_span();
                self.error("expected concurrent statement");
                self.recover_to_semicolon();
                Some(ConcurrentStatement::Error(span))
            }
        }
    }

    /// Parses a statement starting with an identifier — could be labeled or a signal assignment.
    fn parse_labeled_or_signal_assignment(&mut self) -> Option<ConcurrentStatement> {
        let start = self.current_span();

        // Save position to potentially backtrack
        let saved_pos = self.pos;

        let first_name = self.expect_ident();

        // Check for label: name :
        if self.at(VhdlToken::Colon) {
            self.advance(); // eat :
            let label = first_name;

            match self.current() {
                VhdlToken::Process => {
                    return Some(self.parse_process_statement(Some(label)));
                }
                VhdlToken::For => {
                    return Some(self.parse_for_generate(label));
                }
                VhdlToken::If => {
                    return Some(self.parse_if_generate(label));
                }
                VhdlToken::Assert => {
                    return Some(self.parse_concurrent_assert(Some(label)));
                }
                VhdlToken::With => {
                    return Some(self.parse_concurrent_selected_assignment(Some(label), start));
                }
                _ if self.at_signal_assignment_start() => {
                    return Some(self.parse_concurrent_signal_assignment(Some(label), start));
                }
                _ => {
                    // Component instantiation or other labeled statement
                    return Some(self.parse_component_instantiation(label, start));
                }
            }
        }

        // Not a label — backtrack and parse as signal assignment
        self.pos = saved_pos;
        Some(self.parse_concurrent_signal_assignment(None, start))
    }

    /// Returns `true` if the tokens from the current position look like a
    /// signal assignment (`name ... <= ...;` or `(a, b) <= ...;`) rather than
    /// a component instantiation.
    ///
    /// Used after a `label :` to tell `lbl : y <= a;` from `u1 : comp port map
    /// (...);`. Scans forward (without consuming) for a `<=` at parenthesis
    /// depth zero before the statement's `;` or a keyword that can only
    /// belong to an instantiation.
    fn at_signal_assignment_start(&self) -> bool {
        if !(self.at_identifier() || self.at(VhdlToken::LeftParen)) {
            return false;
        }
        let mut depth = 0usize;
        for tok in &self.tokens[self.pos..] {
            match tok.kind {
                VhdlToken::LeftParen => depth += 1,
                VhdlToken::RightParen => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                VhdlToken::LessEquals if depth == 0 => return true,
                VhdlToken::Semicolon
                | VhdlToken::Eof
                | VhdlToken::Port
                | VhdlToken::Generic
                | VhdlToken::Entity
                | VhdlToken::Component
                | VhdlToken::Configuration
                | VhdlToken::Begin
                | VhdlToken::End => return false,
                _ => {}
            }
        }
        false
    }

    /// Parses a process statement.
    fn parse_process_statement(&mut self, label: Option<Ident>) -> ConcurrentStatement {
        let start = if label.is_some() {
            self.prev_span()
        } else {
            self.current_span()
        };
        self.expect(VhdlToken::Process);

        // Sensitivity list
        let sensitivity = if self.at(VhdlToken::LeftParen) {
            self.advance();
            if self.eat(VhdlToken::All) {
                self.expect(VhdlToken::RightParen);
                SensitivityList::All
            } else {
                let mut signals = Vec::new();
                signals.push(self.parse_selected_name());
                while self.eat(VhdlToken::Comma) {
                    signals.push(self.parse_selected_name());
                }
                self.expect(VhdlToken::RightParen);
                SensitivityList::List(signals)
            }
        } else {
            SensitivityList::None
        };

        self.eat(VhdlToken::Is); // optional

        let decls = self.parse_declarations();
        self.expect(VhdlToken::Begin);
        let stmts = self.parse_sequential_statements();
        self.expect(VhdlToken::End);
        self.eat(VhdlToken::Process);
        self.eat_ident(); // optional label
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        ConcurrentStatement::Process(ProcessStatement {
            label,
            sensitivity,
            decls,
            stmts,
            span,
        })
    }

    /// Parses a component instantiation.
    fn parse_component_instantiation(&mut self, label: Ident, start: Span) -> ConcurrentStatement {
        // Parse the instantiated unit
        let unit = if self.eat(VhdlToken::Entity) {
            let name = self.parse_selected_name();
            let arch = if self.at(VhdlToken::LeftParen) {
                self.advance();
                let a = self.expect_ident();
                self.expect(VhdlToken::RightParen);
                Some(a)
            } else {
                None
            };
            InstantiatedUnit::Entity(name, arch)
        } else {
            self.eat(VhdlToken::Component); // optional keyword
            let name = self.parse_selected_name();
            InstantiatedUnit::Component(name)
        };

        let generic_map = if self.at(VhdlToken::Generic) {
            self.advance();
            self.expect(VhdlToken::Map);
            Some(self.parse_association_list())
        } else {
            None
        };

        let port_map = if self.at(VhdlToken::Port) {
            self.advance();
            self.expect(VhdlToken::Map);
            Some(self.parse_association_list())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        ConcurrentStatement::ComponentInstantiation(ComponentInstantiation {
            label,
            unit,
            generic_map,
            port_map,
            span,
        })
    }

    /// Parses an association list: `( element {, element} )`.
    pub fn parse_association_list(&mut self) -> AssociationList {
        let start = self.current_span();
        self.expect(VhdlToken::LeftParen);

        let mut elements = Vec::new();
        loop {
            let elem = self.parse_association_element();
            elements.push(elem);
            if !self.eat(VhdlToken::Comma) {
                break;
            }
        }

        self.expect(VhdlToken::RightParen);
        self.check_association_order(&elements);
        let span = start.merge(self.prev_span());
        AssociationList { elements, span }
    }

    /// Parses a single association element (named or positional).
    fn parse_association_element(&mut self) -> AssociationElement {
        let expr = self.parse_expr();
        self.finish_association_element(expr)
    }

    /// Parses a for-generate statement.
    fn parse_for_generate(&mut self, label: Ident) -> ConcurrentStatement {
        let start = self.current_span();
        self.expect(VhdlToken::For);
        let var = self.expect_ident();
        self.expect(VhdlToken::In);
        let range = self.parse_discrete_range();
        self.expect(VhdlToken::Generate);

        let stmts = self.parse_concurrent_statements();

        self.expect(VhdlToken::End);
        self.expect(VhdlToken::Generate);
        self.eat_ident(); // optional label
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        ConcurrentStatement::ForGenerate(ForGenerate {
            label,
            var,
            range,
            stmts,
            span,
        })
    }

    /// Parses an if-generate statement.
    fn parse_if_generate(&mut self, label: Ident) -> ConcurrentStatement {
        let start = self.current_span();
        self.expect(VhdlToken::If);
        let condition = self.parse_expr();
        self.expect(VhdlToken::Generate);

        let then_stmts = self.parse_concurrent_statements();

        let else_stmts = if self.at(VhdlToken::Else) {
            self.advance();
            self.eat(VhdlToken::Generate);
            self.parse_concurrent_statements()
        } else {
            Vec::new()
        };

        self.expect(VhdlToken::End);
        self.expect(VhdlToken::Generate);
        self.eat_ident();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        ConcurrentStatement::IfGenerate(IfGenerate {
            label,
            condition,
            then_stmts,
            else_stmts,
            span,
        })
    }

    /// Parses a concurrent assertion.
    fn parse_concurrent_assert(&mut self, label: Option<Ident>) -> ConcurrentStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Assert);
        let condition = self.parse_expr();

        let report = if self.eat(VhdlToken::Report) {
            Some(self.parse_expr())
        } else {
            None
        };

        let severity = if self.eat(VhdlToken::Severity) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        ConcurrentStatement::Assert(ConcurrentAssert {
            label,
            condition,
            report,
            severity,
            span,
        })
    }

    /// Parses a concurrent simple or conditional signal assignment, starting
    /// at the target. `start` is the span of the label (if any) or the target.
    ///
    /// The target is parsed as a name or aggregate (not a full expression) to
    /// avoid consuming the `<=` operator as a relational comparison.
    fn parse_concurrent_signal_assignment(
        &mut self,
        label: Option<Ident>,
        start: Span,
    ) -> ConcurrentStatement {
        let target = self.parse_assignment_target();
        if !self.eat(VhdlToken::LessEquals) {
            self.expected("'<=' in concurrent signal assignment");
            self.recover_to_semicolon_or(&[VhdlToken::End]);
            return ConcurrentStatement::Error(start.merge(self.prev_span()));
        }
        if self.at(VhdlToken::Force) || self.at(VhdlToken::Release) {
            self.reject_concurrent_force();
            return ConcurrentStatement::Error(start.merge(self.prev_span()));
        }

        let delay = self.parse_delay_mechanism();
        let rhs = self.parse_signal_assignment_rhs();
        self.finish_statement();
        let span = start.merge(self.prev_span());

        match rhs {
            SignalRhs::Simple(waveforms) => {
                ConcurrentStatement::SignalAssignment(ConcurrentSignalAssignment {
                    label,
                    target,
                    delay,
                    waveforms,
                    span,
                })
            }
            SignalRhs::Conditional(alternatives) => {
                ConcurrentStatement::ConditionalSignalAssignment(ConditionalSignalAssignment {
                    label,
                    target,
                    delay,
                    alternatives,
                    span,
                })
            }
        }
    }

    /// Parses a concurrent selected signal assignment starting at `with`.
    /// `start` is the span of the label (if any) or the `with` keyword.
    fn parse_concurrent_selected_assignment(
        &mut self,
        label: Option<Ident>,
        start: Span,
    ) -> ConcurrentStatement {
        match self.parse_selected_assignment(label, start, false) {
            SelectedParse::Signal(sa) => ConcurrentStatement::SelectedSignalAssignment(sa),
            // Variable and force forms are rejected (with a diagnostic) when
            // `sequential` is false, so only the error case remains.
            SelectedParse::Variable(v) => ConcurrentStatement::Error(v.span),
            SelectedParse::Force(f) => ConcurrentStatement::Error(f.span),
            SelectedParse::Error(span) => ConcurrentStatement::Error(span),
        }
    }

    /// Reports a force/release assignment in concurrent code (only legal in
    /// sequential code) and skips the rest of the statement.
    fn reject_concurrent_force(&mut self) {
        self.error(
            "force and release assignments are only allowed in sequential code \
             (inside a process or subprogram)",
        );
        self.recover_to_semicolon_or(&[VhdlToken::End]);
    }

    /// Parses an assignment target: a name, or a parenthesized aggregate of
    /// names such as `(carry, sum)`.
    fn parse_assignment_target(&mut self) -> Expr {
        if self.at(VhdlToken::LeftParen) {
            self.parse_paren_or_aggregate()
        } else {
            self.parse_name_expr()
        }
    }

    /// Consumes the `;` that ends a statement.
    ///
    /// If the current token is `end`, `elsif`, `else` or `when`, the `;` was
    /// most likely just forgotten before the next clause, so one error is
    /// reported and the token is left for the enclosing construct. Otherwise
    /// one error is reported and the rest of the malformed statement is
    /// skipped up to and including the next `;` (or up to an `end`), so its
    /// leftovers do not cascade into further "expected statement" errors.
    fn finish_statement(&mut self) {
        if self.eat(VhdlToken::Semicolon) {
            return;
        }
        self.expected("Semicolon");
        if matches!(
            self.current(),
            VhdlToken::End | VhdlToken::Elsif | VhdlToken::Else | VhdlToken::When
        ) {
            return;
        }
        self.recover_to_semicolon_or(&[VhdlToken::End]);
    }

    /// Skips tokens up to (not including) the next `;`, `end` or EOF.
    fn skip_to_statement_end(&mut self) {
        while !self.at_eof() && !self.at(VhdlToken::Semicolon) && !self.at(VhdlToken::End) {
            self.advance();
        }
    }

    /// Returns `true` if the current token is a `when` that continues a
    /// conditional assignment (`... when cond [else ...]`).
    ///
    /// A `when` whose choices are followed by `=>` at parenthesis depth zero
    /// starts a case alternative instead (the previous statement is missing
    /// its `;`), so it is left for the enclosing case statement.
    fn at_conditional_when(&self) -> bool {
        if !self.at(VhdlToken::When) {
            return false;
        }
        let mut depth = 0usize;
        for tok in &self.tokens[self.pos + 1..] {
            match tok.kind {
                VhdlToken::LeftParen => depth += 1,
                VhdlToken::RightParen => depth = depth.saturating_sub(1),
                VhdlToken::Arrow if depth == 0 => return false,
                VhdlToken::Semicolon
                | VhdlToken::Else
                | VhdlToken::LessEquals
                | VhdlToken::ColonEquals
                | VhdlToken::End
                | VhdlToken::Eof
                    if depth == 0 =>
                {
                    return true
                }
                _ => {}
            }
        }
        true
    }

    /// Parses an optional delay mechanism: `transport`, `inertial`, or
    /// `reject <time> inertial`.
    fn parse_delay_mechanism(&mut self) -> Option<DelayMechanism> {
        let start = self.current_span();
        match self.current() {
            VhdlToken::Transport => {
                self.advance();
                Some(DelayMechanism::Transport { span: start })
            }
            VhdlToken::Inertial => {
                self.advance();
                Some(DelayMechanism::Inertial {
                    reject: None,
                    span: start,
                })
            }
            VhdlToken::Reject => {
                self.advance();
                let limit = self.parse_expr();
                self.expect(VhdlToken::Inertial);
                Some(DelayMechanism::Inertial {
                    reject: Some(limit),
                    span: start.merge(self.prev_span()),
                })
            }
            _ => None,
        }
    }

    /// Parses the waveform part of a signal assignment after `<=` and the
    /// optional delay mechanism, up to (not including) the final `;`.
    ///
    /// Returns [`SignalRhs::Conditional`] for `wf when c [else ...]` and for
    /// a lone `unaffected`; otherwise [`SignalRhs::Simple`].
    fn parse_signal_assignment_rhs(&mut self) -> SignalRhs {
        let first = self.parse_signal_waveform();
        if self.at_conditional_when() {
            return SignalRhs::Conditional(self.parse_conditional_waveforms(first));
        }
        match first {
            SignalWaveform::Elements(waveforms) => SignalRhs::Simple(waveforms),
            unaffected @ SignalWaveform::Unaffected(_) => {
                let span = unaffected.span();
                SignalRhs::Conditional(vec![ConditionalWaveform {
                    waveform: unaffected,
                    condition: None,
                    span,
                }])
            }
        }
    }

    /// Parses a waveform: `unaffected` or a list of waveform elements.
    fn parse_signal_waveform(&mut self) -> SignalWaveform {
        if self.at(VhdlToken::Unaffected) {
            let span = self.current_span();
            self.advance();
            SignalWaveform::Unaffected(span)
        } else {
            SignalWaveform::Elements(self.parse_waveform_list())
        }
    }

    /// Parses the `when cond [else waveform ...]` chain that follows the
    /// first waveform of a conditional signal assignment.
    ///
    /// Each iteration consumes a `when`, so the loop always makes progress.
    fn parse_conditional_waveforms(&mut self, first: SignalWaveform) -> Vec<ConditionalWaveform> {
        let mut alternatives = Vec::new();
        let mut waveform = first;
        loop {
            let wf_span = waveform.span();
            if !self.at_conditional_when() {
                // Trailing unconditional `else waveform`.
                alternatives.push(ConditionalWaveform {
                    waveform,
                    condition: None,
                    span: wf_span,
                });
                break;
            }
            self.advance(); // eat `when`
            let condition = self.parse_expr();
            let span = wf_span.merge(condition.span());
            alternatives.push(ConditionalWaveform {
                waveform,
                condition: Some(condition),
                span,
            });
            if !self.eat(VhdlToken::Else) {
                break;
            }
            waveform = self.parse_signal_waveform();
        }
        alternatives
    }

    /// Parses the `when cond [else expr ...]` chain that follows the first
    /// value of a conditional variable or force assignment. With no `when`,
    /// returns the single unconditional alternative `first`.
    fn parse_conditional_exprs(&mut self, first: Expr) -> Vec<ConditionalExpr> {
        let mut alternatives = Vec::new();
        let mut value = first;
        loop {
            let value_span = value.span();
            if !self.at_conditional_when() {
                alternatives.push(ConditionalExpr {
                    value,
                    condition: None,
                    span: value_span,
                });
                break;
            }
            self.advance(); // eat `when`
            let condition = self.parse_expr();
            let span = value_span.merge(condition.span());
            alternatives.push(ConditionalExpr {
                value,
                condition: Some(condition),
                span,
            });
            if !self.eat(VhdlToken::Else) {
                break;
            }
            value = self.parse_expr();
        }
        alternatives
    }

    /// Parses a selected assignment starting at `with`, in concurrent
    /// (`sequential == false`) or sequential context:
    /// `with sel select[?] target <= [delay] wf when choices {, ...};`,
    /// `with sel select[?] target := e when choices {, ...};` (sequential), or
    /// `with sel select[?] target <= force [in|out] e when choices {, ...};`
    /// (sequential).
    fn parse_selected_assignment(
        &mut self,
        label: Option<Ident>,
        start: Span,
        sequential: bool,
    ) -> SelectedParse {
        self.expect(VhdlToken::With);
        let selector = self.parse_expr();
        if !self.eat(VhdlToken::Select) {
            self.expected("Select");
            self.recover_to_semicolon_or(&[VhdlToken::End]);
            return SelectedParse::Error(start.merge(self.prev_span()));
        }
        let matching = self.eat(VhdlToken::Question);
        let target = self.parse_assignment_target();

        match self.current() {
            VhdlToken::LessEquals => {
                self.advance();
                if self.at(VhdlToken::Force) && sequential {
                    self.advance();
                    let mode = self.parse_force_mode();
                    let Some(alternatives) = self.parse_selected_exprs() else {
                        return SelectedParse::Error(start.merge(self.prev_span()));
                    };
                    self.finish_statement();
                    return SelectedParse::Force(SelectedForceAssignment {
                        label,
                        selector,
                        matching,
                        target,
                        mode,
                        alternatives,
                        span: start.merge(self.prev_span()),
                    });
                }
                if self.at(VhdlToken::Force) {
                    self.reject_concurrent_force();
                    return SelectedParse::Error(start.merge(self.prev_span()));
                }
                if self.at(VhdlToken::Release) {
                    self.error(
                        "a release assignment cannot be selected; \
                         write `target <= release;` as its own statement",
                    );
                    self.recover_to_semicolon_or(&[VhdlToken::End]);
                    return SelectedParse::Error(start.merge(self.prev_span()));
                }
                let delay = self.parse_delay_mechanism();
                let Some(alternatives) = self.parse_selected_waveforms() else {
                    return SelectedParse::Error(start.merge(self.prev_span()));
                };
                self.finish_statement();
                SelectedParse::Signal(SelectedSignalAssignment {
                    label,
                    selector,
                    matching,
                    target,
                    delay,
                    alternatives,
                    span: start.merge(self.prev_span()),
                })
            }
            VhdlToken::ColonEquals if sequential => {
                self.advance();
                let Some(alternatives) = self.parse_selected_exprs() else {
                    return SelectedParse::Error(start.merge(self.prev_span()));
                };
                self.finish_statement();
                SelectedParse::Variable(SelectedVariableAssignment {
                    label,
                    selector,
                    matching,
                    target,
                    alternatives,
                    span: start.merge(self.prev_span()),
                })
            }
            VhdlToken::ColonEquals => {
                self.error(
                    "variable assignments are only allowed in sequential code \
                     (inside a process or subprogram)",
                );
                self.recover_to_semicolon_or(&[VhdlToken::End]);
                SelectedParse::Error(start.merge(self.prev_span()))
            }
            _ => {
                self.expected(if sequential {
                    "'<=' or ':=' in selected assignment"
                } else {
                    "'<=' in selected signal assignment"
                });
                self.recover_to_semicolon_or(&[VhdlToken::End]);
                SelectedParse::Error(start.merge(self.prev_span()))
            }
        }
    }

    /// Parses `waveform when choices {, waveform when choices}` of a selected
    /// signal assignment, stopping before the final `;`.
    ///
    /// Returns `None` (after reporting one error and skipping to the end of
    /// the statement, consuming its `;`) if an alternative lacks `when`.
    fn parse_selected_waveforms(&mut self) -> Option<Vec<SelectedWaveform>> {
        let mut alternatives = Vec::new();
        // Progress: each iteration either ends the loop or consumes a `,`.
        loop {
            let start = self.current_span();
            let waveform = self.parse_signal_waveform();
            if !self.eat(VhdlToken::When) {
                self.expected("When");
                self.skip_to_statement_end();
                self.eat(VhdlToken::Semicolon);
                return None;
            }
            let choices = self.parse_choices();
            alternatives.push(SelectedWaveform {
                waveform,
                choices,
                span: start.merge(self.prev_span()),
            });
            if !self.eat(VhdlToken::Comma) {
                break;
            }
        }
        Some(alternatives)
    }

    /// Parses `expr when choices {, expr when choices}` of a selected
    /// variable or force assignment, stopping before the final `;`.
    ///
    /// Returns `None` (after reporting one error and skipping to the end of
    /// the statement, consuming its `;`) if an alternative lacks `when`.
    fn parse_selected_exprs(&mut self) -> Option<Vec<SelectedExpr>> {
        let mut alternatives = Vec::new();
        // Progress: each iteration either ends the loop or consumes a `,`.
        loop {
            let value = self.parse_expr();
            if !self.eat(VhdlToken::When) {
                self.expected("When");
                self.skip_to_statement_end();
                self.eat(VhdlToken::Semicolon);
                return None;
            }
            let choices = self.parse_choices();
            let span = value.span().merge(self.prev_span());
            alternatives.push(SelectedExpr {
                value,
                choices,
                span,
            });
            if !self.eat(VhdlToken::Comma) {
                break;
            }
        }
        Some(alternatives)
    }

    /// Parses an optional force/release mode: `in` or `out`.
    fn parse_force_mode(&mut self) -> Option<ForceMode> {
        if self.eat(VhdlToken::In) {
            Some(ForceMode::In)
        } else if self.eat(VhdlToken::Out) {
            Some(ForceMode::Out)
        } else {
            None
        }
    }

    /// Parses a waveform list (comma-separated waveform elements).
    fn parse_waveform_list(&mut self) -> Vec<Waveform> {
        let mut waveforms = Vec::new();
        loop {
            let start = self.current_span();
            let value = self.parse_expr();

            let after = if self.eat(VhdlToken::After) {
                Some(self.parse_expr())
            } else {
                None
            };

            let span = if let Some(ref a) = after {
                start.merge(a.span())
            } else {
                start.merge(value.span())
            };
            waveforms.push(Waveform { value, after, span });

            if !self.eat(VhdlToken::Comma) {
                break;
            }
        }
        waveforms
    }

    // ========================================================================
    // Sequential statements
    // ========================================================================

    /// Parses sequential statements until `end`, `elsif`, `else`, `when`, or EOF.
    pub fn parse_sequential_statements(&mut self) -> Vec<SequentialStatement> {
        let mut stmts = Vec::new();
        loop {
            let iter_start = self.pos;
            match self.current() {
                VhdlToken::End
                | VhdlToken::Elsif
                | VhdlToken::Else
                | VhdlToken::When
                | VhdlToken::Eof => break,
                _ => {
                    if let Some(stmt) = self.parse_sequential_statement() {
                        stmts.push(stmt);
                    }
                }
            }
            self.ensure_progress(iter_start, "sequential statement part");
        }
        stmts
    }

    /// Parses a single sequential statement.
    fn parse_sequential_statement(&mut self) -> Option<SequentialStatement> {
        match self.current() {
            VhdlToken::If => Some(self.parse_if_statement()),
            VhdlToken::Case => Some(self.parse_case_statement()),
            VhdlToken::For => Some(self.parse_for_loop()),
            VhdlToken::While => Some(self.parse_while_loop()),
            VhdlToken::Loop => Some(self.parse_loop_statement(None)),
            VhdlToken::Next => Some(self.parse_next_statement()),
            VhdlToken::Exit => Some(self.parse_exit_statement()),
            VhdlToken::Return => Some(self.parse_return_statement()),
            VhdlToken::Wait => Some(self.parse_wait_statement()),
            VhdlToken::Assert => Some(self.parse_assert_statement()),
            VhdlToken::Report => Some(self.parse_report_statement()),
            VhdlToken::Null => Some(self.parse_null_statement()),
            VhdlToken::With => Some(self.parse_sequential_selected_assignment()),
            // `(a, b) <= ...;` / `(a, b) := ...;` — aggregate target.
            VhdlToken::Identifier | VhdlToken::ExtendedIdentifier | VhdlToken::LeftParen => {
                Some(self.parse_assignment_or_call())
            }
            _ => {
                let span = self.current_span();
                self.error("expected sequential statement");
                self.recover_to_semicolon();
                Some(SequentialStatement::Error(span))
            }
        }
    }

    /// Parses an if statement.
    fn parse_if_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::If);
        let condition = self.parse_expr();
        self.expect(VhdlToken::Then);

        let then_stmts = self.parse_sequential_statements();

        let mut elsif_branches = Vec::new();
        // Progress: each iteration consumes the `elsif` keyword.
        while self.at(VhdlToken::Elsif) {
            let elsif_start = self.current_span();
            self.advance();
            let cond = self.parse_expr();
            self.expect(VhdlToken::Then);
            let stmts = self.parse_sequential_statements();
            let span = elsif_start.merge(self.prev_span());
            elsif_branches.push(ElsifBranch {
                condition: cond,
                stmts,
                span,
            });
        }

        let else_stmts = if self.eat(VhdlToken::Else) {
            self.parse_sequential_statements()
        } else {
            Vec::new()
        };

        self.expect(VhdlToken::End);
        self.expect(VhdlToken::If);
        self.eat_ident(); // optional label
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::If(IfStatement {
            label: None,
            condition,
            then_stmts,
            elsif_branches,
            else_stmts,
            span,
        })
    }

    /// Parses a case statement.
    fn parse_case_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Case);
        let expr = self.parse_expr();
        self.expect(VhdlToken::Is);

        let mut alternatives = Vec::new();
        while self.at(VhdlToken::When) {
            let iter_start = self.pos;
            alternatives.push(self.parse_case_alternative());
            self.ensure_progress(iter_start, "case statement");
        }

        self.expect(VhdlToken::End);
        self.expect(VhdlToken::Case);
        self.eat_ident();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::Case(CaseStatement {
            label: None,
            expr,
            alternatives,
            span,
        })
    }

    /// Parses a case alternative: `when choices => stmts`.
    fn parse_case_alternative(&mut self) -> CaseAlternative {
        let start = self.current_span();
        self.expect(VhdlToken::When);

        let choices = self.parse_choices();

        self.expect(VhdlToken::Arrow);
        let stmts = self.parse_sequential_statements();
        let span = start.merge(self.prev_span());

        CaseAlternative {
            choices,
            stmts,
            span,
        }
    }

    /// Parses a for loop.
    fn parse_for_loop(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::For);
        let var = self.expect_ident();
        self.expect(VhdlToken::In);
        let range = self.parse_discrete_range();
        self.expect(VhdlToken::Loop);

        let stmts = self.parse_sequential_statements();

        self.expect(VhdlToken::End);
        self.expect(VhdlToken::Loop);
        self.eat_ident();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::ForLoop(ForLoop {
            label: None,
            var,
            range,
            stmts,
            span,
        })
    }

    /// Parses a while loop.
    fn parse_while_loop(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::While);
        let condition = self.parse_expr();
        self.expect(VhdlToken::Loop);

        let stmts = self.parse_sequential_statements();

        self.expect(VhdlToken::End);
        self.expect(VhdlToken::Loop);
        self.eat_ident();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::WhileLoop(WhileLoop {
            label: None,
            condition,
            stmts,
            span,
        })
    }

    /// Parses a plain loop.
    fn parse_loop_statement(&mut self, _label: Option<Ident>) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Loop);

        let stmts = self.parse_sequential_statements();

        self.expect(VhdlToken::End);
        self.expect(VhdlToken::Loop);
        self.eat_ident();
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::Loop(LoopStatement {
            label: None,
            stmts,
            span,
        })
    }

    /// Parses a next statement.
    fn parse_next_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Next);

        let label = if self.at(VhdlToken::Identifier) && !self.at(VhdlToken::When) {
            if self.peek_is(VhdlToken::When) || self.peek_is(VhdlToken::Semicolon) {
                Some(self.expect_ident())
            } else {
                None
            }
        } else {
            None
        };

        let condition = if self.eat(VhdlToken::When) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::Next {
            label,
            condition,
            span,
        }
    }

    /// Parses an exit statement.
    fn parse_exit_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Exit);

        let label = if self.at(VhdlToken::Identifier) && !self.at(VhdlToken::When) {
            if self.peek_is(VhdlToken::When) || self.peek_is(VhdlToken::Semicolon) {
                Some(self.expect_ident())
            } else {
                None
            }
        } else {
            None
        };

        let condition = if self.eat(VhdlToken::When) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::Exit {
            label,
            condition,
            span,
        }
    }

    /// Parses a return statement.
    fn parse_return_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Return);

        let value = if !self.at(VhdlToken::Semicolon) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::Return { value, span }
    }

    /// Parses a wait statement.
    fn parse_wait_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Wait);

        // wait on signal_list
        let on = if self.eat(VhdlToken::On) {
            let mut signals = Vec::new();
            signals.push(self.parse_selected_name());
            while self.eat(VhdlToken::Comma) {
                signals.push(self.parse_selected_name());
            }
            signals
        } else {
            Vec::new()
        };

        // wait until condition
        let until = if self.eat(VhdlToken::Until) {
            Some(self.parse_expr())
        } else {
            None
        };

        // wait for time
        let duration = if self.eat(VhdlToken::For) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::Wait(WaitStatement {
            on,
            until,
            duration,
            span,
        })
    }

    /// Parses an assert statement.
    fn parse_assert_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Assert);
        let condition = self.parse_expr();

        let report = if self.eat(VhdlToken::Report) {
            Some(self.parse_expr())
        } else {
            None
        };

        let severity = if self.eat(VhdlToken::Severity) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::Assert {
            condition,
            report,
            severity,
            span,
        }
    }

    /// Parses a report statement.
    fn parse_report_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Report);
        let message = self.parse_expr();

        let severity = if self.eat(VhdlToken::Severity) {
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        SequentialStatement::Report {
            message,
            severity,
            span,
        }
    }

    /// Parses a null statement.
    fn parse_null_statement(&mut self) -> SequentialStatement {
        let start = self.current_span();
        self.expect(VhdlToken::Null);
        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());
        SequentialStatement::Null { span }
    }

    /// Parses an assignment (signal, variable, force or release) or a
    /// procedure call, starting at the target name or aggregate.
    fn parse_assignment_or_call(&mut self) -> SequentialStatement {
        let start = self.current_span();
        let target = self.parse_assignment_target();

        match self.current() {
            VhdlToken::LessEquals => {
                // Signal assignment (simple, conditional, force or release)
                self.advance();
                match self.current() {
                    VhdlToken::Force => return self.parse_force_assignment(target, start),
                    VhdlToken::Release => return self.parse_release_assignment(target, start),
                    _ => {}
                }
                let delay = self.parse_delay_mechanism();
                let rhs = self.parse_signal_assignment_rhs();
                self.finish_statement();
                let span = start.merge(self.prev_span());
                match rhs {
                    SignalRhs::Simple(waveforms) => SequentialStatement::SignalAssignment {
                        target,
                        delay,
                        waveforms,
                        span,
                    },
                    SignalRhs::Conditional(alternatives) => {
                        SequentialStatement::ConditionalSignalAssignment(
                            ConditionalSignalAssignment {
                                label: None,
                                target,
                                delay,
                                alternatives,
                                span,
                            },
                        )
                    }
                }
            }
            VhdlToken::ColonEquals => {
                // Variable assignment (simple or conditional)
                self.advance();
                let value = self.parse_expr();
                if self.at_conditional_when() {
                    let alternatives = self.parse_conditional_exprs(value);
                    self.finish_statement();
                    let span = start.merge(self.prev_span());
                    return SequentialStatement::ConditionalVariableAssignment(
                        ConditionalVariableAssignment {
                            label: None,
                            target,
                            alternatives,
                            span,
                        },
                    );
                }
                self.finish_statement();
                let span = start.merge(self.prev_span());
                SequentialStatement::VariableAssignment {
                    target,
                    value,
                    span,
                }
            }
            VhdlToken::Semicolon if matches!(target, Expr::Name(_)) => {
                // Procedure call (no arguments, or arguments already in name)
                self.advance();
                let span = start.merge(self.prev_span());
                SequentialStatement::ProcedureCall {
                    name: target,
                    args: None,
                    span,
                }
            }
            _ => {
                let span = self.current_span();
                if matches!(target, Expr::Name(_)) {
                    self.error("expected ':=', '<=', or ';' after name");
                } else {
                    self.error("expected ':=' or '<=' after aggregate target");
                }
                self.recover_to_semicolon();
                SequentialStatement::Error(start.merge(span))
            }
        }
    }

    /// Parses a VHDL-2008 simple or conditional force assignment after
    /// `target <=`: `force [in|out] expr [when cond else ...];`.
    fn parse_force_assignment(&mut self, target: Expr, start: Span) -> SequentialStatement {
        self.expect(VhdlToken::Force);
        let mode = self.parse_force_mode();
        let value = self.parse_expr();
        let alternatives = self.parse_conditional_exprs(value);
        self.finish_statement();
        let span = start.merge(self.prev_span());
        SequentialStatement::ForceAssignment(ForceAssignment {
            label: None,
            target,
            mode,
            alternatives,
            span,
        })
    }

    /// Parses a VHDL-2008 release assignment after `target <=`:
    /// `release [in|out];`.
    fn parse_release_assignment(&mut self, target: Expr, start: Span) -> SequentialStatement {
        self.expect(VhdlToken::Release);
        let mode = self.parse_force_mode();
        self.finish_statement();
        let span = start.merge(self.prev_span());
        SequentialStatement::ReleaseAssignment(ReleaseAssignment {
            label: None,
            target,
            mode,
            span,
        })
    }

    /// Parses a VHDL-2008 sequential selected assignment (signal, variable
    /// or force) starting at `with`.
    fn parse_sequential_selected_assignment(&mut self) -> SequentialStatement {
        let start = self.current_span();
        match self.parse_selected_assignment(None, start, true) {
            SelectedParse::Signal(sa) => SequentialStatement::SelectedSignalAssignment(sa),
            SelectedParse::Variable(va) => SequentialStatement::SelectedVariableAssignment(va),
            SelectedParse::Force(fa) => SequentialStatement::SelectedForceAssignment(fa),
            SelectedParse::Error(span) => SequentialStatement::Error(span),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::*;
    use crate::lexer;
    use crate::parser::VhdlParser;
    use aion_common::Interner;
    use aion_diagnostics::{Diagnostic, DiagnosticSink};
    use aion_source::FileId;

    /// Parses `source`, returning the AST, the diagnostics and the interner
    /// (for resolving identifiers).
    fn parse_vhdl(source: &str) -> (VhdlDesignFile, Vec<Diagnostic>, Interner) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = FileId::from_raw(0);
        let tokens = lexer::lex(source, file, &sink);
        let ast = VhdlParser::new(tokens, source, file, &interner, &sink).parse_design_file();
        (ast, sink.take_all(), interner)
    }

    fn messages(diags: &[Diagnostic]) -> Vec<&str> {
        diags.iter().map(|d| d.message.as_str()).collect()
    }

    /// Wraps concurrent statements in an architecture and returns them,
    /// asserting the parse is error-free.
    fn arch_stmts(body: &str) -> Vec<ConcurrentStatement> {
        let (ast, diags, _) = parse_vhdl(&arch_src(body));
        assert!(
            diags.is_empty(),
            "unexpected errors: {:?}",
            messages(&diags)
        );
        arch_of(ast)
    }

    fn arch_src(body: &str) -> String {
        format!("architecture rtl of top is\nbegin\n{body}\nend architecture rtl;")
    }

    fn arch_of(ast: VhdlDesignFile) -> Vec<ConcurrentStatement> {
        match ast.units.into_iter().next() {
            Some(DesignUnit::ContextUnit {
                unit: DesignUnitKind::Architecture(a),
                ..
            }) => a.stmts,
            other => panic!("expected architecture, got {other:?}"),
        }
    }

    /// Wraps sequential statements in a process and returns them, asserting
    /// the parse is error-free.
    fn process_stmts(body: &str) -> Vec<SequentialStatement> {
        let (ast, diags, _) = parse_vhdl(&process_src(body));
        assert!(
            diags.is_empty(),
            "unexpected errors: {:?}",
            messages(&diags)
        );
        process_of(ast)
    }

    fn process_src(body: &str) -> String {
        arch_src(&format!("process (all)\nbegin\n{body}\nend process;"))
    }

    fn process_of(ast: VhdlDesignFile) -> Vec<SequentialStatement> {
        match arch_of(ast).into_iter().next() {
            Some(ConcurrentStatement::Process(p)) => p.stmts,
            other => panic!("expected process, got {other:?}"),
        }
    }

    fn expect_conditional(stmt: &ConcurrentStatement) -> &ConditionalSignalAssignment {
        match stmt {
            ConcurrentStatement::ConditionalSignalAssignment(c) => c,
            other => panic!("expected conditional signal assignment, got {other:?}"),
        }
    }

    fn expect_selected(stmt: &ConcurrentStatement) -> &SelectedSignalAssignment {
        match stmt {
            ConcurrentStatement::SelectedSignalAssignment(s) => s,
            other => panic!("expected selected signal assignment, got {other:?}"),
        }
    }

    fn element_count(wf: &SignalWaveform) -> usize {
        match wf {
            SignalWaveform::Elements(e) => e.len(),
            SignalWaveform::Unaffected(_) => 0,
        }
    }

    // ========================================================================
    // Concurrent conditional signal assignments
    // ========================================================================

    #[test]
    fn concurrent_conditional_issue_repro() {
        let stmts = arch_stmts("y <= a when s = '1' else b;");
        assert_eq!(stmts.len(), 1);
        let c = expect_conditional(&stmts[0]);
        assert!(c.label.is_none());
        assert!(c.delay.is_none());
        assert_eq!(c.alternatives.len(), 2);
        assert!(matches!(
            c.alternatives[0].condition,
            Some(Expr::Binary {
                op: BinaryOp::Eq,
                ..
            })
        ));
        assert_eq!(element_count(&c.alternatives[0].waveform), 1);
        assert!(c.alternatives[1].condition.is_none());
    }

    #[test]
    fn concurrent_conditional_three_way_chain() {
        let stmts = arch_stmts(r#"y <= a when s = "00" else b when s = "01" else c;"#);
        let c = expect_conditional(&stmts[0]);
        assert_eq!(c.alternatives.len(), 3);
        assert!(c.alternatives[0].condition.is_some());
        assert!(c.alternatives[1].condition.is_some());
        assert!(c.alternatives[2].condition.is_none());
    }

    #[test]
    fn concurrent_conditional_without_final_else() {
        let stmts = arch_stmts("q <= d when en = '1';");
        let c = expect_conditional(&stmts[0]);
        assert_eq!(c.alternatives.len(), 1);
        assert!(c.alternatives[0].condition.is_some());
    }

    #[test]
    fn concurrent_conditional_with_unaffected() {
        let stmts = arch_stmts("y <= a when en = '1' else unaffected;");
        let c = expect_conditional(&stmts[0]);
        assert_eq!(c.alternatives.len(), 2);
        assert!(matches!(
            c.alternatives[1].waveform,
            SignalWaveform::Unaffected(_)
        ));
        assert!(c.alternatives[1].condition.is_none());
    }

    #[test]
    fn concurrent_lone_unaffected_is_conditional() {
        let stmts = arch_stmts("y <= unaffected;");
        let c = expect_conditional(&stmts[0]);
        assert_eq!(c.alternatives.len(), 1);
        assert!(matches!(
            c.alternatives[0].waveform,
            SignalWaveform::Unaffected(_)
        ));
    }

    #[test]
    fn concurrent_conditional_multi_element_waveforms() {
        let stmts =
            arch_stmts("y <= '1' after 1 ns, '0' after 2 ns when go = '1' else '0' after 3 ns;");
        let c = expect_conditional(&stmts[0]);
        assert_eq!(c.alternatives.len(), 2);
        assert_eq!(element_count(&c.alternatives[0].waveform), 2);
        assert_eq!(element_count(&c.alternatives[1].waveform), 1);
    }

    #[test]
    fn concurrent_simple_assignment_still_simple() {
        let stmts = arch_stmts("y <= a and b;");
        match &stmts[0] {
            ConcurrentStatement::SignalAssignment(sa) => {
                assert_eq!(sa.waveforms.len(), 1);
                assert!(sa.delay.is_none());
            }
            other => panic!("expected simple assignment, got {other:?}"),
        }
    }

    // ========================================================================
    // Concurrent selected signal assignments
    // ========================================================================

    #[test]
    fn concurrent_selected_issue_repro() {
        let stmts = arch_stmts("with s select y <= a when '1', b when others;");
        assert_eq!(stmts.len(), 1);
        let s = expect_selected(&stmts[0]);
        assert!(!s.matching);
        assert!(matches!(s.selector, Expr::Name(_)));
        assert!(matches!(s.target, Expr::Name(_)));
        assert_eq!(s.alternatives.len(), 2);
        assert!(matches!(s.alternatives[0].choices[..], [Choice::Expr(_)]));
        assert!(matches!(s.alternatives[1].choices[..], [Choice::Others(_)]));
    }

    #[test]
    fn concurrent_selected_choice_lists_and_ranges() {
        let stmts = arch_stmts(
            r#"with sel select y <= a when "00" | "01", b when "10", c when others;
               with n select z <= p when 0 to 3, q when others;"#,
        );
        assert_eq!(stmts.len(), 2);
        let s = expect_selected(&stmts[0]);
        assert_eq!(s.alternatives.len(), 3);
        assert_eq!(s.alternatives[0].choices.len(), 2);
        let r = expect_selected(&stmts[1]);
        assert!(matches!(r.alternatives[0].choices[..], [Choice::Range(_)]));
    }

    #[test]
    fn concurrent_selected_matching_form() {
        for src in [
            r#"with s select? y <= a when "1-", b when others;"#,
            r#"with s select ? y <= a when "1-", b when others;"#,
        ] {
            let stmts = arch_stmts(src);
            assert!(expect_selected(&stmts[0]).matching, "{src}");
        }
    }

    #[test]
    fn concurrent_selected_with_unaffected_and_multi_element() {
        let stmts = arch_stmts(
            "with s select y <= '1' after 1 ns, '0' after 2 ns when '1', unaffected when others;",
        );
        let s = expect_selected(&stmts[0]);
        assert_eq!(element_count(&s.alternatives[0].waveform), 2);
        assert!(matches!(
            s.alternatives[1].waveform,
            SignalWaveform::Unaffected(_)
        ));
    }

    #[test]
    fn concurrent_selected_expression_selector() {
        let stmts = arch_stmts("with a & b select y <= c when \"11\", d when others;");
        let s = expect_selected(&stmts[0]);
        assert!(matches!(
            s.selector,
            Expr::Binary {
                op: BinaryOp::Concat,
                ..
            }
        ));
    }

    // ========================================================================
    // Delay mechanisms
    // ========================================================================

    #[test]
    fn delay_mechanisms_on_simple_assignments() {
        let stmts = arch_stmts(
            "y1 <= transport a after 1 ns;
             y2 <= inertial a after 1 ns;
             y3 <= reject 2 ns inertial a after 5 ns;",
        );
        assert_eq!(stmts.len(), 3);
        let delays: Vec<_> = stmts
            .iter()
            .map(|s| match s {
                ConcurrentStatement::SignalAssignment(sa) => sa.delay.clone(),
                other => panic!("expected simple assignment, got {other:?}"),
            })
            .collect();
        assert!(matches!(delays[0], Some(DelayMechanism::Transport { .. })));
        assert!(matches!(
            delays[1],
            Some(DelayMechanism::Inertial { reject: None, .. })
        ));
        assert!(matches!(
            delays[2],
            Some(DelayMechanism::Inertial {
                reject: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn delay_mechanisms_on_conditional_and_selected() {
        let stmts = arch_stmts(
            "y <= transport a when c = '1' else b;
             with s select z <= reject 1 ns inertial a when '1', b when others;",
        );
        assert!(matches!(
            expect_conditional(&stmts[0]).delay,
            Some(DelayMechanism::Transport { .. })
        ));
        assert!(matches!(
            expect_selected(&stmts[1]).delay,
            Some(DelayMechanism::Inertial {
                reject: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn reject_without_inertial_is_one_error() {
        let (ast, diags, _) = parse_vhdl(&arch_src("y <= reject 1 ns a;\nz <= b;"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("Inertial"));
        assert_eq!(arch_of(ast).len(), 2);
    }

    // ========================================================================
    // Aggregate targets and labels
    // ========================================================================

    #[test]
    fn concurrent_aggregate_target() {
        let stmts = arch_stmts("(carry, sum) <= a when c = '1' else b;\n(x, y) <= v;");
        let c = expect_conditional(&stmts[0]);
        match &c.target {
            Expr::Aggregate { elements, .. } => assert_eq!(elements.len(), 2),
            other => panic!("expected aggregate target, got {other:?}"),
        }
        match &stmts[1] {
            ConcurrentStatement::SignalAssignment(sa) => {
                assert!(matches!(sa.target, Expr::Aggregate { .. }));
            }
            other => panic!("expected simple assignment, got {other:?}"),
        }
    }

    #[test]
    fn labeled_concurrent_assignments() {
        let (ast, diags, interner) = parse_vhdl(&arch_src(
            "mux : y <= a when s = '1' else b;
             dec : with s select z <= a when '1', b when others;
             cp : q <= d;
             u1 : entity work.counter port map (clk => clk);",
        ));
        assert!(diags.is_empty(), "{:?}", messages(&diags));
        let stmts = arch_of(ast);
        assert_eq!(stmts.len(), 4);
        let label = |l: Option<aion_common::Ident>| interner.resolve(l.unwrap()).to_string();
        assert_eq!(label(expect_conditional(&stmts[0]).label), "mux");
        assert_eq!(label(expect_selected(&stmts[1]).label), "dec");
        match &stmts[2] {
            ConcurrentStatement::SignalAssignment(sa) => assert_eq!(label(sa.label), "cp"),
            other => panic!("expected simple assignment, got {other:?}"),
        }
        assert!(matches!(
            stmts[3],
            ConcurrentStatement::ComponentInstantiation(_)
        ));
    }

    // ========================================================================
    // Concurrent error recovery
    // ========================================================================

    #[test]
    fn selected_alternative_without_when_is_one_error() {
        let (ast, diags, _) = parse_vhdl(&arch_src(
            "with s select y <= a, b;\nz <= c when d = '1' else e;",
        ));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("When"));
        let stmts = arch_of(ast);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[0], ConcurrentStatement::Error(_)));
        expect_conditional(&stmts[1]);
    }

    #[test]
    fn selected_missing_select_keyword_is_one_error() {
        let (ast, diags, _) = parse_vhdl(&arch_src("with s y <= a when others;\nz <= c;"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        let stmts = arch_of(ast);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[1], ConcurrentStatement::SignalAssignment(_)));
    }

    #[test]
    fn concurrent_force_is_rejected_without_cascade() {
        let (ast, diags, _) = parse_vhdl(&arch_src(
            "y <= force '1';\ny <= release;\nwith s select y <= force a when others;\nz <= c;",
        ));
        assert_eq!(diags.len(), 3, "{:?}", messages(&diags));
        assert!(diags.iter().all(|d| d.message.contains("sequential code")));
        let stmts = arch_of(ast);
        assert_eq!(stmts.len(), 4);
        assert!(matches!(stmts[3], ConcurrentStatement::SignalAssignment(_)));
    }

    #[test]
    fn concurrent_selected_variable_assignment_is_rejected() {
        let (ast, diags, _) = parse_vhdl(&arch_src("with s select v := a when others;\nz <= c;"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("sequential code"));
        assert_eq!(arch_of(ast).len(), 2);
    }

    #[test]
    fn trailing_garbage_after_conditional_is_one_error() {
        let (ast, diags, _) = parse_vhdl(&arch_src("y <= a when c = '1' else b q;\nz <= d;"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        let stmts = arch_of(ast);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[1], ConcurrentStatement::SignalAssignment(_)));
    }

    #[test]
    fn missing_semicolon_before_end_is_one_error() {
        let (ast, diags, _) = parse_vhdl(&arch_src("y <= a when c = '1' else b"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        expect_conditional(&arch_of(ast)[0]);
    }

    // ========================================================================
    // Sequential (VHDL-2008) conditional / selected assignments
    // ========================================================================

    #[test]
    fn sequential_conditional_signal_assignment_issue_repro() {
        let stmts = process_stmts("x <= a when c = '1' else b;\ny <= x;");
        assert_eq!(stmts.len(), 2);
        match &stmts[0] {
            SequentialStatement::ConditionalSignalAssignment(c) => {
                assert_eq!(c.alternatives.len(), 2);
                assert!(c.alternatives[1].condition.is_none());
            }
            other => panic!("expected conditional signal assignment, got {other:?}"),
        }
        assert!(matches!(
            stmts[1],
            SequentialStatement::SignalAssignment { .. }
        ));
    }

    #[test]
    fn sequential_selected_signal_assignment() {
        let stmts = process_stmts("with s select x <= transport a when '1', b when others;");
        match &stmts[0] {
            SequentialStatement::SelectedSignalAssignment(s) => {
                assert_eq!(s.alternatives.len(), 2);
                assert!(matches!(s.delay, Some(DelayMechanism::Transport { .. })));
            }
            other => panic!("expected selected signal assignment, got {other:?}"),
        }
    }

    #[test]
    fn sequential_simple_assignment_keeps_delay() {
        let stmts = process_stmts("x <= inertial a after 2 ns;");
        assert!(matches!(
            stmts[0],
            SequentialStatement::SignalAssignment {
                delay: Some(DelayMechanism::Inertial { .. }),
                ..
            }
        ));
    }

    #[test]
    fn conditional_variable_assignment() {
        let stmts = process_stmts("v := a when c = '1' else b when d = '1' else e;\nw := v;");
        match &stmts[0] {
            SequentialStatement::ConditionalVariableAssignment(c) => {
                assert_eq!(c.alternatives.len(), 3);
                assert!(c.alternatives[2].condition.is_none());
            }
            other => panic!("expected conditional variable assignment, got {other:?}"),
        }
        assert!(matches!(
            stmts[1],
            SequentialStatement::VariableAssignment { .. }
        ));
    }

    #[test]
    fn selected_variable_assignment() {
        let stmts = process_stmts("with s select? v := a when \"1-\", b when others;");
        match &stmts[0] {
            SequentialStatement::SelectedVariableAssignment(s) => {
                assert!(s.matching);
                assert_eq!(s.alternatives.len(), 2);
                assert!(matches!(s.alternatives[1].choices[..], [Choice::Others(_)]));
            }
            other => panic!("expected selected variable assignment, got {other:?}"),
        }
    }

    #[test]
    fn conditional_inside_case_alternative_and_if() {
        let stmts = process_stmts(
            "case s is
               when '0' => y <= a when en = '1' else b;
               when others => y <= c;
             end case;
             if go = '1' then
               v := a when en = '1' else b;
             else
               y <= unaffected;
             end if;",
        );
        assert_eq!(stmts.len(), 2);
        match &stmts[0] {
            SequentialStatement::Case(cs) => {
                assert_eq!(cs.alternatives.len(), 2);
                assert!(matches!(
                    cs.alternatives[0].stmts[0],
                    SequentialStatement::ConditionalSignalAssignment(_)
                ));
            }
            other => panic!("expected case, got {other:?}"),
        }
        match &stmts[1] {
            SequentialStatement::If(ifs) => {
                assert!(matches!(
                    ifs.then_stmts[0],
                    SequentialStatement::ConditionalVariableAssignment(_)
                ));
                assert!(matches!(
                    ifs.else_stmts[0],
                    SequentialStatement::ConditionalSignalAssignment(_)
                ));
            }
            other => panic!("expected if, got {other:?}"),
        }
    }

    #[test]
    fn next_and_exit_when_still_parse() {
        let stmts = process_stmts(
            "for i in 0 to 3 loop
               next when i = 1;
               exit when i = 2;
             end loop;",
        );
        match &stmts[0] {
            SequentialStatement::ForLoop(fl) => {
                assert!(matches!(
                    fl.stmts[0],
                    SequentialStatement::Next {
                        condition: Some(_),
                        ..
                    }
                ));
                assert!(matches!(
                    fl.stmts[1],
                    SequentialStatement::Exit {
                        condition: Some(_),
                        ..
                    }
                ));
            }
            other => panic!("expected for loop, got {other:?}"),
        }
    }

    // ========================================================================
    // Force / release
    // ========================================================================

    #[test]
    fn force_assignments() {
        let stmts = process_stmts(
            "x <= force '1';
             x <= force in a;
             x <= force out a when c = '1' else b;",
        );
        let forces: Vec<_> = stmts
            .iter()
            .map(|s| match s {
                SequentialStatement::ForceAssignment(f) => f,
                other => panic!("expected force assignment, got {other:?}"),
            })
            .collect();
        assert_eq!(forces[0].mode, None);
        assert_eq!(forces[0].alternatives.len(), 1);
        assert!(forces[0].alternatives[0].condition.is_none());
        assert_eq!(forces[1].mode, Some(ForceMode::In));
        assert_eq!(forces[2].mode, Some(ForceMode::Out));
        assert_eq!(forces[2].alternatives.len(), 2);
        assert!(forces[2].alternatives[0].condition.is_some());
    }

    #[test]
    fn release_assignments() {
        let stmts = process_stmts("x <= release;\nx <= release in;\nx <= release out;");
        let modes: Vec<_> = stmts
            .iter()
            .map(|s| match s {
                SequentialStatement::ReleaseAssignment(r) => r.mode,
                other => panic!("expected release assignment, got {other:?}"),
            })
            .collect();
        assert_eq!(modes, vec![None, Some(ForceMode::In), Some(ForceMode::Out)]);
    }

    #[test]
    fn selected_force_assignment() {
        let stmts = process_stmts("with s select x <= force in a when '1', b when others;");
        match &stmts[0] {
            SequentialStatement::SelectedForceAssignment(f) => {
                assert_eq!(f.mode, Some(ForceMode::In));
                assert_eq!(f.alternatives.len(), 2);
            }
            other => panic!("expected selected force assignment, got {other:?}"),
        }
    }

    #[test]
    fn selected_release_is_one_error() {
        let (ast, diags, _) = parse_vhdl(&process_src("with s select x <= release;\ny <= a;"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("release"));
        let stmts = process_of(ast);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(
            stmts[1],
            SequentialStatement::SignalAssignment { .. }
        ));
    }

    // ========================================================================
    // Sequential aggregate targets and error recovery
    // ========================================================================

    #[test]
    fn sequential_aggregate_targets() {
        let stmts = process_stmts("(a, b) <= c;\n(u, v) := w when e = '1' else z;");
        assert!(matches!(
            stmts[0],
            SequentialStatement::SignalAssignment {
                target: Expr::Aggregate { .. },
                ..
            }
        ));
        match &stmts[1] {
            SequentialStatement::ConditionalVariableAssignment(c) => {
                assert!(matches!(c.target, Expr::Aggregate { .. }));
            }
            other => panic!("expected conditional variable assignment, got {other:?}"),
        }
    }

    #[test]
    fn sequential_aggregate_without_assignment_is_one_error() {
        let (ast, diags, _) = parse_vhdl(&process_src("(a, b);\ny <= a;"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("aggregate target"));
        assert_eq!(process_of(ast).len(), 2);
    }

    #[test]
    fn case_choices_use_shared_choice_parser() {
        let stmts = process_stmts(
            "case s is
                when 1 | 3 to 5 | 7 => y <= a;
                when others => null;
            end case;",
        );
        let SequentialStatement::Case(c) = &stmts[0] else {
            panic!("expected case, got {:?}", stmts[0]);
        };
        assert!(matches!(
            c.alternatives[0].choices[..],
            [Choice::Expr(_), Choice::Range(_), Choice::Expr(_)]
        ));
    }

    #[test]
    fn case_others_combined_with_choices_is_rejected() {
        let (ast, diags, _) = parse_vhdl(&process_src(
            "case s is
                when 1 | others => y <= a;
            end case;
            y <= b;",
        ));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("cannot be combined"));
        assert_eq!(process_of(ast).len(), 2);

        // A missing choice is reported without losing the alternative.
        let (_, diags, _) = parse_vhdl(&process_src("case s is when 1 | => null; end case;"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("expected choice"));
    }

    #[test]
    fn named_association_procedure_calls() {
        let stmts = process_stmts("write(l, value => x);\nproc(a => open);");
        for stmt in &stmts {
            let SequentialStatement::ProcedureCall {
                name: Expr::Name(n),
                ..
            } = stmt
            else {
                panic!("expected procedure call, got {stmt:?}");
            };
            assert!(matches!(n.parts[..], [NameSuffix::Call(..)]));
        }
    }

    #[test]
    fn port_map_positional_after_named_is_rejected() {
        let stmts = arch_stmts("u0 : entity work.sub port map (clk, d => x, q => open);");
        assert_eq!(stmts.len(), 1);
        let (_, diags, _) = parse_vhdl(&arch_src("u0 : entity work.sub port map (d => x, clk);"));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("positional association"));
    }

    #[test]
    fn missing_semicolon_before_case_alternative_is_one_error() {
        // The `when` after `y <= a` starts the next case alternative, not a
        // condition: report the missing `;` once and keep both alternatives.
        let (ast, diags, _) = parse_vhdl(&process_src(
            "case s is
               when '0' => y <= a
               when others => y <= b;
             end case;",
        ));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(diags[0].message.contains("Semicolon"));
        match &process_of(ast)[0] {
            SequentialStatement::Case(cs) => assert_eq!(cs.alternatives.len(), 2),
            other => panic!("expected case, got {other:?}"),
        }
    }

    #[test]
    fn missing_semicolon_before_end_if_is_one_error() {
        let (ast, diags, _) = parse_vhdl(&process_src(
            "if c = '1' then y <= a when d = '1' else b end if;",
        ));
        assert_eq!(diags.len(), 1, "{:?}", messages(&diags));
        assert!(matches!(process_of(ast)[0], SequentialStatement::If(_)));
    }

    #[test]
    fn sequential_selected_missing_when_is_one_error_each() {
        let (ast, diags, _) = parse_vhdl(&process_src(
            "with s select v := a, b;\nwith s select x <= a;\ny <= c;",
        ));
        assert_eq!(diags.len(), 2, "{:?}", messages(&diags));
        let stmts = process_of(ast);
        assert_eq!(stmts.len(), 3);
        assert!(matches!(stmts[0], SequentialStatement::Error(_)));
        assert!(matches!(stmts[1], SequentialStatement::Error(_)));
    }
}
