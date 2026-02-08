//! AST statement lowering to IR statements.
//!
//! Converts language-specific AST statements (Verilog, SystemVerilog, VHDL)
//! into the unified [`Statement`](aion_ir::stmt::Statement) representation.
//! Compound assignments and increment/decrement are expanded into plain
//! assignments with the appropriate binary operation.

use aion_common::Interner;
use aion_diagnostics::DiagnosticSink;
use aion_ir::expr::{BinaryOp, Expr as IrExpr};
use aion_ir::ids::TypeId;
use aion_ir::stmt::{CaseArm as IrCaseArm, Statement as IrStmt};
use aion_source::SourceDb;

use crate::const_eval;
use crate::expr::{
    self, lower_sv_expr, lower_sv_to_signal_ref, lower_to_signal_ref, lower_verilog_expr,
    lower_vhdl_expr, lower_vhdl_to_signal_ref, SignalEnv,
};

/// Default timescale: 1 time unit = 1 ns = 1,000,000 fs.
///
/// Applied to delay literal values when no explicit `timescale is specified.
const DEFAULT_TIMESCALE_FS: u64 = 1_000_000;

/// Lowers a Verilog AST statement to an IR statement.
pub fn lower_verilog_stmt(
    stmt: &aion_verilog_parser::ast::Statement,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    use aion_verilog_parser::ast::Statement;
    match stmt {
        Statement::Blocking {
            target,
            value,
            span,
        } => {
            let tgt = lower_to_signal_ref(target, sig_env, interner, sink);
            let val = lower_verilog_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                span: *span,
            }
        }
        Statement::NonBlocking {
            target,
            value,
            span,
        } => {
            let tgt = lower_to_signal_ref(target, sig_env, interner, sink);
            let val = lower_verilog_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                span: *span,
            }
        }
        Statement::Block { stmts, span, .. } => {
            let ir_stmts: Vec<_> = stmts
                .iter()
                .map(|s| lower_verilog_stmt(s, sig_env, source_db, interner, sink))
                .collect();
            IrStmt::Block {
                stmts: ir_stmts,
                span: *span,
            }
        }
        Statement::If {
            condition,
            then_stmt,
            else_stmt,
            span,
        } => {
            let cond = lower_verilog_expr(condition, sig_env, source_db, interner, sink);
            let then_body = lower_verilog_stmt(then_stmt, sig_env, source_db, interner, sink);
            let else_body = else_stmt
                .as_ref()
                .map(|s| Box::new(lower_verilog_stmt(s, sig_env, source_db, interner, sink)));
            IrStmt::If {
                condition: cond,
                then_body: Box::new(then_body),
                else_body,
                span: *span,
            }
        }
        Statement::Case {
            expr, arms, span, ..
        } => {
            let subject = lower_verilog_expr(expr, sig_env, source_db, interner, sink);
            let mut ir_arms = Vec::new();
            let mut default = None;
            for arm in arms {
                if arm.is_default {
                    default = Some(Box::new(lower_verilog_stmt(
                        &arm.body, sig_env, source_db, interner, sink,
                    )));
                } else {
                    let patterns: Vec<_> = arm
                        .patterns
                        .iter()
                        .map(|p| lower_verilog_expr(p, sig_env, source_db, interner, sink))
                        .collect();
                    let body = lower_verilog_stmt(&arm.body, sig_env, source_db, interner, sink);
                    ir_arms.push(IrCaseArm {
                        patterns,
                        body,
                        span: arm.span,
                    });
                }
            }
            IrStmt::Case {
                subject,
                arms: ir_arms,
                default,
                span: *span,
            }
        }
        Statement::For { body, .. } => {
            // For loops in behavioral blocks — lower the body only
            lower_verilog_stmt(body, sig_env, source_db, interner, sink)
        }
        Statement::While { body, .. } => {
            lower_verilog_stmt(body, sig_env, source_db, interner, sink)
        }
        Statement::Forever { body, span } => {
            let ir_body = lower_verilog_stmt(body, sig_env, source_db, interner, sink);
            IrStmt::Forever {
                body: Box::new(ir_body),
                span: *span,
            }
        }
        Statement::Repeat { body, .. } => {
            lower_verilog_stmt(body, sig_env, source_db, interner, sink)
        }
        Statement::Wait { span, .. } => IrStmt::Wait {
            duration: None,
            span: *span,
        },
        Statement::EventControl { body, .. } => {
            // Sensitivity is captured at the Process level; lower the body
            lower_verilog_stmt(body, sig_env, source_db, interner, sink)
        }
        Statement::Delay {
            delay, body, span, ..
        } => {
            let ir_body = lower_verilog_stmt(body, sig_env, source_db, interner, sink);
            // Try to evaluate the delay expression as a compile-time constant
            let duration_fs = eval_delay_expr_verilog(delay, source_db, interner, sink);
            IrStmt::Delay {
                duration_fs,
                body: Box::new(ir_body),
                span: *span,
            }
        }
        Statement::SystemTaskCall {
            name, args, span, ..
        } => {
            let task_name = interner.resolve(*name).to_lowercase();
            match task_name.as_str() {
                "$display" | "$write" | "$strobe" | "$monitor" => {
                    let format = if args.is_empty() {
                        String::new()
                    } else {
                        source_db.snippet(args[0].span()).to_string()
                    };
                    let ir_args: Vec<_> = args
                        .iter()
                        .skip(1)
                        .map(|a| lower_verilog_expr(a, sig_env, source_db, interner, sink))
                        .collect();
                    IrStmt::Display {
                        format,
                        args: ir_args,
                        span: *span,
                    }
                }
                "$finish" | "$stop" => IrStmt::Finish { span: *span },
                _ => IrStmt::Nop,
            }
        }
        Statement::TaskCall { .. } => IrStmt::Nop,
        Statement::Disable { .. } => IrStmt::Nop,
        Statement::Null { .. } => IrStmt::Nop,
        Statement::Error(_) => IrStmt::Nop,
    }
}

