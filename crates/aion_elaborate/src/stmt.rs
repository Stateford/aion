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
pub(crate) const DEFAULT_TIMESCALE_FS: u64 = 1_000_000;

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
            timing,
            value,
            span,
        } => {
            // The IR has no delayed assignment: lower as zero-delay and say so.
            if let Some(t) = timing {
                sink.emit(errors::warn_intra_assignment_timing_ignored(
                    matches!(t, aion_verilog_parser::ast::TimingControl::Delay(_)),
                    false,
                    t.span(),
                ));
            }
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
            timing,
            value,
            span,
        } => {
            // The IR has no delayed assignment: lower as zero-delay and say so.
            if let Some(t) = timing {
                sink.emit(errors::warn_intra_assignment_timing_ignored(
                    matches!(t, aion_verilog_parser::ast::TimingControl::Delay(_)),
                    true,
                    t.span(),
                ));
            }
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
            let duration_fs = eval_delay_expr_verilog(delay, sig_env, source_db, interner, sink);
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
            timing,
            value,
            span,
        } => {
            // The IR has no delayed assignment: lower as zero-delay and say so.
            if let Some(t) = timing {
                sink.emit(errors::warn_intra_assignment_timing_ignored(
                    matches!(t, aion_sv_parser::ast::TimingControl::Delay(_)),
                    false,
                    t.span(),
                ));
            }
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
            timing,
            value,
            span,
        } => {
            // The IR has no delayed assignment: lower as zero-delay and say so.
            if let Some(t) = timing {
                sink.emit(errors::warn_intra_assignment_timing_ignored(
                    matches!(t, aion_sv_parser::ast::TimingControl::Delay(_)),
                    true,
                    t.span(),
                ));
            }
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
            let duration_fs = eval_delay_expr_sv(delay, sig_env, source_db, interner, sink);
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
/// Tries to const-evaluate the expression, with the parameters in
/// `sig_env` in scope (`#(PERIOD/2)`); if it resolves to an integer,
/// multiplies by `DEFAULT_TIMESCALE_FS` (1 ns). Falls back to 0 fs if
/// the expression cannot be evaluated.
fn eval_delay_expr_verilog(
    expr: &aion_verilog_parser::ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> u64 {
    let env = sig_env.consts();
    if let Some(val) = const_eval::eval_verilog_expr(expr, source_db, interner, env, sink) {
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
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> u64 {
    // A time literal (`#10ns`, `#(1.5ps)`) carries its own unit.
    let mut inner = expr;
    while let aion_sv_parser::ast::Expr::Paren { inner: i, .. } = inner {
        inner = i;
    }
    if let aion_sv_parser::ast::Expr::TimeLiteral { span } = inner {
        return time_literal_delay_fs(*span, source_db, sink);
    }
    let env = sig_env.consts();
    if let Some(val) = const_eval::eval_sv_expr(expr, source_db, interner, env, sink) {
        if let Some(v) = const_eval::const_to_i64(&val) {
            return (v.unsigned_abs()) * DEFAULT_TIMESCALE_FS;
        }
    }
    0
}

/// Converts a SystemVerilog time literal used as a delay to femtoseconds.
///
/// A literal with a unit is absolute, so `#10ns` is 10,000,000 fs whatever
/// the time unit; only rounding to the time precision depends on the
/// timescale, which the elaborator does not know. The IR resolution (1 fs) is
/// used as the precision: a literal finer than that is rounded with a W207
/// warning, and `1step` (one precision unit) becomes 1 fs with a W207
/// warning. A literal too large for 64-bit femtoseconds is an E209 error and
/// lowers to 0.
fn time_literal_delay_fs(
    span: aion_source::Span,
    source_db: &SourceDb,
    sink: &DiagnosticSink,
) -> u64 {
    let text = source_db.snippet(span);
    match const_eval::parse_time_literal(text) {
        Some(const_eval::TimeLiteralValue::Fs { fs, exact: true }) => fs,
        Some(const_eval::TimeLiteralValue::Fs { fs, exact: false }) => {
            sink.emit(errors::warn_time_literal_assumed(
                text,
                &format!("rounded to {fs} fs, the finest time step Aion represents"),
                span,
            ));
            fs
        }
        Some(const_eval::TimeLiteralValue::Step) => {
            sink.emit(errors::warn_time_literal_assumed(
                text,
                "`1step` is one unit of the time precision, which is unknown; assumed 1 fs",
                span,
            ));
            1
        }
        None => {
            sink.emit(errors::error_param_not_const(
                &format!("time literal `{text}` is out of range"),
                span,
            ));
            0
        }
    }
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
            timing: None,
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
            timing: None,
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
            timing: None,
            value: sv_ident(b),
            span: Span::DUMMY,
        };
        let nonblocking = aion_sv_parser::ast::Statement::NonBlocking {
            target: sv_ident(a),
            timing: None,
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
}
