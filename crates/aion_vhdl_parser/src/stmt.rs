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

impl VhdlParser<'_> {
    // ========================================================================
    // Concurrent statements
    // ========================================================================

    /// Parses concurrent statements until `end` or EOF.
    pub fn parse_concurrent_statements(&mut self) -> Vec<ConcurrentStatement> {
        let mut stmts = Vec::new();
        loop {
            match self.current() {
                VhdlToken::End | VhdlToken::Eof => break,
                _ => {
                    if let Some(stmt) = self.parse_concurrent_statement() {
                        stmts.push(stmt);
                    }
                }
            }
        }
        stmts
    }

    /// Parses a single concurrent statement.
    fn parse_concurrent_statement(&mut self) -> Option<ConcurrentStatement> {
        match self.current() {
            VhdlToken::Process => Some(self.parse_process_statement(None)),
            VhdlToken::Assert => Some(self.parse_concurrent_assert(None)),
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
                _ => {
                    // Component instantiation or other labeled statement
                    return Some(self.parse_component_instantiation(label, start));
                }
            }
        }

        // Not a label — backtrack and parse as signal assignment
        self.pos = saved_pos;
        Some(self.parse_concurrent_signal_assignment(None))
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
        let span = start.merge(self.prev_span());
        AssociationList { elements, span }
    }

    /// Parses a single association element (named or positional).
    fn parse_association_element(&mut self) -> AssociationElement {
        let start = self.current_span();
        let expr = self.parse_expr();

        if self.eat(VhdlToken::Arrow) {
            // Named association: formal => actual
            let actual = self.parse_expr();
            let span = start.merge(actual.span());
            AssociationElement {
                formal: Some(expr),
                actual,
                span,
            }
        } else {
            // Positional association
            let span = expr.span();
            AssociationElement {
                formal: None,
                actual: expr,
                span,
            }
        }
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

    /// Parses a concurrent signal assignment.
    ///
    /// The target is parsed as a name expression (not a full expression) to avoid
    /// consuming the `<=` operator as a relational comparison.
    fn parse_concurrent_signal_assignment(&mut self, label: Option<Ident>) -> ConcurrentStatement {
        let start = self.current_span();
        let target = self.parse_name_expr();
        self.expect(VhdlToken::LessEquals);

        let waveforms = self.parse_waveform_list();

        self.expect(VhdlToken::Semicolon);
        let span = start.merge(self.prev_span());

        ConcurrentStatement::SignalAssignment(ConcurrentSignalAssignment {
            label,
            target,
            waveforms,
            span,
        })
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
            VhdlToken::Identifier | VhdlToken::ExtendedIdentifier => {
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
            alternatives.push(self.parse_case_alternative());
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

        let mut choices = Vec::new();
        loop {
            let choice = self.parse_choice();
            choices.push(choice);
            if !self.eat(VhdlToken::Bar) {
                break;
            }
        }

        self.expect(VhdlToken::Arrow);
        let stmts = self.parse_sequential_statements();
        let span = start.merge(self.prev_span());

        CaseAlternative {
            choices,
            stmts,
            span,
        }
    }

    /// Parses a single choice.
    fn parse_choice(&mut self) -> Choice {
        if self.at(VhdlToken::Others) {
            let span = self.current_span();
            self.advance();
            Choice::Others(span)
        } else {
            let expr = self.parse_expr();
            // Check for range
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
                Choice::Expr(expr)
            }
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

    /// Parses an assignment (signal or variable) or procedure call.
    fn parse_assignment_or_call(&mut self) -> SequentialStatement {
        let start = self.current_span();
        let target = self.parse_name_expr();

        match self.current() {
            VhdlToken::LessEquals => {
                // Signal assignment
                self.advance();
                let waveforms = self.parse_waveform_list();
                self.expect(VhdlToken::Semicolon);
                let span = start.merge(self.prev_span());
                SequentialStatement::SignalAssignment {
                    target,
                    waveforms,
                    span,
                }
            }
            VhdlToken::ColonEquals => {
                // Variable assignment
                self.advance();
                let value = self.parse_expr();
                self.expect(VhdlToken::Semicolon);
                let span = start.merge(self.prev_span());
                SequentialStatement::VariableAssignment {
                    target,
                    value,
                    span,
                }
            }
            VhdlToken::Semicolon => {
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
                self.error("expected ':=', '<=', or ';' after name");
                self.recover_to_semicolon();
                SequentialStatement::Error(start.merge(span))
            }
        }
    }
}