/// Lowers a SystemVerilog AST statement to an IR statement.
pub fn lower_sv_stmt(
    stmt: &aion_sv_parser::ast::Statement,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    use aion_sv_parser::ast::Statement;
    match stmt {
        Statement::Blocking {
            target,
            value,
            span,
        } => {
            let tgt = lower_sv_to_signal_ref(target, sig_env, interner, sink);
            let val = lower_sv_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                span: *span,
            }
        }
        Statement::NonBlocking {
            target,
            value,
            span,
        } => {
            let tgt = lower_sv_to_signal_ref(target, sig_env, interner, sink);
            let val = lower_sv_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                span: *span,
            }
        }
        Statement::CompoundAssign {
            target,
            op,
            value,
            span,
        } => {
            // Expand `target op= value` into `target = target op value`
            let tgt = lower_sv_to_signal_ref(target, sig_env, interner, sink);
            let tgt_expr = lower_sv_expr(target, sig_env, source_db, interner, sink);
            let val = lower_sv_expr(value, sig_env, source_db, interner, sink);
            let ir_op = expr::map_sv_compound_op(*op);
            let combined = IrExpr::Binary {
                op: ir_op,
                lhs: Box::new(tgt_expr),
                rhs: Box::new(val),
                ty: TypeId::from_raw(0),
                span: *span,
            };
            IrStmt::Assign {
                target: tgt,
                value: combined,
                span: *span,
            }
        }
        Statement::IncrDecr {
            operand,
            increment,
            span,
            ..
        } => {
            // Expand `i++` / `i--` into `i = i + 1` / `i = i - 1`
            let tgt = lower_sv_to_signal_ref(operand, sig_env, interner, sink);
            let tgt_expr = lower_sv_expr(operand, sig_env, source_db, interner, sink);
            let one = IrExpr::Literal(crate::expr::logic_vec_from_u64(32, 1));
            let op = if *increment {
                BinaryOp::Add
            } else {
                BinaryOp::Sub
            };
            let combined = IrExpr::Binary {
                op,
                lhs: Box::new(tgt_expr),
                rhs: Box::new(one),
                ty: TypeId::from_raw(0),
                span: *span,
            };
            IrStmt::Assign {
                target: tgt,
                value: combined,
                span: *span,
            }
        }
        Statement::Block { stmts, span, .. } => {
            let ir_stmts: Vec<_> = stmts
                .iter()
                .map(|s| lower_sv_stmt(s, sig_env, source_db, interner, sink))
                .collect();
            IrStmt::Block {
                stmts: ir_stmts,
                span: *span,
            }
        }
        Statement::If {
            condition,
            then_stmt,
            else_stmt,
            span,
            ..
        } => {
            let cond = lower_sv_expr(condition, sig_env, source_db, interner, sink);
            let then_body = lower_sv_stmt(then_stmt, sig_env, source_db, interner, sink);
            let else_body = else_stmt
                .as_ref()
                .map(|s| Box::new(lower_sv_stmt(s, sig_env, source_db, interner, sink)));
            IrStmt::If {
                condition: cond,
                then_body: Box::new(then_body),
                else_body,
                span: *span,
            }
        }
        Statement::Case {
            expr, arms, span, ..
        } => {
            let subject = lower_sv_expr(expr, sig_env, source_db, interner, sink);
            let mut ir_arms = Vec::new();
            let mut default = None;
            for arm in arms {
                if arm.is_default {
                    default = Some(Box::new(lower_sv_stmt(
                        &arm.body, sig_env, source_db, interner, sink,
                    )));
                } else {
                    let patterns: Vec<_> = arm
                        .patterns
                        .iter()
                        .map(|p| lower_sv_expr(p, sig_env, source_db, interner, sink))
                        .collect();
                    let body = lower_sv_stmt(&arm.body, sig_env, source_db, interner, sink);
                    ir_arms.push(IrCaseArm {
                        patterns,
                        body,
                        span: arm.span,
                    });
                }
            }
            IrStmt::Case {
                subject,
                arms: ir_arms,
                default,
                span: *span,
            }
        }
        Statement::For { body, .. } => lower_sv_stmt(body, sig_env, source_db, interner, sink),
        Statement::While { body, .. } => lower_sv_stmt(body, sig_env, source_db, interner, sink),
        Statement::DoWhile { body, .. } => lower_sv_stmt(body, sig_env, source_db, interner, sink),
        Statement::Forever { body, span } => {
            let ir_body = lower_sv_stmt(body, sig_env, source_db, interner, sink);
            IrStmt::Forever {
                body: Box::new(ir_body),
                span: *span,
            }
        }
        Statement::Repeat { body, .. } => lower_sv_stmt(body, sig_env, source_db, interner, sink),
        Statement::Foreach { body, .. } => lower_sv_stmt(body, sig_env, source_db, interner, sink),
        Statement::Wait { span, .. } => IrStmt::Wait {
            duration: None,
            span: *span,
        },
        Statement::EventControl { body, .. } => {
            lower_sv_stmt(body, sig_env, source_db, interner, sink)
        }
        Statement::Delay {
            delay, body, span, ..
        } => {
            let ir_body = lower_sv_stmt(body, sig_env, source_db, interner, sink);
            let duration_fs = eval_delay_expr_sv(delay, source_db, interner, sink);
            IrStmt::Delay {
                duration_fs,
                body: Box::new(ir_body),
                span: *span,
            }
        }
        Statement::SystemTaskCall {
            name, args, span, ..
        } => {
            let task_name = interner.resolve(*name).to_lowercase();
            match task_name.as_str() {
                "$display" | "$write" | "$strobe" | "$monitor" => {
                    let format = if args.is_empty() {
                        String::new()
                    } else {
                        source_db.snippet(args[0].span()).to_string()
                    };
                    let ir_args: Vec<_> = args
                        .iter()
                        .skip(1)
                        .map(|a| lower_sv_expr(a, sig_env, source_db, interner, sink))
                        .collect();
                    IrStmt::Display {
                        format,
                        args: ir_args,
                        span: *span,
                    }
                }
                "$finish" | "$stop" => IrStmt::Finish { span: *span },
                _ => IrStmt::Nop,
            }
        }
        Statement::TaskCall { .. } => IrStmt::Nop,
        Statement::Disable { .. } => IrStmt::Nop,
        Statement::Return { .. } => IrStmt::Nop,
        Statement::Break { .. } => IrStmt::Nop,
        Statement::Continue { .. } => IrStmt::Nop,
        Statement::Assertion(_) => IrStmt::Nop,
        Statement::LocalVarDecl(_) => IrStmt::Nop,
        Statement::Null { .. } => IrStmt::Nop,
        Statement::Error(_) => IrStmt::Nop,
    }
}

