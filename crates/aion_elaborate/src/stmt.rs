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
use aion_ir::signal::SignalRef;
use aion_ir::stmt::{AssignKind, CaseArm as IrCaseArm, Statement as IrStmt};
use aion_source::SourceDb;

use crate::const_eval;
use crate::errors;
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
            let tgt = lower_to_signal_ref(target, sig_env, source_db, interner, sink);
            let val = lower_verilog_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                kind: AssignKind::Blocking,
                span: *span,
            }
        }
        Statement::NonBlocking {
            target,
            value,
            span,
        } => {
            let tgt = lower_to_signal_ref(target, sig_env, source_db, interner, sink);
            let val = lower_verilog_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                kind: AssignKind::NonBlocking,
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
            if !verilog_may_yield(body, interner) {
                sink.emit(errors::warn_forever_without_timing(*span));
            }
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

/// Lowers the body of a Verilog `always` block that has no top-level event
/// control (e.g. `always #5 clk = ~clk;`).
///
/// Such a block restarts as soon as its body ends, so it is lowered as a
/// `forever` loop spanning the whole `always` block; the simulator starts it
/// once at time 0 and it then advances only through its own delays. A body
/// that can never yield gets W203 (the simulator rejects it at run time with
/// a zero-time-loop error instead of hanging).
pub fn lower_verilog_looping_always(
    body: &aion_verilog_parser::ast::Statement,
    always_span: aion_source::Span,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    if !verilog_may_yield(body, interner) {
        sink.emit(errors::warn_always_without_timing(always_span));
    }
    IrStmt::Forever {
        body: Box::new(lower_verilog_stmt(body, sig_env, source_db, interner, sink)),
        span: always_span,
    }
}

/// SystemVerilog counterpart of [`lower_verilog_looping_always`] for a plain
/// `always` block without a top-level event control.
pub fn lower_sv_looping_always(
    body: &aion_sv_parser::ast::Statement,
    always_span: aion_source::Span,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    if !sv_may_yield(body, interner) {
        sink.emit(errors::warn_always_without_timing(always_span));
    }
    IrStmt::Forever {
        body: Box::new(lower_sv_stmt(body, sig_env, source_db, interner, sink)),
        span: always_span,
    }
}

/// Returns `true` if a system task name ends the simulation (`$finish`/`$stop`).
fn is_finish_task(name: aion_common::Ident, interner: &Interner) -> bool {
    matches!(
        interner.resolve(name).to_lowercase().as_str(),
        "$finish" | "$stop"
    )
}

/// Returns `true` if a Verilog statement can suspend or leave an enclosing
/// `forever` loop.
///
/// Used to flag `forever` loops that can only ever spin at one simulation
/// time (W202). The check is deliberately conservative so it never fires on a
/// loop that might be fine: delays, event controls, `wait`, `disable`,
/// `$finish`/`$stop`, user task calls (which may contain delays) and error
/// nodes all count as "may yield". It runs on the AST because lowering drops
/// nested event controls, so the IR alone cannot tell `forever @(posedge clk)`
/// apart from a loop with no timing control.
fn verilog_may_yield(stmt: &aion_verilog_parser::ast::Statement, interner: &Interner) -> bool {
    use aion_verilog_parser::ast::Statement;
    match stmt {
        Statement::Delay { .. }
        | Statement::EventControl { .. }
        | Statement::Wait { .. }
        | Statement::Disable { .. }
        | Statement::TaskCall { .. }
        | Statement::Error(_) => true,
        Statement::SystemTaskCall { name, .. } => is_finish_task(*name, interner),
        Statement::Block { stmts, .. } => stmts.iter().any(|s| verilog_may_yield(s, interner)),
        Statement::If {
            then_stmt,
            else_stmt,
            ..
        } => {
            verilog_may_yield(then_stmt, interner)
                || else_stmt
                    .as_deref()
                    .is_some_and(|s| verilog_may_yield(s, interner))
        }
        Statement::Case { arms, .. } => arms.iter().any(|a| verilog_may_yield(&a.body, interner)),
        Statement::For {
            init, step, body, ..
        } => {
            verilog_may_yield(init, interner)
                || verilog_may_yield(step, interner)
                || verilog_may_yield(body, interner)
        }
        Statement::While { body, .. }
        | Statement::Forever { body, .. }
        | Statement::Repeat { body, .. } => verilog_may_yield(body, interner),
        Statement::Blocking { .. } | Statement::NonBlocking { .. } | Statement::Null { .. } => {
            false
        }
    }
}

/// SystemVerilog counterpart of [`verilog_may_yield`].
///
/// Additionally treats `break` and `return` as leaving the loop.
fn sv_may_yield(stmt: &aion_sv_parser::ast::Statement, interner: &Interner) -> bool {
    use aion_sv_parser::ast::Statement;
    match stmt {
        Statement::Delay { .. }
        | Statement::EventControl { .. }
        | Statement::Wait { .. }
        | Statement::Disable { .. }
        | Statement::TaskCall { .. }
        | Statement::Break { .. }
        | Statement::Return { .. }
        | Statement::Error(_) => true,
        Statement::SystemTaskCall { name, .. } => is_finish_task(*name, interner),
        Statement::Block { stmts, .. } => stmts.iter().any(|s| sv_may_yield(s, interner)),
        Statement::If {
            then_stmt,
            else_stmt,
            ..
        } => {
            sv_may_yield(then_stmt, interner)
                || else_stmt
                    .as_deref()
                    .is_some_and(|s| sv_may_yield(s, interner))
        }
        Statement::Case { arms, .. } => arms.iter().any(|a| sv_may_yield(&a.body, interner)),
        Statement::For {
            init, step, body, ..
        } => {
            sv_may_yield(init, interner)
                || sv_may_yield(step, interner)
                || sv_may_yield(body, interner)
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::Forever { body, .. }
        | Statement::Repeat { body, .. }
        | Statement::Foreach { body, .. } => sv_may_yield(body, interner),
        Statement::Blocking { .. }
        | Statement::NonBlocking { .. }
        | Statement::CompoundAssign { .. }
        | Statement::IncrDecr { .. }
        | Statement::Continue { .. }
        | Statement::Assertion(_)
        | Statement::LocalVarDecl(_)
        | Statement::Null { .. } => false,
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
            let tgt = lower_sv_to_signal_ref(target, sig_env, source_db, interner, sink);
            let val = lower_sv_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                kind: AssignKind::Blocking,
                span: *span,
            }
        }
        Statement::NonBlocking {
            target,
            value,
            span,
        } => {
            let tgt = lower_sv_to_signal_ref(target, sig_env, source_db, interner, sink);
            let val = lower_sv_expr(value, sig_env, source_db, interner, sink);
            IrStmt::Assign {
                target: tgt,
                value: val,
                kind: AssignKind::NonBlocking,
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
            let tgt = lower_sv_to_signal_ref(target, sig_env, source_db, interner, sink);
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
                kind: AssignKind::Blocking,
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
            let tgt = lower_sv_to_signal_ref(operand, sig_env, source_db, interner, sink);
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
                kind: AssignKind::Blocking,
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
            if !sv_may_yield(body, interner) {
                sink.emit(errors::warn_forever_without_timing(*span));
            }
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
            // The delay mechanism, like `after`, only shapes simulation
            // timing; the IR models zero-delay assignment (see
            // `first_waveform_value`).
            delay: _,
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
                kind: AssignKind::NonBlocking,
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
                kind: AssignKind::Blocking,
                span: *span,
            }
        }
        SequentialStatement::ConditionalSignalAssignment(ca) => {
            lower_vhdl_conditional_signal_assignment(ca, sig_env, source_db, interner, sink)
        }
        SequentialStatement::SelectedSignalAssignment(sa) => {
            lower_vhdl_selected_signal_assignment(sa, sig_env, source_db, interner, sink)
        }
        SequentialStatement::ConditionalVariableAssignment(cv) => {
            let tgt = lower_vhdl_to_signal_ref(&cv.target, sig_env, interner, sink);
            let alts = cv
                .alternatives
                .iter()
                .map(|a| (a.condition.as_ref(), Some(&a.value), a.span));
            let lower = VhdlLowering {
                sig_env,
                source_db,
                interner,
                sink,
            };
            lower.if_chain(&tgt, AssignKind::Blocking, alts)
        }
        SequentialStatement::SelectedVariableAssignment(sv) => {
            let tgt = lower_vhdl_to_signal_ref(&sv.target, sig_env, interner, sink);
            let alts = sv
                .alternatives
                .iter()
                .map(|a| (a.choices.as_slice(), Some(&a.value), a.span));
            let lower = VhdlLowering {
                sig_env,
                source_db,
                interner,
                sink,
            };
            lower.selected_case(
                &sv.selector,
                sv.matching,
                &tgt,
                AssignKind::Blocking,
                alts,
                sv.span,
            )
        }
        SequentialStatement::ForceAssignment(fa) => {
            sink.emit(errors::error_unsupported(
                "VHDL-2008 `force` assignment (simulation override)",
                fa.span,
            ));
            IrStmt::Nop
        }
        SequentialStatement::SelectedForceAssignment(fa) => {
            sink.emit(errors::error_unsupported(
                "VHDL-2008 selected `force` assignment (simulation override)",
                fa.span,
            ));
            IrStmt::Nop
        }
        SequentialStatement::ReleaseAssignment(ra) => {
            sink.emit(errors::error_unsupported(
                "VHDL-2008 `release` assignment (simulation override)",
                ra.span,
            ));
            IrStmt::Nop
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

/// Returns the value of the first element of a VHDL waveform, or `None` for
/// `unaffected`.
///
/// As for simple signal assignments, only the first element's value is
/// used. `after` clauses and delay mechanisms (`transport`,
/// `[reject t] inertial`) only shape the simulation timing of the projected
/// waveform; the IR models zero-delay assignments and synthesis ignores
/// delays, so they are dropped here deliberately.
pub fn first_waveform_value(
    waveform: &aion_vhdl_parser::ast::SignalWaveform,
) -> Option<&aion_vhdl_parser::ast::Expr> {
    match waveform {
        aion_vhdl_parser::ast::SignalWaveform::Elements(elems) => elems.first().map(|w| &w.value),
        aion_vhdl_parser::ast::SignalWaveform::Unaffected(_) => None,
    }
}

/// Lowers a VHDL conditional signal assignment
/// (`t <= a when c1 else b when c2 else c;`) to a priority chain of IR `If`
/// statements with non-blocking assignments.
///
/// Alternatives without a final `else`, and `unaffected` alternatives, leave
/// the target untouched in that branch (an empty branch), which matches the
/// VHDL semantics of "no new transaction". Used both for the VHDL-2008
/// sequential form and as the body of the process equivalent to the
/// concurrent form.
pub fn lower_vhdl_conditional_signal_assignment(
    ca: &aion_vhdl_parser::ast::ConditionalSignalAssignment,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    let tgt = lower_vhdl_to_signal_ref(&ca.target, sig_env, interner, sink);
    let alts = ca.alternatives.iter().map(|a| {
        (
            a.condition.as_ref(),
            first_waveform_value(&a.waveform),
            a.span,
        )
    });
    let lower = VhdlLowering {
        sig_env,
        source_db,
        interner,
        sink,
    };
    lower.if_chain(&tgt, AssignKind::NonBlocking, alts)
}

/// Lowers a VHDL selected signal assignment
/// (`with s select t <= a when c1, b when others;`) to an IR `Case`
/// statement with non-blocking assignments; `others` becomes the default arm.
///
/// Range choices and `select?` choices containing `'-'` don't-cares cannot
/// be expressed as IR case patterns yet and are reported as unsupported
/// (E210) instead of being silently dropped.
pub fn lower_vhdl_selected_signal_assignment(
    sa: &aion_vhdl_parser::ast::SelectedSignalAssignment,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrStmt {
    let tgt = lower_vhdl_to_signal_ref(&sa.target, sig_env, interner, sink);
    let alts = sa.alternatives.iter().map(|a| {
        (
            a.choices.as_slice(),
            first_waveform_value(&a.waveform),
            a.span,
        )
    });
    let lower = VhdlLowering {
        sig_env,
        source_db,
        interner,
        sink,
    };
    lower.selected_case(
        &sa.selector,
        sa.matching,
        &tgt,
        AssignKind::NonBlocking,
        alts,
        sa.span,
    )
}

/// Lowers the alternatives of a VHDL conditional signal assignment to a
/// nested IR ternary (mux) expression `c1 ? a : (c2 ? b : c)`.
///
/// Returns `None` when the assignment is not a pure mux: when the last
/// alternative still has a condition (the target must hold its value, so a
/// latch-style `If` chain is needed) or when any alternative is
/// `unaffected`. Nothing is lowered (and no diagnostic emitted) in that case.
pub fn lower_vhdl_conditional_value(
    alternatives: &[aion_vhdl_parser::ast::ConditionalWaveform],
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> Option<IrExpr> {
    let (last, rest) = alternatives.split_last()?;
    if last.condition.is_some() {
        return None;
    }
    let values: Vec<_> = alternatives
        .iter()
        .map(|a| first_waveform_value(&a.waveform))
        .collect::<Option<_>>()?;

    let mut acc = lower_vhdl_expr(values[rest.len()], sig_env, source_db, interner, sink);
    for (alt, value) in rest.iter().zip(&values).rev() {
        let cond = alt.condition.as_ref()?;
        acc = IrExpr::Ternary {
            condition: Box::new(lower_vhdl_expr(cond, sig_env, source_db, interner, sink)),
            true_val: Box::new(lower_vhdl_expr(value, sig_env, source_db, interner, sink)),
            false_val: Box::new(acc),
            ty: TypeId::from_raw(0),
            span: alt.span,
        };
    }
    Some(acc)
}

/// Bundles the lowering context for the VHDL conditional/selected
/// assignment helpers.
struct VhdlLowering<'a> {
    sig_env: &'a SignalEnv,
    source_db: &'a SourceDb,
    interner: &'a Interner,
    sink: &'a DiagnosticSink,
}

impl VhdlLowering<'_> {
    /// Lowers a VHDL expression in this context.
    fn expr(&self, e: &aion_vhdl_parser::ast::Expr) -> IrExpr {
        lower_vhdl_expr(e, self.sig_env, self.source_db, self.interner, self.sink)
    }

    /// Builds `target <= value` (or `:=`), or `Nop` for `unaffected`.
    fn assign(
        &self,
        target: &SignalRef,
        kind: AssignKind,
        value: Option<&aion_vhdl_parser::ast::Expr>,
        span: aion_source::Span,
    ) -> IrStmt {
        match value {
            Some(v) => IrStmt::Assign {
                target: target.clone(),
                value: self.expr(v),
                kind,
                span,
            },
            None => IrStmt::Nop,
        }
    }

    /// Builds a priority `If` chain from `(condition, value, span)`
    /// alternatives, where a `None` condition is the trailing `else` and a
    /// `None` value is `unaffected`.
    fn if_chain<'e>(
        &self,
        target: &SignalRef,
        kind: AssignKind,
        alternatives: impl DoubleEndedIterator<
            Item = (
                Option<&'e aion_vhdl_parser::ast::Expr>,
                Option<&'e aion_vhdl_parser::ast::Expr>,
                aion_source::Span,
            ),
        >,
    ) -> IrStmt {
        let mut else_part: Option<Box<IrStmt>> = None;
        for (condition, value, span) in alternatives.rev() {
            let body = self.assign(target, kind, value, span);
            else_part = Some(Box::new(match condition {
                // The parser only produces an unconditional alternative last.
                None => body,
                Some(c) => IrStmt::If {
                    condition: self.expr(c),
                    then_body: Box::new(body),
                    else_body: else_part,
                    span,
                },
            }));
        }
        else_part.map_or(IrStmt::Nop, |s| *s)
    }

    /// Builds an IR `Case` over `selector` from `(choices, value, span)`
    /// alternatives; an alternative containing `others` becomes the default.
    fn selected_case<'e>(
        &self,
        selector: &aion_vhdl_parser::ast::Expr,
        matching: bool,
        target: &SignalRef,
        kind: AssignKind,
        alternatives: impl Iterator<
            Item = (
                &'e [aion_vhdl_parser::ast::Choice],
                Option<&'e aion_vhdl_parser::ast::Expr>,
                aion_source::Span,
            ),
        >,
        span: aion_source::Span,
    ) -> IrStmt {
        use aion_vhdl_parser::ast::Choice;
        let subject = self.expr(selector);
        let mut arms = Vec::new();
        let mut default = None;
        for (choices, value, alt_span) in alternatives {
            let body = self.assign(target, kind, value, alt_span);
            if choices.iter().any(|c| matches!(c, Choice::Others(_))) {
                default = Some(Box::new(body));
                continue;
            }
            let mut patterns = Vec::new();
            for choice in choices {
                match choice {
                    Choice::Expr(e) if matching && vhdl_has_dont_care(e, self.source_db) => {
                        self.sink.emit(errors::error_unsupported(
                            "`select?` choice with '-' don't-care bits",
                            e.span(),
                        ));
                    }
                    Choice::Expr(e) => patterns.push(self.expr(e)),
                    Choice::Range(r) => {
                        self.sink.emit(errors::error_unsupported(
                            "range choice in a selected assignment",
                            r.span,
                        ));
                    }
                    Choice::Others(_) => {}
                }
            }
            if !patterns.is_empty() {
                arms.push(IrCaseArm {
                    patterns,
                    body,
                    span: alt_span,
                });
            }
        }
        IrStmt::Case {
            subject,
            arms,
            default,
            span,
        }
    }
}

/// Returns `true` if a VHDL choice expression contains a character, string
/// or bit-string literal with a `'-'` (don't-care) element, which only has
/// matching semantics under `select?` / `case?`.
fn vhdl_has_dont_care(expr: &aion_vhdl_parser::ast::Expr, source_db: &SourceDb) -> bool {
    use aion_vhdl_parser::ast::Expr;
    match expr {
        Expr::CharLiteral { span }
        | Expr::StringLiteral { span }
        | Expr::BitStringLiteral { span } => source_db.snippet(*span).contains('-'),
        Expr::Binary { left, right, .. } => {
            vhdl_has_dont_care(left, source_db) || vhdl_has_dont_care(right, source_db)
        }
        Expr::Paren { inner, .. } | Expr::Qualified { expr: inner, .. } => {
            vhdl_has_dont_care(inner, source_db)
        }
        Expr::Aggregate { elements, .. } => elements
            .iter()
            .any(|e| vhdl_has_dont_care(&e.value, source_db)),
        _ => false,
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
        assert!(matches!(
            ir,
            IrStmt::Assign {
                kind: AssignKind::Blocking,
                ..
            }
        ));
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
        if let IrStmt::Assign { value, kind, .. } = &ir {
            assert_eq!(*kind, AssignKind::Blocking);
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
        if let IrStmt::Assign { value, kind, .. } = &ir {
            assert_eq!(*kind, AssignKind::Blocking);
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
            delay: None,
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
        assert!(matches!(
            ir,
            IrStmt::Assign {
                kind: AssignKind::NonBlocking,
                ..
            }
        ));
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
        // `forever ;` has no timing control → W202.
        let diags = sink.diagnostics();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, crate::errors::W202);
    }

    #[test]
    fn verilog_may_yield_conservative_cases() {
        use aion_verilog_parser::ast::{Expr, Statement};
        let interner = Interner::new();
        let null = Statement::Null { span: Span::DUMMY };
        assert!(!verilog_may_yield(&null, &interner));
        // User task calls may contain delays; error nodes must not cascade.
        let task = Statement::TaskCall {
            name: Expr::Identifier {
                name: interner.get_or_intern("my_task"),
                span: Span::DUMMY,
            },
            args: vec![],
            span: Span::DUMMY,
        };
        assert!(verilog_may_yield(&task, &interner));
        assert!(verilog_may_yield(&Statement::Error(Span::DUMMY), &interner));
        let display = Statement::SystemTaskCall {
            name: interner.get_or_intern("$display"),
            args: vec![],
            span: Span::DUMMY,
        };
        assert!(!verilog_may_yield(&display, &interner));
        let stop = Statement::SystemTaskCall {
            name: interner.get_or_intern("$STOP"),
            args: vec![],
            span: Span::DUMMY,
        };
        assert!(verilog_may_yield(&stop, &interner));
    }

    #[test]
    fn sv_may_yield_break_return_continue() {
        use aion_sv_parser::ast::Statement;
        let interner = Interner::new();
        assert!(sv_may_yield(
            &Statement::Break { span: Span::DUMMY },
            &interner
        ));
        assert!(sv_may_yield(
            &Statement::Return {
                value: None,
                span: Span::DUMMY
            },
            &interner
        ));
        // `continue` restarts the loop; it does not leave it.
        assert!(!sv_may_yield(
            &Statement::Continue { span: Span::DUMMY },
            &interner
        ));
    }

    #[test]
    fn forever_with_delay_emits_no_w202() {
        let mut sdb = SourceDb::new();
        let file_id = sdb.add_source("test.sv", "5".into());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let env = SignalEnv::new();
        let stmt = aion_sv_parser::ast::Statement::Forever {
            body: Box::new(aion_sv_parser::ast::Statement::Delay {
                delay: aion_sv_parser::ast::Expr::Literal {
                    span: aion_source::Span {
                        file: file_id,
                        start: 0,
                        end: 1,
                    },
                },
                body: Box::new(aion_sv_parser::ast::Statement::Null { span: Span::DUMMY }),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        lower_sv_stmt(&stmt, &env, &sdb, &interner, &sink);
        assert!(sink.diagnostics().is_empty());
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
        // `forever ;` has no timing control → W202.
        let diags = sink.diagnostics();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, crate::errors::W202);
    }

    // ---- blocking vs non-blocking assignment kinds ----

    fn v_ident(name: aion_common::Ident) -> aion_verilog_parser::ast::Expr {
        aion_verilog_parser::ast::Expr::Identifier {
            name,
            span: Span::DUMMY,
        }
    }

    fn sv_ident(name: aion_common::Ident) -> aion_sv_parser::ast::Expr {
        aion_sv_parser::ast::Expr::Identifier {
            name,
            span: Span::DUMMY,
        }
    }

    fn vhdl_name(primary: aion_common::Ident) -> aion_vhdl_parser::ast::Expr {
        aion_vhdl_parser::ast::Expr::Name(aion_vhdl_parser::ast::Name {
            primary,
            parts: vec![],
            span: Span::DUMMY,
        })
    }

    fn assign_kind(ir: &IrStmt) -> AssignKind {
        match ir {
            IrStmt::Assign { kind, .. } => *kind,
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn verilog_nonblocking_assign_kind() {
        let (sdb, interner, sink, mut env) = setup();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        env.insert(a, SignalId::from_raw(0));
        env.insert(b, SignalId::from_raw(1));
        let stmt = aion_verilog_parser::ast::Statement::NonBlocking {
            target: v_ident(a),
            value: v_ident(b),
            span: Span::DUMMY,
        };
        let ir = lower_verilog_stmt(&stmt, &env, &sdb, &interner, &sink);
        assert_eq!(assign_kind(&ir), AssignKind::NonBlocking);
    }

    #[test]
    fn sv_blocking_and_nonblocking_assign_kinds() {
        let (sdb, interner, sink, mut env) = setup();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        env.insert(a, SignalId::from_raw(0));
        env.insert(b, SignalId::from_raw(1));
        let blocking = aion_sv_parser::ast::Statement::Blocking {
            target: sv_ident(a),
            value: sv_ident(b),
            span: Span::DUMMY,
        };
        let nonblocking = aion_sv_parser::ast::Statement::NonBlocking {
            target: sv_ident(a),
            value: sv_ident(b),
            span: Span::DUMMY,
        };
        let ir_b = lower_sv_stmt(&blocking, &env, &sdb, &interner, &sink);
        let ir_nb = lower_sv_stmt(&nonblocking, &env, &sdb, &interner, &sink);
        assert_eq!(assign_kind(&ir_b), AssignKind::Blocking);
        assert_eq!(assign_kind(&ir_nb), AssignKind::NonBlocking);
    }

    #[test]
    fn vhdl_variable_assignment_is_blocking() {
        let (sdb, interner, sink, mut env) = setup();
        let v = interner.get_or_intern("v");
        let d = interner.get_or_intern("d");
        env.insert(v, SignalId::from_raw(0));
        env.insert(d, SignalId::from_raw(1));
        let stmt = aion_vhdl_parser::ast::SequentialStatement::VariableAssignment {
            target: vhdl_name(v),
            value: vhdl_name(d),
            span: Span::DUMMY,
        };
        let ir = lower_vhdl_stmt(&stmt, &env, &sdb, &interner, &sink);
        assert_eq!(assign_kind(&ir), AssignKind::Blocking);
    }

    /// Parses `src` (language picked from the extension of `file_name`),
    /// elaborates it with top `top`, and returns the source text and kind of
    /// every procedural assignment in the top module's processes, in order.
    fn assign_kinds(file_name: &str, src: &str, top: &str) -> Vec<(String, AssignKind)> {
        fn walk(stmt: &IrStmt, sdb: &SourceDb, out: &mut Vec<(String, AssignKind)>) {
            match stmt {
                IrStmt::Assign { kind, span, .. } => {
                    out.push((sdb.snippet(*span).trim().to_string(), *kind));
                }
                IrStmt::Block { stmts, .. } => stmts.iter().for_each(|s| walk(s, sdb, out)),
                IrStmt::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    walk(then_body, sdb, out);
                    if let Some(e) = else_body {
                        walk(e, sdb, out);
                    }
                }
                IrStmt::Case { arms, default, .. } => {
                    arms.iter().for_each(|a| walk(&a.body, sdb, out));
                    if let Some(d) = default {
                        walk(d, sdb, out);
                    }
                }
                IrStmt::Delay { body, .. } | IrStmt::Forever { body, .. } => walk(body, sdb, out),
                _ => {}
            }
        }

        let (design, source_db, diags) = elaborate_src(file_name, src, top);
        assert!(
            !diags
                .iter()
                .any(|d| d.severity == aion_diagnostics::Severity::Error),
            "errors: {diags:?}"
        );
        let mut out = Vec::new();
        for (_, process) in design.modules[design.top].processes.iter() {
            walk(&process.body, &source_db, &mut out);
        }
        out
    }

    /// Parses `src` (language picked from the extension of `file_name`) and
    /// elaborates it with top `top`, returning the design, the source
    /// database and every diagnostic emitted.
    fn elaborate_src(
        file_name: &str,
        src: &str,
        top: &str,
    ) -> (aion_ir::Design, SourceDb, Vec<aion_diagnostics::Diagnostic>) {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source(file_name, src.to_string());
        let mut parsed = crate::ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![],
        };
        if file_name.ends_with(".sv") {
            parsed.sv_files.push(aion_sv_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else if file_name.ends_with(".vhd") {
            parsed.vhdl_files.push(aion_vhdl_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        } else {
            parsed.verilog_files.push(aion_verilog_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            ));
        }
        let config: aion_config::ProjectConfig = toml::from_str(&format!(
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\ntop = \"{top}\"\n"
        ))
        .unwrap();
        let design = crate::elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        (design, source_db, sink.diagnostics())
    }

    #[test]
    fn verilog_source_assign_kinds() {
        let kinds = assign_kinds(
            "t.v",
            "module t(input clk, input [7:0] d);\n\
             reg [7:0] tmp; reg [7:0] q;\n\
             always @(posedge clk) begin tmp = d; q <= tmp; end\n\
             endmodule",
            "t",
        );
        assert_eq!(
            kinds,
            vec![
                ("tmp = d;".to_string(), AssignKind::Blocking),
                ("q <= tmp;".to_string(), AssignKind::NonBlocking),
            ]
        );
    }

    #[test]
    fn sv_source_assign_kinds() {
        let kinds = assign_kinds(
            "t.sv",
            "module t(input logic clk, input logic [7:0] d);\n\
             logic [7:0] acc; logic [7:0] q; logic [7:0] n;\n\
             always_ff @(posedge clk) q <= d;\n\
             always_comb begin acc = d; acc += 8'd1; n = acc; end\n\
             endmodule",
            "t",
        );
        assert_eq!(kinds.len(), 4, "{kinds:?}");
        assert_eq!(kinds[0].1, AssignKind::NonBlocking, "{kinds:?}");
        // `=` and the compound `+=` are both blocking.
        assert!(
            kinds[1..].iter().all(|(_, k)| *k == AssignKind::Blocking),
            "{kinds:?}"
        );
    }

    #[test]
    fn vhdl_source_signal_assignment_is_nonblocking() {
        let kinds = assign_kinds(
            "t.vhd",
            "library ieee;\nuse ieee.std_logic_1164.all;\n\
             entity t is port (clk : in std_logic; d : in std_logic; q : out std_logic); end entity;\n\
             architecture rtl of t is\n\
             begin\n\
               process (clk) begin\n\
                 if rising_edge(clk) then q <= d; end if;\n\
               end process;\n\
             end architecture;",
            "t",
        );
        assert_eq!(kinds.len(), 1, "{kinds:?}");
        assert_eq!(kinds[0].1, AssignKind::NonBlocking, "{kinds:?}");
    }

    #[test]
    fn vhdl_source_variable_assignment_is_blocking() {
        let kinds = assign_kinds(
            "t.vhd",
            "library ieee;\nuse ieee.std_logic_1164.all;\n\
             entity t is port (clk : in std_logic; d : in std_logic; q : out std_logic); end entity;\n\
             architecture rtl of t is\n\
             begin\n\
               process (clk)\n\
                 variable v : std_logic;\n\
               begin\n\
                 if rising_edge(clk) then v := d; q <= v; end if;\n\
               end process;\n\
             end architecture;",
            "t",
        );
        assert_eq!(
            kinds.iter().map(|(_, k)| *k).collect::<Vec<_>>(),
            vec![AssignKind::Blocking, AssignKind::NonBlocking],
            "{kinds:?}"
        );
    }

    #[test]
    fn vhdl_process_variable_not_visible_outside_its_process() {
        // `v` belongs to the first process; the second one must not see it.
        let (_, _, diags) = elaborate_src(
            "t.vhd",
            "library ieee;\nuse ieee.std_logic_1164.all;\n\
             entity t is port (d : in std_logic; q : out std_logic; r : out std_logic); end entity;\n\
             architecture rtl of t is\n\
             begin\n\
               process (d)\n\
                 variable v : std_logic;\n\
               begin\n\
                 v := d; q <= v;\n\
               end process;\n\
               process (d) begin r <= v; end process;\n\
             end architecture;",
            "t",
        );
        let unknown: Vec<_> = diags
            .iter()
            .filter(|d| d.code.number == 204)
            .map(|d| d.message.clone())
            .collect();
        assert_eq!(unknown, vec!["unknown signal `v`".to_string()], "{diags:?}");
    }

    /// A top-module IR signal as `(name, kind, init as u64)`.
    type VhdlObject = (String, aion_ir::SignalKind, Option<u64>);

    /// Elaborates a VHDL architecture body `arch_body` (declarations, then
    /// `begin` and statements) of entity `t`, returning every top-module
    /// signal as `(name, kind, init as u64)` plus the diagnostics.
    fn vhdl_objects(arch_body: &str) -> (Vec<VhdlObject>, Vec<aion_diagnostics::Diagnostic>) {
        let src = format!(
            "library ieee;\nuse ieee.std_logic_1164.all;\n\
             entity t is port (a : in std_logic; y : out std_logic); end entity;\n\
             architecture rtl of t is\n{arch_body}\nend architecture;"
        );
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source("t.vhd", src);
        let parsed = crate::ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![aion_vhdl_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            )],
        };
        let config: aion_config::ProjectConfig =
            toml::from_str("[project]\nname = \"t\"\nversion = \"0.1.0\"\ntop = \"t\"\n").unwrap();
        let design = crate::elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        let objects = design.modules[design.top]
            .signals
            .iter()
            .map(|(_, s)| {
                let init = match &s.init {
                    Some(aion_ir::ConstValue::Logic(lv)) => lv.to_u64(),
                    Some(aion_ir::ConstValue::Int(v)) => Some(*v as u64),
                    _ => None,
                };
                (interner.resolve(s.name).to_string(), s.kind, init)
            })
            .collect();
        (objects, sink.diagnostics())
    }

    fn find<'a>(objects: &'a [VhdlObject], name: &str) -> &'a VhdlObject {
        objects
            .iter()
            .find(|(n, _, _)| n == name)
            .unwrap_or_else(|| panic!("no object {name} in {objects:?}"))
    }

    #[test]
    fn vhdl_process_locals_get_scoped_names_kinds_and_defaults() {
        use aion_ir::SignalKind;
        let (objects, diags) = vhdl_objects(
            "begin\n\
             p : process (a)\n\
               constant K : std_logic_vector(3 downto 0) := \"1010\";\n\
               variable t : std_logic_vector(3 downto 0) := \"0110\";\n\
               variable n : integer := 3;\n\
               variable u : std_logic;\n\
             begin y <= a; end process;\n\
             process (a) variable v : std_logic := '1'; begin y <= v; end process;",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(find(&objects, "p.k").1, SignalKind::Const);
        assert_eq!(find(&objects, "p.k").2, Some(0b1010));
        assert_eq!(find(&objects, "p.t").1, SignalKind::Variable);
        assert_eq!(find(&objects, "p.t").2, Some(0b0110));
        assert_eq!(find(&objects, "p.n").2, Some(3));
        // No default: no init (the simulator starts it as X).
        assert_eq!(find(&objects, "p.u").2, None);
        // Unlabelled process: `_process<N>` scope (N = its index).
        assert_eq!(find(&objects, "_process1.v").1, SignalKind::Variable);
        assert_eq!(find(&objects, "_process1.v").2, Some(1));
        assert!(objects.iter().all(|(n, _, _)| n != "t" && n != "v"));
    }

    #[test]
    fn vhdl_architecture_defaults_and_shared_variable_kind() {
        use aion_ir::SignalKind;
        let (objects, diags) = vhdl_objects(
            "constant K : std_logic_vector(7 downto 0) := x\"5A\";\n\
             constant Z : std_logic_vector(7 downto 0) := (others => '1');\n\
             constant P : std_logic_vector(1 downto 0) := ('1', '0');\n\
             constant K2 : std_logic_vector(7 downto 0) := K;\n\
             constant B : boolean := true;\n\
             signal s : std_logic := '0';\n\
             shared variable sv : std_logic;\n\
             begin\n y <= a;",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(find(&objects, "k").2, Some(0x5A));
        assert_eq!(find(&objects, "z").2, Some(0xFF));
        assert_eq!(find(&objects, "p").2, Some(0b10));
        assert_eq!(find(&objects, "k2").2, Some(0x5A));
        assert_eq!(find(&objects, "b").2, Some(1));
        assert_eq!(
            find(&objects, "s"),
            &("s".to_string(), SignalKind::Wire, Some(0))
        );
        // Shared variables are visible to all processes: not process-local.
        assert_eq!(find(&objects, "sv").1, SignalKind::Reg);
    }

    #[test]
    fn vhdl_unevaluable_default_is_reported_not_dropped_silently() {
        let (objects, diags) = vhdl_objects(
            "begin\n\
             p : process (a)\n\
               variable t : std_logic := a;\n\
             begin y <= t; end process;",
        );
        let w204: Vec<_> = diags.iter().filter(|d| d.code == errors::W204).collect();
        assert_eq!(w204.len(), 1, "{diags:?}");
        assert!(w204[0].message.contains("`t`"), "{}", w204[0].message);
        assert_eq!(find(&objects, "p.t").2, None);
    }

    // ========================================================================
    // VHDL conditional / selected / force assignments (#16)
    // ========================================================================

    /// Elaborates an architecture body (declarations, `begin`, statements)
    /// for a fixed test entity and returns the top module's IR, a signal
    /// lookup by name, and every diagnostic.
    fn elab_vhdl_arch(
        arch_body: &str,
    ) -> (
        aion_ir::module::Module,
        std::collections::HashMap<String, SignalId>,
        Vec<aion_diagnostics::Diagnostic>,
    ) {
        let src = format!(
            "library ieee;\nuse ieee.std_logic_1164.all;\n\
             entity t is port (\n\
               clk, rst, en, s : in std_logic;\n\
               sel : in std_logic_vector(1 downto 0);\n\
               a, b, c, d : in std_logic_vector(3 downto 0);\n\
               y, q, z : out std_logic_vector(3 downto 0);\n\
               hi, lo : out std_logic_vector(1 downto 0));\n\
             end entity;\n\
             architecture rtl of t is\n{arch_body}\nend architecture;"
        );
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source("t.vhd", src);
        let parsed = crate::ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![aion_vhdl_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            )],
        };
        let config: aion_config::ProjectConfig =
            toml::from_str("[project]\nname = \"t\"\nversion = \"0.1.0\"\ntop = \"t\"\n").unwrap();
        let design = crate::elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        let module = design.modules[design.top].clone();
        let names = module
            .signals
            .iter()
            .map(|(id, sig)| (interner.resolve(sig.name).to_string(), id))
            .collect();
        (module, names, sink.diagnostics())
    }

    /// Returns `true` if `e` reads exactly the whole signal `id`.
    fn is_sig(e: &IrExpr, id: SignalId) -> bool {
        matches!(e, IrExpr::Signal(SignalRef::Signal(s)) if *s == id)
    }

    fn errors_of(diags: &[aion_diagnostics::Diagnostic]) -> Vec<&aion_diagnostics::Diagnostic> {
        diags
            .iter()
            .filter(|d| d.severity == aion_diagnostics::Severity::Error)
            .collect()
    }

    fn assert_no_errors(diags: &[aion_diagnostics::Diagnostic]) {
        let errs = errors_of(diags);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    fn only_process(module: &aion_ir::module::Module) -> &aion_ir::process::Process {
        assert_eq!(module.processes.len(), 1, "{:?}", module.processes);
        module.processes.values().next().unwrap()
    }

    #[test]
    fn vhdl_concurrent_conditional_lowers_to_ternary_mux() {
        let (m, sig, diags) =
            elab_vhdl_arch("begin\n y <= a when sel = \"00\" else b when sel = \"01\" else c;");
        assert_no_errors(&diags);
        assert!(m.processes.is_empty(), "complete mux needs no process");
        assert_eq!(m.assignments.len(), 1);
        let asg = &m.assignments[0];
        assert_eq!(asg.target, SignalRef::Signal(sig["y"]));
        let IrExpr::Ternary {
            true_val,
            false_val,
            ..
        } = &asg.value
        else {
            panic!("expected ternary, got {:?}", asg.value);
        };
        assert!(is_sig(true_val, sig["a"]));
        let IrExpr::Ternary {
            true_val: t2,
            false_val: f2,
            ..
        } = &**false_val
        else {
            panic!("expected nested ternary, got {false_val:?}");
        };
        assert!(is_sig(t2, sig["b"]));
        assert!(is_sig(f2, sig["c"]));
    }

    #[test]
    fn vhdl_concurrent_conditional_without_else_is_latching_process() {
        let (m, sig, diags) = elab_vhdl_arch("begin\n q <= d when en = '1';");
        assert_no_errors(&diags);
        assert!(m.assignments.is_empty());
        let p = only_process(&m);
        assert_eq!(p.kind, aion_ir::process::ProcessKind::Combinational);
        assert!(matches!(p.sensitivity, aion_ir::process::Sensitivity::All));
        let IrStmt::If {
            then_body,
            else_body,
            ..
        } = &p.body
        else {
            panic!("expected if, got {:?}", p.body);
        };
        assert!(else_body.is_none(), "target must hold when en = '0'");
        assert!(matches!(
            &**then_body,
            IrStmt::Assign { target, kind: AssignKind::NonBlocking, .. }
                if *target == SignalRef::Signal(sig["q"])
        ));
    }

    #[test]
    fn vhdl_concurrent_conditional_unaffected_branch_is_empty() {
        let (m, _, diags) = elab_vhdl_arch("begin\n y <= a when en = '1' else unaffected;");
        assert_no_errors(&diags);
        assert!(m.assignments.is_empty());
        let IrStmt::If {
            then_body,
            else_body,
            ..
        } = &only_process(&m).body
        else {
            panic!("expected if");
        };
        assert!(matches!(&**then_body, IrStmt::Assign { .. }));
        assert!(matches!(else_body.as_deref(), Some(IrStmt::Nop)));
    }

    #[test]
    fn vhdl_concurrent_all_unaffected_emits_nothing() {
        let (m, _, diags) = elab_vhdl_arch("begin\n y <= unaffected;");
        assert_no_errors(&diags);
        assert!(m.assignments.is_empty());
        assert!(m.processes.is_empty());
    }

    #[test]
    fn vhdl_concurrent_clocked_conditional_is_sequential_process() {
        use aion_ir::process::{Edge, ProcessKind, Sensitivity};
        let (m, sig, diags) = elab_vhdl_arch("begin\n q <= d when rising_edge(clk);");
        assert_no_errors(&diags);
        let p = only_process(&m);
        assert_eq!(p.kind, ProcessKind::Sequential);
        let Sensitivity::EdgeList(edges) = &p.sensitivity else {
            panic!("expected edge list, got {:?}", p.sensitivity);
        };
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].signal, sig["clk"]);
        assert_eq!(edges[0].edge, Edge::Posedge);
        assert!(matches!(
            p.body,
            IrStmt::If {
                else_body: None,
                ..
            }
        ));
    }

    #[test]
    fn vhdl_concurrent_async_reset_conditional_sensitivity() {
        use aion_ir::process::{Edge, Sensitivity};
        let (m, sig, diags) = elab_vhdl_arch(
            "begin\n q <= (others => '0') when rst = '1' else d when falling_edge(clk);",
        );
        assert_no_errors(&diags);
        let p = only_process(&m);
        let Sensitivity::EdgeList(edges) = &p.sensitivity else {
            panic!("expected edge list, got {:?}", p.sensitivity);
        };
        let pairs: Vec<_> = edges.iter().map(|e| (e.signal, e.edge)).collect();
        // The clock edge, then the asynchronous reset; `d` is data only.
        assert_eq!(
            pairs,
            vec![(sig["clk"], Edge::Negedge), (sig["rst"], Edge::Both)]
        );
        // if rst = '1' then q <= 0; elsif falling_edge(clk) then q <= d;
        let IrStmt::If { else_body, .. } = &p.body else {
            panic!("expected if");
        };
        assert!(matches!(
            else_body.as_deref(),
            Some(IrStmt::If {
                else_body: None,
                ..
            })
        ));
    }

    #[test]
    fn vhdl_concurrent_selected_lowers_to_case_process() {
        let (m, sig, diags) = elab_vhdl_arch(
            "begin\n with sel select y <= a when \"00\" | \"01\", b when \"10\", c when others;",
        );
        assert_no_errors(&diags);
        assert!(m.assignments.is_empty());
        let p = only_process(&m);
        assert_eq!(p.kind, aion_ir::process::ProcessKind::Combinational);
        assert!(matches!(p.sensitivity, aion_ir::process::Sensitivity::All));
        let IrStmt::Case {
            subject,
            arms,
            default,
            ..
        } = &p.body
        else {
            panic!("expected case, got {:?}", p.body);
        };
        assert!(is_sig(subject, sig["sel"]));
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[0].patterns.len(), 2);
        assert_eq!(arms[1].patterns.len(), 1);
        assert!(matches!(
            default.as_deref(),
            Some(IrStmt::Assign { value: IrExpr::Signal(SignalRef::Signal(id)), .. }) if *id == sig["c"]
        ));
    }

    #[test]
    fn vhdl_selected_range_choice_is_reported_not_dropped() {
        let (_, _, diags) = elab_vhdl_arch(
            "signal n : integer range 0 to 7;\nbegin\n with n select y <= a when 0 to 3, b when others;",
        );
        let errs = errors_of(&diags);
        assert_eq!(errs.len(), 1, "{diags:?}");
        assert_eq!(errs[0].code, errors::E210);
        assert!(errs[0].message.contains("range choice"));
    }

    #[test]
    fn vhdl_matching_select_dont_care_is_reported() {
        let (_, _, diags) =
            elab_vhdl_arch("begin\n with sel select? y <= a when \"1-\", b when others;");
        let errs = errors_of(&diags);
        assert_eq!(errs.len(), 1, "{diags:?}");
        assert!(errs[0].message.contains("don't-care"));

        // Without '-' choices, `select?` behaves like `select` for 0/1 values.
        let (m, _, diags) =
            elab_vhdl_arch("begin\n with sel select? y <= a when \"10\", b when others;");
        assert_no_errors(&diags);
        assert!(matches!(only_process(&m).body, IrStmt::Case { .. }));
    }

    #[test]
    fn vhdl_sequential_conditional_and_selected_assignment_kinds() {
        let kinds = assign_kinds(
            "t.vhd",
            "library ieee;\nuse ieee.std_logic_1164.all;\n\
             entity t is port (s, a, b : in std_logic; y, z : out std_logic); end entity;\n\
             architecture rtl of t is\n\
             begin\n\
               process (all)\n\
                 variable v : std_logic;\n\
               begin\n\
                 y <= a when s = '1' else b;\n\
                 v := b when s = '1' else a;\n\
                 with s select z <= v when '1', a when others;\n\
               end process;\n\
             end architecture;",
            "t",
        );
        let kinds: Vec<_> = kinds.into_iter().map(|(_, k)| k).collect();
        assert_eq!(
            kinds,
            vec![
                AssignKind::NonBlocking,
                AssignKind::NonBlocking,
                AssignKind::Blocking,
                AssignKind::Blocking,
                AssignKind::NonBlocking,
                AssignKind::NonBlocking,
            ]
        );
    }

    #[test]
    fn vhdl_selected_variable_assignment_lowers_to_blocking_case() {
        let (m, sig, diags) = elab_vhdl_arch(
            "begin\n process (all)\n variable v : std_logic_vector(3 downto 0);\n begin\n \
             with sel select v := a when \"00\", b when others;\n y <= v;\n end process;",
        );
        assert_no_errors(&diags);
        let IrStmt::Block { stmts, .. } = &only_process(&m).body else {
            panic!("expected block");
        };
        let IrStmt::Case { arms, default, .. } = &stmts[0] else {
            panic!("expected case, got {:?}", stmts[0]);
        };
        assert!(matches!(
            arms[0].body,
            IrStmt::Assign {
                kind: AssignKind::Blocking,
                ..
            }
        ));
        assert!(default.is_some());
        assert!(!sig.contains_key("v"), "process variables are scoped");
    }

    #[test]
    fn vhdl_sequential_clocked_conditional_makes_process_sequential() {
        let (m, _, diags) = elab_vhdl_arch(
            "begin\n process (clk)\n begin\n q <= d when rising_edge(clk);\n end process;",
        );
        assert_no_errors(&diags);
        assert_eq!(
            only_process(&m).kind,
            aion_ir::process::ProcessKind::Sequential
        );
    }

    #[test]
    fn vhdl_force_and_release_are_rejected() {
        let (_, _, diags) = elab_vhdl_arch(
            "begin\n process\n begin\n y <= force a;\n \
             with s select y <= force a when '1', b when others;\n y <= release;\n \
             wait;\n end process;",
        );
        let errs = errors_of(&diags);
        assert_eq!(errs.len(), 3, "{diags:?}");
        assert!(errs.iter().all(|e| e.code == errors::E210));
        assert!(errs[0].message.contains("force"));
        assert!(errs[2].message.contains("release"));
    }

    #[test]
    fn vhdl_aggregate_target_lowers_to_concat() {
        let (m, sig, diags) = elab_vhdl_arch("begin\n (hi, lo) <= a;");
        assert_no_errors(&diags);
        assert_eq!(
            m.assignments[0].target,
            SignalRef::Concat(vec![
                SignalRef::Signal(sig["hi"]),
                SignalRef::Signal(sig["lo"])
            ])
        );
    }

    #[test]
    fn vhdl_named_aggregate_target_is_reported() {
        let (_, _, diags) = elab_vhdl_arch("begin\n (1 => hi, 0 => lo) <= a;");
        let errs = errors_of(&diags);
        assert_eq!(errs.len(), 1, "{diags:?}");
        assert!(errs[0].message.contains("aggregate assignment target"));
    }

    #[test]
    fn vhdl_delay_mechanism_is_ignored_for_lowering() {
        let (m, sig, diags) = elab_vhdl_arch(
            "begin\n y <= transport a after 1 ns;\n z <= reject 1 ns inertial b when en = '1' else c;",
        );
        assert_no_errors(&diags);
        assert_eq!(m.assignments.len(), 2);
        assert!(is_sig(&m.assignments[0].value, sig["a"]));
        assert!(matches!(m.assignments[1].value, IrExpr::Ternary { .. }));
    }

    #[test]
    fn first_waveform_value_and_conditional_value_helpers() {
        use aion_vhdl_parser::ast::{ConditionalWaveform, SignalWaveform, Waveform};
        let (sdb, interner, sink, mut env) = setup();
        let a = interner.get_or_intern("a");
        let c = interner.get_or_intern("c");
        env.insert(a, SignalId::from_raw(0));
        env.insert(c, SignalId::from_raw(1));
        let wf = |id| {
            SignalWaveform::Elements(vec![Waveform {
                value: vhdl_name(id),
                after: None,
                span: Span::DUMMY,
            }])
        };
        assert!(first_waveform_value(&wf(a)).is_some());
        assert!(first_waveform_value(&SignalWaveform::Unaffected(Span::DUMMY)).is_none());

        let guarded = ConditionalWaveform {
            waveform: wf(a),
            condition: Some(vhdl_name(c)),
            span: Span::DUMMY,
        };
        let fallback = ConditionalWaveform {
            waveform: wf(c),
            condition: None,
            span: Span::DUMMY,
        };
        let complete = [guarded.clone(), fallback];
        assert!(matches!(
            lower_vhdl_conditional_value(&complete, &env, &sdb, &interner, &sink),
            Some(IrExpr::Ternary { .. })
        ));
        // No final else: not a pure mux.
        assert!(lower_vhdl_conditional_value(&[guarded], &env, &sdb, &interner, &sink).is_none());
        assert!(lower_vhdl_conditional_value(&[], &env, &sdb, &interner, &sink).is_none());
        assert!(!sink.has_errors());
    }

    #[test]
    fn vhdl_sized_bit_string_defaults() {
        // Issue #22: sized/signed/decimal bit-string literals as defaults.
        let (objects, diags) = vhdl_objects(
            "constant U8 : std_logic_vector(7 downto 0) := 8UX\"F\";\n\
             constant S8 : std_logic_vector(7 downto 0) := 8SX\"F\";\n\
             constant D16 : std_logic_vector(15 downto 0) := 16D\"255\";\n\
             constant S12 : std_logic_vector(11 downto 0) := 12SB\"101\";\n\
             constant UX4 : std_logic_vector(3 downto 0) := UX\"A\";\n\
             begin\n y <= a;",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(find(&objects, "u8").2, Some(0x0F));
        assert_eq!(find(&objects, "s8").2, Some(0xFF));
        assert_eq!(find(&objects, "d16").2, Some(255));
        assert_eq!(find(&objects, "s12").2, Some(0xFFD));
        assert_eq!(find(&objects, "ux4").2, Some(0xA));
    }

    #[test]
    fn vhdl_truncating_bit_string_is_a_parse_error() {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source(
            "t.vhd",
            "architecture rtl of t is constant K : std_logic_vector(2 downto 0) := 3UX\"F\"; \
             begin end;"
                .to_string(),
        );
        let _ = aion_vhdl_parser::parse_file(file_id, &source_db, &interner, &sink);
        let diags = sink.diagnostics();
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("cannot be truncated"));
    }
}
