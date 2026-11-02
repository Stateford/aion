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

/// How a procedural [`Statement::Assign`] updates its target.
///
/// HDLs distinguish assignments whose effect is visible to the very next
/// statement from assignments whose effect is deferred until the end of the
/// current time step. Synthesis treats both the same way (the final value of
/// the target at the end of the process is what gets implemented), but the
/// simulator must honour the difference to schedule updates in the correct
/// IEEE 1364/1800 region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssignKind {
    /// An immediate (blocking) assignment: Verilog/SV `=`, SV compound
    /// assignments (`+=`, `++`, ...) and VHDL variable assignment `:=`.
    ///
    /// Later statements in the same process execution read the new value
    /// right away; other processes see it once the update is committed as an
    /// active-region event.
    Blocking,
    /// A deferred (non-blocking) assignment: Verilog/SV `<=` and VHDL signal
    /// assignment `<=` inside a process.
    ///
    /// The right-hand side is evaluated when the statement executes, but the
    /// target is only updated in the non-blocking assignment (NBA) region,
    /// after every active process at the current time has run. Reads in the
    /// same process keep seeing the old value.
    NonBlocking,
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
    /// A procedural assignment (`target = value` / `target <= value` /
    /// `target := value`).
    Assign {
        /// The target signal or signal slice.
        target: SignalRef,
        /// The value expression.
        value: Expr,
        /// Whether the assignment is blocking or non-blocking, which decides
        /// when the simulator makes the new value visible.
        kind: AssignKind,
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
            kind: AssignKind::Blocking,
            span: Span::DUMMY,
        };
        assert!(matches!(
            stmt,
            Statement::Assign {
                kind: AssignKind::Blocking,
                ..
            }
        ));
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
                kind: AssignKind::NonBlocking,
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

    #[test]
    fn assign_kinds_distinct() {
        assert_ne!(AssignKind::Blocking, AssignKind::NonBlocking);
    }

    #[test]
    fn assign_kind_serde_roundtrip() {
        for kind in [AssignKind::Blocking, AssignKind::NonBlocking] {
            let stmt = Statement::Assign {
                target: SignalRef::Slice {
                    signal: SignalId::from_raw(3),
                    high: 7,
                    low: 4,
                },
                value: Expr::Literal(LogicVec::from_u64(0xA, 4)),
                kind,
                span: Span::DUMMY,
            };
            let json = serde_json::to_string(&stmt).unwrap();
            let back: Statement = serde_json::from_str(&json).unwrap();
            match back {
                Statement::Assign {
                    kind: k, target, ..
                } => {
                    assert_eq!(k, kind);
                    assert!(matches!(
                        target,
                        SignalRef::Slice {
                            high: 7,
                            low: 4,
                            ..
                        }
                    ));
                }
                other => panic!("expected Assign, got {other:?}"),
            }
        }
    }

    #[test]
    fn assign_kind_bincode_roundtrip() {
        let stmt = Statement::Block {
            stmts: vec![
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::all_one(1)),
                    kind: AssignKind::Blocking,
                    span: Span::DUMMY,
                },
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::all_zero(1)),
                    kind: AssignKind::NonBlocking,
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let config = bincode::config::standard();
        let bytes = bincode::serde::encode_to_vec(&stmt, config).unwrap();
        let (back, _): (Statement, usize) =
            bincode::serde::decode_from_slice(&bytes, config).unwrap();
        let Statement::Block { stmts, .. } = back else {
            panic!("expected Block");
        };
        let kinds: Vec<AssignKind> = stmts
            .iter()
            .map(|s| match s {
                Statement::Assign { kind, .. } => *kind,
                other => panic!("expected Assign, got {other:?}"),
            })
            .collect();
        assert_eq!(kinds, vec![AssignKind::Blocking, AssignKind::NonBlocking]);
    }
}