/// Lowers a VHDL sequential statement to an IR statement.
pub fn lower_vhdl_stmt(
    stmt: &aion_vhdl_parser::ast::SequentialStatement,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    use aion_vhdl_parser::ast::SequentialStatement;
    match stmt {
        SequentialStatement::SignalAssignment {
            target,
            waveforms,
            span,
        } => {
            let tgt = lower_vhdl_to_signal_ref(target, sig_env, interner, sink);
            // Use the first waveform's value (ignore `after` timing)
            let val = if let Some(wf) = waveforms.first() {
                lower_vhdl_expr(&wf.value, sig_env, source_db, interner, sink)
            } else {
                IrExpr::Literal(aion_common::LogicVec::all_zero(1))
            };
            IrStmt::Assign {
                target: tgt,
                value: val,
                span: *span,
            }
        }
        SequentialStatement::VariableAssignment {
            target,
            value,
            span,
        } => {
            let tgt = lower_vhdl_to_signal_ref(target, sig_env, interner, sink);
            let val = lower_vhdl_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                span: *span,
            }
        }
        SequentialStatement::If(if_stmt) => {
            lower_vhdl_if(if_stmt, sig_env, source_db, interner, sink)
        }
        SequentialStatement::Case(case_stmt) => {
            lower_vhdl_case(case_stmt, sig_env, source_db, interner, sink)
        }
        SequentialStatement::ForLoop(for_loop) => {
            // Lower the body statements as a block
            let ir_stmts: Vec<_> = for_loop
                .stmts
                .iter()
                .map(|s| lower_vhdl_stmt(s, sig_env, source_db, interner, sink))
                .collect();
            IrStmt::Block {
                stmts: ir_stmts,
                span: for_loop.span,
            }
        }
        SequentialStatement::WhileLoop(wl) => {
            let ir_stmts: Vec<_> = wl
                .stmts
                .iter()
                .map(|s| lower_vhdl_stmt(s, sig_env, source_db, interner, sink))
                .collect();
            IrStmt::Block {
                stmts: ir_stmts,
                span: wl.span,
            }
        }
        SequentialStatement::Loop(lp) => {
            let ir_stmts: Vec<_> = lp
                .stmts
                .iter()
                .map(|s| lower_vhdl_stmt(s, sig_env, source_db, interner, sink))
                .collect();
            IrStmt::Block {
                stmts: ir_stmts,
                span: lp.span,
            }
        }
        SequentialStatement::Assert {
            condition, span, ..
        } => IrStmt::Assertion {
            kind: aion_ir::stmt::AssertionKind::Assert,
            condition: lower_vhdl_expr(condition, sig_env, source_db, interner, sink),
            message: None,
            span: *span,
        },
        SequentialStatement::Report { message, span, .. } => {
            let msg_text = source_db.snippet(message.span()).to_string();
            IrStmt::Display {
                format: msg_text,
                args: vec![],
                span: *span,
            }
        }
        SequentialStatement::Wait(w) => IrStmt::Wait {
            duration: None,
            span: w.span,
        },
        SequentialStatement::Next { .. }
        | SequentialStatement::Exit { .. }
        | SequentialStatement::Return { .. }
        | SequentialStatement::ProcedureCall { .. } => IrStmt::Nop,
        SequentialStatement::Null { .. } => IrStmt::Nop,
        SequentialStatement::Error(_) => IrStmt::Nop,
    }
}

