//! Behavioral statements for process bodies.
//!
//! [`Statement`] represents language-independent behavioral code inside
//! processes (VHDL processes, Verilog always blocks).

use crate::expr::Expr;
use crate::signal::SignalRef;
use aion_source::Span;
use serde::{Deserialize, Serialize};

/// The kind of assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssertionKind {
    /// An `assert` statement — aborts on failure.
    Assert,
    /// An `assume` statement — constrains formal verification.
    Assume,
    /// A `cover` statement — marks a reachability goal.
    Cover,
}

/// A case arm in a case/switch statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseArm {
    /// The pattern expressions to match against.
    pub patterns: Vec<Expr>,
    /// The body to execute when matched.
    pub body: Statement,
    /// Source location.
    pub span: Span,
}

/// A behavioral statement in the IR.
///
/// Statements appear inside [`Process`](crate::process::Process) bodies
/// and represent the behavioral description of hardware. During synthesis,
/// these are lowered into combinational cells and flip-flops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Statement {
    /// A signal assignment (`target <= value` / `assign target = value`).
    Assign {
        /// The target signal or signal slice.
        target: SignalRef,
        /// The value expression.
        value: Expr,
        /// Source location.
        span: Span,
    },
    /// An if-else statement.
    If {
        /// The condition expression.
        condition: Expr,
        /// The body when condition is true.
        then_body: Box<Statement>,
        /// The optional body when condition is false.
        else_body: Option<Box<Statement>>,
        /// Source location.
        span: Span,
    },
    /// A case/switch statement.
    Case {
        /// The subject expression being matched.
        subject: Expr,
        /// The match arms.
        arms: Vec<CaseArm>,
        /// The default arm, if any.
        default: Option<Box<Statement>>,
        /// Source location.
        span: Span,
    },
    /// A block of sequential statements.
    Block {
        /// The statements in execution order.
        stmts: Vec<Statement>,
        /// Source location.
        span: Span,
    },
    /// A wait statement (simulation only, not synthesizable).
    Wait {
        /// The optional duration expression.
        duration: Option<Expr>,
        /// Source location.
        span: Span,
    },
    /// An assertion statement.
    Assertion {
        /// The kind of assertion.
        kind: AssertionKind,
        /// The condition to check.
        condition: Expr,
        /// An optional message string.
        message: Option<String>,
        /// Source location.
        span: Span,
    },
    /// A display/report statement (`$display` / `report`).
    Display {
        /// The format string.
        format: String,
        /// The format arguments.
        args: Vec<Expr>,
        /// Source location.
        span: Span,
    },
    /// A simulation finish statement (`$finish` / `std.env.stop`).
    Finish {
        /// Source location.
        span: Span,
    },
    /// A time delay statement (`#5`, `wait for 10 ns`).
    ///
    /// Suspends process execution for `duration_fs` femtoseconds, then
    /// resumes with `body`. Used by initial blocks and testbenches.
    Delay {
        /// Delay duration in femtoseconds (pre-evaluated at elaboration time).
        duration_fs: u64,
        /// The statement to execute after the delay elapses.
        body: Box<Statement>,
        /// Source location.
        span: Span,
    },
    /// An infinite loop (`forever`) wrapping a body statement.
    ///
    /// Typically contains a delay to generate periodic signals (e.g. clocks).
    /// A forever loop without any delay inside is a simulation error.
    Forever {
        /// The loop body (usually contains a delay).
        body: Box<Statement>,
        /// Source location.
        span: Span,
    },
    /// A no-operation (placeholder for empty branches).
    Nop,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SignalId;
    use aion_common::LogicVec;

    #[test]
    fn assign_statement() {
        let stmt = Statement::Assign {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Literal(LogicVec::all_zero(8)),
            span: Span::DUMMY,
        };
        assert!(matches!(stmt, Statement::Assign { .. }));
    }

    #[test]
    fn if_statement() {
        let stmt = Statement::If {
            condition: Expr::Literal(LogicVec::all_one(1)),
            then_body: Box::new(Statement::Nop),
            else_body: Some(Box::new(Statement::Nop)),
            span: Span::DUMMY,
        };
        if let Statement::If { else_body, .. } = &stmt {
            assert!(else_body.is_some());
        } else {
            panic!("expected If");
        }
    }

    #[test]
    fn case_statement() {
        let arm = CaseArm {
            patterns: vec![Expr::Literal(LogicVec::all_zero(2))],
            body: Statement::Nop,
            span: Span::DUMMY,
        };
        let stmt = Statement::Case {
            subject: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
            arms: vec![arm],
            default: Some(Box::new(Statement::Nop)),
            span: Span::DUMMY,
        };
        if let Statement::Case { arms, default, .. } = &stmt {
            assert_eq!(arms.len(), 1);
            assert!(default.is_some());
        } else {
            panic!("expected Case");
        }
    }

    #[test]
    fn block_statement() {
        let stmt = Statement::Block {
            stmts: vec![Statement::Nop, Statement::Nop],
            span: Span::DUMMY,
        };
        if let Statement::Block { stmts, .. } = &stmt {
            assert_eq!(stmts.len(), 2);
        } else {
            panic!("expected Block");
        }
    }

    #[test]
    fn assertion_kinds_distinct() {
        assert_ne!(AssertionKind::Assert, AssertionKind::Assume);
        assert_ne!(AssertionKind::Assert, AssertionKind::Cover);
        assert_ne!(AssertionKind::Assume, AssertionKind::Cover);
    }

    #[test]
    fn delay_statement() {
        let stmt = Statement::Delay {
            duration_fs: 5_000_000,
            body: Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::all_one(1)),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        if let Statement::Delay {
            duration_fs, body, ..
        } = &stmt
        {
            assert_eq!(*duration_fs, 5_000_000);
            assert!(matches!(**body, Statement::Assign { .. }));
        } else {
            panic!("expected Delay");
        }
    }

    #[test]
    fn forever_statement() {
        let stmt = Statement::Forever {
            body: Box::new(Statement::Delay {
                duration_fs: 5_000_000,
                body: Box::new(Statement::Nop),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        if let Statement::Forever { body, .. } = &stmt {
            assert!(matches!(**body, Statement::Delay { .. }));
        } else {
            panic!("expected Forever");
        }
    }

    #[test]
    fn display_statement() {
        let stmt = Statement::Display {
            format: "value = %d".to_string(),
            args: vec![Expr::Literal(LogicVec::all_zero(8))],
            span: Span::DUMMY,
        };
        if let Statement::Display { format, args, .. } = &stmt {
            assert_eq!(format, "value = %d");
            assert_eq!(args.len(), 1);
        } else {
            panic!("expected Display");
        }
    }
}