/// Lowers a VHDL `if` statement including `elsif` branches to nested IR `If` statements.
fn lower_vhdl_if(
    if_stmt: &aion_vhdl_parser::ast::IfStatement,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    let cond = lower_vhdl_expr(&if_stmt.condition, sig_env, source_db, interner, sink);
    let then_body = lower_vhdl_stmt_list(
        &if_stmt.then_stmts,
        sig_env,
        source_db,
        interner,
        sink,
        if_stmt.span,
    );

    // Build else from elsif chain, starting from the end
    let else_body = if !if_stmt.elsif_branches.is_empty() {
        let mut else_part: Option<Box<IrStmt>> = if if_stmt.else_stmts.is_empty() {
            None
        } else {
            Some(Box::new(lower_vhdl_stmt_list(
                &if_stmt.else_stmts,
                sig_env,
                source_db,
                interner,
                sink,
                if_stmt.span,
            )))
        };

        // Process elsif branches in reverse order to build nested if-else
        for branch in if_stmt.elsif_branches.iter().rev() {
            let elsif_cond = lower_vhdl_expr(&branch.condition, sig_env, source_db, interner, sink);
            let elsif_body = lower_vhdl_stmt_list(
                &branch.stmts,
                sig_env,
                source_db,
                interner,
                sink,
                branch.span,
            );
            else_part = Some(Box::new(IrStmt::If {
                condition: elsif_cond,
                then_body: Box::new(elsif_body),
                else_body: else_part,
                span: branch.span,
            }));
        }
        else_part
    } else if if_stmt.else_stmts.is_empty() {
        None
    } else {
        Some(Box::new(lower_vhdl_stmt_list(
            &if_stmt.else_stmts,
            sig_env,
            source_db,
            interner,
            sink,
            if_stmt.span,
        )))
    };

    IrStmt::If {
        condition: cond,
        then_body: Box::new(then_body),
        else_body,
        span: if_stmt.span,
    }
}

/// Lowers a VHDL `case` statement to an IR `Case` statement.
fn lower_vhdl_case(
    case_stmt: &aion_vhdl_parser::ast::CaseStatement,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    let subject = lower_vhdl_expr(&case_stmt.expr, sig_env, source_db, interner, sink);
    let mut ir_arms = Vec::new();
    let mut default = None;

    for alt in &case_stmt.alternatives {
        let is_others = alt
            .choices
            .iter()
            .any(|c| matches!(c, aion_vhdl_parser::ast::Choice::Others(_)));

        if is_others {
            let body =
                lower_vhdl_stmt_list(&alt.stmts, sig_env, source_db, interner, sink, alt.span);
            default = Some(Box::new(body));
        } else {
            let patterns: Vec<_> = alt
                .choices
                .iter()
                .filter_map(|c| match c {
                    aion_vhdl_parser::ast::Choice::Expr(e) => {
                        Some(lower_vhdl_expr(e, sig_env, source_db, interner, sink))
                    }
                    _ => None,
                })
                .collect();
            let body =
                lower_vhdl_stmt_list(&alt.stmts, sig_env, source_db, interner, sink, alt.span);
            ir_arms.push(IrCaseArm {
                patterns,
                body,
                span: alt.span,
            });
        }
    }

    IrStmt::Case {
        subject,
        arms: ir_arms,
        default,
        span: case_stmt.span,
    }
}

/// Lowers a list of VHDL sequential statements to an IR block or single statement.
fn lower_vhdl_stmt_list(
    stmts: &[aion_vhdl_parser::ast::SequentialStatement],
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
    span: aion_source::Span,
) -> IrStmt {
    let ir_stmts: Vec<_> = stmts
        .iter()
        .map(|s| lower_vhdl_stmt(s, sig_env, source_db, interner, sink))
        .collect();
    if ir_stmts.len() == 1 {
        ir_stmts.into_iter().next().unwrap()
    } else {
        IrStmt::Block {
            stmts: ir_stmts,
            span,
        }
    }
}

/// Evaluates a Verilog delay expression to femtoseconds.
///
/// Tries to const-evaluate the expression; if it resolves to an integer,
/// multiplies by `DEFAULT_TIMESCALE_FS` (1 ns). Falls back to 0 fs if
/// the expression cannot be evaluated.
fn eval_delay_expr_verilog(
    expr: &aion_verilog_parser::ast::Expr,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> u64 {
    let env = crate::const_eval::ConstEnv::default();
    if let Some(val) = const_eval::eval_verilog_expr(expr, source_db, interner, &env, sink) {
        if let Some(v) = const_eval::const_to_i64(&val) {
            return (v.unsigned_abs()) * DEFAULT_TIMESCALE_FS;
        }
    }
    0
}

/// Evaluates a SystemVerilog delay expression to femtoseconds.
///
/// Same approach as [`eval_delay_expr_verilog`] but uses the SV const evaluator.
fn eval_delay_expr_sv(
    expr: &aion_sv_parser::ast::Expr,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> u64 {
    let env = crate::const_eval::ConstEnv::default();
    if let Some(val) = const_eval::eval_sv_expr(expr, source_db, interner, &env, sink) {
        if let Some(v) = const_eval::const_to_i64(&val) {
            return (v.unsigned_abs()) * DEFAULT_TIMESCALE_FS;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_ir::ids::SignalId;
    use aion_source::{SourceDb, Span};

    fn setup() -> (SourceDb, Interner, DiagnosticSink, SignalEnv) {
        (
            SourceDb::new(),
            Interner::new(),
            DiagnosticSink::new(),
            SignalEnv::new(),
        )
    }

    #[test]
    fn verilog_blocking_assign() {
        let (sdb, interner, sink, mut env) = setup();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        env.insert(a, SignalId::from_raw(0));
        env.insert(b, SignalId::from_raw(1));

        let stmt = aion_verilog_parser::ast::Statement::Blocking {
            target: aion_verilog_parser::ast::Expr::Identifier {
                name: a,
                span: Span::DUMMY,
            },
            value: aion_verilog_parser::ast::Expr::Identifier {
                name: b,
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let ir = lower_verilog_stmt(&stmt, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrStmt::Assign { .. }));
    }

    #[test]
    fn verilog_if_stmt() {
        let (sdb, interner, sink, mut env) = setup();
        let c = interner.get_or_intern("c");
        env.insert(c, SignalId::from_raw(0));

        let stmt = aion_verilog_parser::ast::Statement::If {
            condition: aion_verilog_parser::ast::Expr::Identifier {
                name: c,
                span: Span::DUMMY,
            },
            then_stmt: Box::new(aion_verilog_parser::ast::Statement::Null { span: Span::DUMMY }),
            else_stmt: None,
            span: Span::DUMMY,
        };
        let ir = lower_verilog_stmt(&stmt, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrStmt::If { .. }));
    }

    #[test]
    fn verilog_case_stmt() {
        let (sdb, interner, sink, mut env) = setup();
        let sel = interner.get_or_intern("sel");
        env.insert(sel, SignalId::from_raw(0));

        let stmt = aion_verilog_parser::ast::Statement::Case {
            kind: aion_verilog_parser::ast::CaseKind::Case,
            expr: aion_verilog_parser::ast::Expr::Identifier {
                name: sel,
                span: Span::DUMMY,
            },
            arms: vec![aion_verilog_parser::ast::CaseArm {
                patterns: vec![],
                is_default: true,
                body: aion_verilog_parser::ast::Statement::Null { span: Span::DUMMY },
                span: Span::DUMMY,
            }],
            span: Span::DUMMY,
        };
        let ir = lower_verilog_stmt(&stmt, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrStmt::Case { .. }));
    }

    #[test]
    fn sv_compound_assign_expands() {
        let (sdb, interner, sink, mut env) = setup();
        let x = interner.get_or_intern("x");
        env.insert(x, SignalId::from_raw(0));

        let stmt = aion_sv_parser::ast::Statement::CompoundAssign {
            target: aion_sv_parser::ast::Expr::Identifier {
                name: x,
                span: Span::DUMMY,
            },
            op: aion_sv_parser::ast::CompoundOp::Add,
            value: aion_sv_parser::ast::Expr::Identifier {
                name: x,
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let ir = lower_sv_stmt(&stmt, &env, &sdb, &interner, &sink);
        if let IrStmt::Assign { value, .. } = &ir {
            assert!(matches!(
                value,
                IrExpr::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            ));
        } else {
            panic!("expected Assign");
        }
    }

    #[test]
    fn sv_incr_expands() {
        let (sdb, interner, sink, mut env) = setup();
        let i = interner.get_or_intern("i");
        env.insert(i, SignalId::from_raw(0));

        let stmt = aion_sv_parser::ast::Statement::IncrDecr {
            operand: aion_sv_parser::ast::Expr::Identifier {
                name: i,
                span: Span::DUMMY,
            },
            increment: true,
            prefix: false,
            span: Span::DUMMY,
        };
        let ir = lower_sv_stmt(&stmt, &env, &sdb, &interner, &sink);
        if let IrStmt::Assign { value, .. } = &ir {
            assert!(matches!(
                value,
                IrExpr::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            ));
        } else {
            panic!("expected Assign");
        }
    }

    #[test]
    fn vhdl_signal_assignment() {
        let (sdb, interner, sink, mut env) = setup();
        let q = interner.get_or_intern("q");
        let d = interner.get_or_intern("d");
        env.insert(q, SignalId::from_raw(0));
        env.insert(d, SignalId::from_raw(1));

        let stmt = aion_vhdl_parser::ast::SequentialStatement::SignalAssignment {
            target: aion_vhdl_parser::ast::Expr::Name(aion_vhdl_parser::ast::Name {
                primary: q,
                parts: vec![],
                span: Span::DUMMY,
            }),
            waveforms: vec![aion_vhdl_parser::ast::Waveform {
                value: aion_vhdl_parser::ast::Expr::Name(aion_vhdl_parser::ast::Name {
                    primary: d,
                    parts: vec![],
                    span: Span::DUMMY,
                }),
                after: None,
                span: Span::DUMMY,
            }],
            span: Span::DUMMY,
        };
        let ir = lower_vhdl_stmt(&stmt, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrStmt::Assign { .. }));
    }

    #[test]
    fn verilog_event_control_passes_through() {
        let (sdb, interner, sink, env) = setup();
        let stmt = aion_verilog_parser::ast::Statement::EventControl {
            sensitivity: aion_verilog_parser::ast::SensitivityList::Star,
            body: Box::new(aion_verilog_parser::ast::Statement::Null { span: Span::DUMMY }),
            span: Span::DUMMY,
        };
        let ir = lower_verilog_stmt(&stmt, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrStmt::Nop));
    }

    #[test]
    fn verilog_block_stmt() {
        let (sdb, interner, sink, env) = setup();
        let stmt = aion_verilog_parser::ast::Statement::Block {
            label: None,
            decls: vec![],
            stmts: vec![
                aion_verilog_parser::ast::Statement::Null { span: Span::DUMMY },
                aion_verilog_parser::ast::Statement::Null { span: Span::DUMMY },
            ],
            span: Span::DUMMY,
        };
        let ir = lower_verilog_stmt(&stmt, &env, &sdb, &interner, &sink);
        if let IrStmt::Block { stmts, .. } = &ir {
            assert_eq!(stmts.len(), 2);
        } else {
            panic!("expected Block");
        }
    }

    #[test]
    fn verilog_delay_preserved() {
        let mut sdb = SourceDb::new();
        let file_id = sdb.add_source("test.v", "5".into());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let env = SignalEnv::new();
        // Span covering "5" in the source
        let lit_span = aion_source::Span {
            file: file_id,
            start: 0,
            end: 1,
        };
        let stmt = aion_verilog_parser::ast::Statement::Delay {
            delay: aion_verilog_parser::ast::Expr::Literal { span: lit_span },
            body: Box::new(aion_verilog_parser::ast::Statement::Null { span: Span::DUMMY }),
            span: Span::DUMMY,
        };
        let ir = lower_verilog_stmt(&stmt, &env, &sdb, &interner, &sink);
        if let IrStmt::Delay {
            duration_fs, body, ..
        } = &ir
        {
            // 5 * 1_000_000 fs = 5_000_000 fs (5 ns)
            assert_eq!(*duration_fs, 5_000_000);
            assert!(matches!(**body, IrStmt::Nop));
        } else {
            panic!("expected Delay, got {:?}", ir);
        }
    }

    #[test]
    fn verilog_forever_preserved() {
        let (sdb, interner, sink, env) = setup();
        let stmt = aion_verilog_parser::ast::Statement::Forever {
            body: Box::new(aion_verilog_parser::ast::Statement::Null { span: Span::DUMMY }),
            span: Span::DUMMY,
        };
        let ir = lower_verilog_stmt(&stmt, &env, &sdb, &interner, &sink);
        if let IrStmt::Forever { body, .. } = &ir {
            assert!(matches!(**body, IrStmt::Nop));
        } else {
            panic!("expected Forever, got {:?}", ir);
        }
    }

    #[test]
    fn sv_delay_preserved() {
        let mut sdb = SourceDb::new();
        let file_id = sdb.add_source("test.sv", "20".into());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let env = SignalEnv::new();
        let lit_span = aion_source::Span {
            file: file_id,
            start: 0,
            end: 2,
        };
        let stmt = aion_sv_parser::ast::Statement::Delay {
            delay: aion_sv_parser::ast::Expr::Literal { span: lit_span },
            body: Box::new(aion_sv_parser::ast::Statement::Null { span: Span::DUMMY }),
            span: Span::DUMMY,
        };
        let ir = lower_sv_stmt(&stmt, &env, &sdb, &interner, &sink);
        if let IrStmt::Delay {
            duration_fs, body, ..
        } = &ir
        {
            // 20 * 1_000_000 fs = 20_000_000 fs (20 ns)
            assert_eq!(*duration_fs, 20_000_000);
            assert!(matches!(**body, IrStmt::Nop));
        } else {
            panic!("expected Delay, got {:?}", ir);
        }
    }

    #[test]
    fn sv_forever_preserved() {
        let (sdb, interner, sink, env) = setup();
        let stmt = aion_sv_parser::ast::Statement::Forever {
            body: Box::new(aion_sv_parser::ast::Statement::Null { span: Span::DUMMY }),
            span: Span::DUMMY,
        };
        let ir = lower_sv_stmt(&stmt, &env, &sdb, &interner, &sink);
        if let IrStmt::Forever { body, .. } = &ir {
            assert!(matches!(**body, IrStmt::Nop));
        } else {
            panic!("expected Forever, got {:?}", ir);
        }
    }
}
