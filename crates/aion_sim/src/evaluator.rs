//! Expression evaluator and statement executor for the simulation kernel.
//!
//! [`eval_expr`] recursively evaluates an IR [`Expr`] tree into a [`LogicVec`],
//! reading signal values from the simulation state. [`exec_statement`] executes
//! a [`Statement`] tree, collecting deferred [`PendingUpdate`]s for the kernel
//! to commit after the process segment completes.
//!
//! Blocking assignments ([`AssignKind::Blocking`]) are also recorded in a
//! process-local overlay, so later statements of the same segment read the
//! new value immediately. Non-blocking assignments
//! ([`AssignKind::NonBlocking`]) evaluate their right-hand side right away but
//! leave the overlay untouched; the kernel applies them in the NBA region.

use std::collections::HashMap;

use aion_common::{Logic, LogicVec};
use aion_ir::arena::Arena;
use aion_ir::{
    AssertionKind, AssignKind, BinaryOp, Expr, SignalId, SignalRef, Statement, TypeDb, UnaryOp,
};

use crate::error::SimError;
use crate::value::{SimSignalId, SimSignalState};

/// A deferred signal update collected during statement execution.
///
/// Updates are not written to the shared signal state while the process
/// runs; the kernel commits them after the process segment finishes. How
/// they are committed depends on [`kind`](Self::kind): blocking updates
/// become active-region events for the next delta (the writing process
/// already saw them through its overlay), while non-blocking updates wait
/// in the kernel's NBA queue until the active region at the current time
/// has drained.
#[derive(Debug, Clone)]
pub struct PendingUpdate {
    /// The flat simulation signal to update.
    pub target: SimSignalId,
    /// The new value to drive.
    pub value: LogicVec,
    /// Optional bit range for partial updates `(high, low)` inclusive.
    pub range: Option<(u32, u32)>,
    /// Whether the update came from a blocking or a non-blocking assignment,
    /// which decides the scheduling region it is applied in.
    pub kind: AssignKind,
}

/// The result of executing a single statement.
#[derive(Debug, Clone)]
pub enum ExecResult {
    /// Continue executing subsequent statements.
    Continue,
    /// Stop simulation (`$finish` was encountered).
    Finish,
    /// Suspend execution for a delay, resuming later with the continuation.
    ///
    /// The kernel saves the continuation and schedules a wakeup event
    /// at `current_time + delay_fs`.
    Suspend {
        /// Delay in femtoseconds before resuming.
        delay_fs: u64,
        /// The remaining statements to execute after the delay.
        continuation: Box<Statement>,
    },
}

/// Context for expression evaluation and statement execution.
///
/// Holds references to the simulation signal arena, the mapping from
/// IR `SignalId` to flat `SimSignalId`, and the type database.
pub struct EvalContext<'a> {
    /// The flat simulation signal states.
    pub signals: &'a Arena<SimSignalId, SimSignalState>,
    /// Mapping from per-module IR `SignalId` to flat `SimSignalId`.
    pub signal_map: &'a HashMap<SignalId, SimSignalId>,
    /// The type database for width lookups.
    pub types: &'a TypeDb,
}

/// Returns `true` if the least-significant bit of a `LogicVec` is `Logic::One`.
///
/// Used for condition evaluation in if/case statements.
pub fn logic_is_true(lv: &LogicVec) -> bool {
    lv.width() > 0 && lv.get(0) == Logic::One
}

/// Process-local values written by blocking assignments during one process
/// execution segment, keyed by flat signal.
///
/// Reads inside the same segment consult this overlay before the committed
/// signal state, so `tmp = a + b; y = tmp;` sees the new `tmp`. The overlay
/// lives only for a single [`exec_statement`] call: the kernel commits the
/// corresponding blocking [`PendingUpdate`]s before the process runs again
/// (including after a `#delay`), so nothing needs to survive a suspension.
type BlockingOverlay = HashMap<SimSignalId, LogicVec>;

/// Evaluates an IR expression into a `LogicVec`.
///
/// Recursively walks the expression tree, reading signal values from the
/// evaluation context. Operations involving X or Z values propagate
/// unknown results following IEEE 1364 semantics.
pub fn eval_expr(ctx: &EvalContext<'_>, expr: &Expr) -> Result<LogicVec, SimError> {
    eval_expr_ov(ctx, &BlockingOverlay::new(), expr)
}

/// Returns the value of `id` as seen by the executing process: its own
/// pending blocking write if it made one, else the committed signal value.
fn read_signal<'v>(
    ctx: &'v EvalContext<'_>,
    ov: &'v BlockingOverlay,
    id: SimSignalId,
) -> &'v LogicVec {
    ov.get(&id).unwrap_or_else(|| &ctx.signals.get(id).value)
}

/// Evaluates an expression, reading signals through the blocking overlay `ov`.
fn eval_expr_ov(
    ctx: &EvalContext<'_>,
    ov: &BlockingOverlay,
    expr: &Expr,
) -> Result<LogicVec, SimError> {
    match expr {
        Expr::Literal(lv) => Ok(lv.clone()),

        Expr::Signal(signal_ref) => eval_signal_ref_ov(ctx, ov, signal_ref),

        Expr::Unary { op, operand, .. } => {
            let val = eval_expr_ov(ctx, ov, operand)?;
            eval_unary(*op, &val)
        }

        Expr::Binary { op, lhs, rhs, .. } => {
            let l = eval_expr_ov(ctx, ov, lhs)?;
            let r = eval_expr_ov(ctx, ov, rhs)?;
            eval_binary(*op, &l, &r)
        }

        Expr::Ternary {
            condition,
            true_val,
            false_val,
            ..
        } => {
            let cond = eval_expr_ov(ctx, ov, condition)?;
            if has_xz(&cond) {
                // Unknown condition → all X result
                let tv = eval_expr_ov(ctx, ov, true_val)?;
                let mut result = LogicVec::new(tv.width());
                for i in 0..tv.width() {
                    result.set(i, Logic::X);
                }
                Ok(result)
            } else if logic_is_true(&cond) {
                eval_expr_ov(ctx, ov, true_val)
            } else {
                eval_expr_ov(ctx, ov, false_val)
            }
        }

        Expr::Concat(parts) => {
            let mut evaluated: Vec<LogicVec> = Vec::with_capacity(parts.len());
            for p in parts {
                evaluated.push(eval_expr_ov(ctx, ov, p)?);
            }
            let total_width: u32 = evaluated.iter().map(|v| v.width()).sum();
            let mut result = LogicVec::new(total_width);
            let mut offset = 0u32;
            // Concat: rightmost (last) element goes to LSB
            for part in evaluated.iter().rev() {
                for i in 0..part.width() {
                    result.set(offset + i, part.get(i));
                }
                offset += part.width();
            }
            Ok(result)
        }

        Expr::Repeat { expr, count, .. } => {
            let val = eval_expr_ov(ctx, ov, expr)?;
            let total_width = val.width() * count;
            let mut result = LogicVec::new(total_width);
            for rep in 0..*count {
                for i in 0..val.width() {
                    result.set(rep * val.width() + i, val.get(i));
                }
            }
            Ok(result)
        }

        Expr::Index { expr, index, .. } => {
            let val = eval_expr_ov(ctx, ov, expr)?;
            let idx_val = eval_expr_ov(ctx, ov, index)?;
            match idx_val.to_u64() {
                Some(idx) if (idx as u32) < val.width() => {
                    Ok(LogicVec::from_bool(val.get(idx as u32) == Logic::One))
                }
                Some(_) => {
                    // Out of range → X
                    let mut r = LogicVec::new(1);
                    r.set(0, Logic::X);
                    Ok(r)
                }
                None => {
                    // X/Z index → X
                    let mut r = LogicVec::new(1);
                    r.set(0, Logic::X);
                    Ok(r)
                }
            }
        }

        Expr::Slice {
            expr, high, low, ..
        } => {
            let val = eval_expr_ov(ctx, ov, expr)?;
            let h_val = eval_expr_ov(ctx, ov, high)?;
            let l_val = eval_expr_ov(ctx, ov, low)?;
            match (h_val.to_u64(), l_val.to_u64()) {
                (Some(h), Some(l)) if h >= l => {
                    let width = (h - l + 1) as u32;
                    let mut result = LogicVec::new(width);
                    for i in 0..width {
                        let src_idx = l as u32 + i;
                        if src_idx < val.width() {
                            result.set(i, val.get(src_idx));
                        } else {
                            result.set(i, Logic::X);
                        }
                    }
                    Ok(result)
                }
                _ => Err(SimError::EvalError {
                    reason: "invalid slice bounds".into(),
                }),
            }
        }

        Expr::FuncCall { .. } => Err(SimError::Unsupported {
            reason: "function calls in simulation".into(),
        }),
    }
}

/// Evaluates a `SignalRef` to its current `LogicVec` value, as seen through
/// the executing process's blocking overlay `ov`.
fn eval_signal_ref_ov(
    ctx: &EvalContext<'_>,
    ov: &BlockingOverlay,
    signal_ref: &SignalRef,
) -> Result<LogicVec, SimError> {
    match signal_ref {
        SignalRef::Signal(sig_id) => {
            let sim_id = ctx
                .signal_map
                .get(sig_id)
                .ok_or_else(|| SimError::InvalidSignalRef {
                    reason: format!("unmapped signal ID {}", sig_id.as_raw()),
                })?;
            Ok(read_signal(ctx, ov, *sim_id).clone())
        }
        SignalRef::Slice { signal, high, low } => {
            let sim_id = ctx
                .signal_map
                .get(signal)
                .ok_or_else(|| SimError::InvalidSignalRef {
                    reason: format!("unmapped signal ID {}", signal.as_raw()),
                })?;
            let full = read_signal(ctx, ov, *sim_id);
            let width = high - low + 1;
            let mut result = LogicVec::new(width);
            for i in 0..width {
                let src = low + i;
                if src < full.width() {
                    result.set(i, full.get(src));
                } else {
                    result.set(i, Logic::X);
                }
            }
            Ok(result)
        }
        SignalRef::Concat(refs) => {
            let mut parts = Vec::with_capacity(refs.len());
            for r in refs {
                parts.push(eval_signal_ref_ov(ctx, ov, r)?);
            }
            let total_width: u32 = parts.iter().map(|v| v.width()).sum();
            let mut result = LogicVec::new(total_width);
            let mut offset = 0u32;
            for part in parts.iter().rev() {
                for i in 0..part.width() {
                    result.set(offset + i, part.get(i));
                }
                offset += part.width();
            }
            Ok(result)
        }
        SignalRef::Const(lv) => Ok(lv.clone()),
    }
}

/// Evaluates a unary operation on a `LogicVec`.
fn eval_unary(op: UnaryOp, val: &LogicVec) -> Result<LogicVec, SimError> {
    match op {
        UnaryOp::Not => Ok(!val),
        UnaryOp::Neg => match val.to_u64() {
            Some(v) => {
                let negated = (v as i64).wrapping_neg() as u64;
                Ok(LogicVec::from_u64(negated, val.width()))
            }
            None => Ok(all_x(val.width())),
        },
        UnaryOp::RedAnd => {
            let result = if val.is_all_one() {
                Logic::One
            } else if has_xz(val) {
                Logic::X
            } else {
                Logic::Zero
            };
            let mut r = LogicVec::new(1);
            r.set(0, result);
            Ok(r)
        }
        UnaryOp::RedOr => {
            let has_one = (0..val.width()).any(|i| val.get(i) == Logic::One);
            let result = if has_one {
                Logic::One
            } else if has_xz(val) {
                Logic::X
            } else {
                Logic::Zero
            };
            let mut r = LogicVec::new(1);
            r.set(0, result);
            Ok(r)
        }
        UnaryOp::RedXor => match val.to_u64() {
            Some(v) => {
                let parity = v.count_ones() % 2;
                Ok(LogicVec::from_bool(parity == 1))
            }
            None => {
                let mut r = LogicVec::new(1);
                r.set(0, Logic::X);
                Ok(r)
            }
        },
        UnaryOp::LogicNot => {
            if has_xz(val) {
                let mut r = LogicVec::new(1);
                r.set(0, Logic::X);
                Ok(r)
            } else {
                Ok(LogicVec::from_bool(val.is_all_zero()))
            }
        }
    }
}

/// Evaluates a binary operation on two `LogicVec` operands.
#[allow(clippy::too_many_lines)]
fn eval_binary(op: BinaryOp, lhs: &LogicVec, rhs: &LogicVec) -> Result<LogicVec, SimError> {
    match op {
        // Bitwise operations — use LogicVec operators directly
        BinaryOp::And => {
            let (l, r) = match_widths(lhs, rhs);
            Ok(&l & &r)
        }
        BinaryOp::Or => {
            let (l, r) = match_widths(lhs, rhs);
            Ok(&l | &r)
        }
        BinaryOp::Xor => {
            let (l, r) = match_widths(lhs, rhs);
            Ok(&l ^ &r)
        }

        // Arithmetic — via u64, X/Z → all X
        BinaryOp::Add => arith_op(lhs, rhs, |a, b| a.wrapping_add(b)),
        BinaryOp::Sub => arith_op(lhs, rhs, |a, b| a.wrapping_sub(b)),
        BinaryOp::Mul => arith_op(lhs, rhs, |a, b| a.wrapping_mul(b)),
        BinaryOp::Div => {
            let (lv, rv) = (lhs.to_u64(), rhs.to_u64());
            match (lv, rv) {
                (_, Some(0)) => Err(SimError::DivisionByZero),
                (Some(a), Some(b)) => {
                    let width = lhs.width().max(rhs.width());
                    Ok(LogicVec::from_u64(a / b, width))
                }
                _ => Ok(all_x(lhs.width().max(rhs.width()))),
            }
        }
        BinaryOp::Mod => {
            let (lv, rv) = (lhs.to_u64(), rhs.to_u64());
            match (lv, rv) {
                (_, Some(0)) => Err(SimError::DivisionByZero),
                (Some(a), Some(b)) => {
                    let width = lhs.width().max(rhs.width());
                    Ok(LogicVec::from_u64(a % b, width))
                }
                _ => Ok(all_x(lhs.width().max(rhs.width()))),
            }
        }
        BinaryOp::Pow => match (lhs.to_u64(), rhs.to_u64()) {
            (Some(base), Some(exp)) => {
                let result = (base as u128).pow(exp as u32) as u64;
                Ok(LogicVec::from_u64(result, lhs.width().max(rhs.width())))
            }
            _ => Ok(all_x(lhs.width().max(rhs.width()))),
        },

        // Shift
        BinaryOp::Shl => match (lhs.to_u64(), rhs.to_u64()) {
            (Some(a), Some(b)) => {
                let result = if b >= 64 { 0 } else { a << b };
                Ok(LogicVec::from_u64(result, lhs.width()))
            }
            _ => Ok(all_x(lhs.width())),
        },
        BinaryOp::Shr => match (lhs.to_u64(), rhs.to_u64()) {
            (Some(a), Some(b)) => {
                let result = if b >= 64 { 0 } else { a >> b };
                Ok(LogicVec::from_u64(result, lhs.width()))
            }
            _ => Ok(all_x(lhs.width())),
        },

        // Comparison — result is 1-bit
        BinaryOp::Eq => cmp_op(lhs, rhs, |a, b| a == b),
        BinaryOp::Ne => cmp_op(lhs, rhs, |a, b| a != b),
        BinaryOp::Lt => cmp_op(lhs, rhs, |a, b| a < b),
        BinaryOp::Le => cmp_op(lhs, rhs, |a, b| a <= b),
        BinaryOp::Gt => cmp_op(lhs, rhs, |a, b| a > b),
        BinaryOp::Ge => cmp_op(lhs, rhs, |a, b| a >= b),

        // Logical
        BinaryOp::LogicAnd => {
            if has_xz(lhs) || has_xz(rhs) {
                let mut r = LogicVec::new(1);
                // If either is definitely false (all zero with no X/Z), result is 0
                if !has_xz(lhs) && lhs.is_all_zero() {
                    return Ok(LogicVec::from_bool(false));
                }
                if !has_xz(rhs) && rhs.is_all_zero() {
                    return Ok(LogicVec::from_bool(false));
                }
                r.set(0, Logic::X);
                Ok(r)
            } else {
                let l_true = !lhs.is_all_zero();
                let r_true = !rhs.is_all_zero();
                Ok(LogicVec::from_bool(l_true && r_true))
            }
        }
        BinaryOp::LogicOr => {
            if has_xz(lhs) || has_xz(rhs) {
                // If either is definitely true (has a One bit, no X/Z), result is 1
                if !has_xz(lhs) && !lhs.is_all_zero() {
                    return Ok(LogicVec::from_bool(true));
                }
                if !has_xz(rhs) && !rhs.is_all_zero() {
                    return Ok(LogicVec::from_bool(true));
                }
                let mut r = LogicVec::new(1);
                r.set(0, Logic::X);
                Ok(r)
            } else {
                let l_true = !lhs.is_all_zero();
                let r_true = !rhs.is_all_zero();
                Ok(LogicVec::from_bool(l_true || r_true))
            }
        }
    }
}

/// Executes a statement, collecting pending updates and display output.
///
/// The call is one process execution segment: it runs until the statement
/// completes, suspends on a delay, or hits `$finish`. Blocking writes made
/// during the segment are visible to its later reads (through a
/// process-local overlay) and are also returned in `pending` for the kernel
/// to commit. Non-blocking writes are returned in `pending` with
/// [`AssignKind::NonBlocking`] and are not visible to later reads.
///
/// Returns `ExecResult::Finish` if a `$finish` is encountered.
pub fn exec_statement(
    ctx: &EvalContext<'_>,
    stmt: &Statement,
    pending: &mut Vec<PendingUpdate>,
    display_output: &mut Vec<String>,
) -> Result<ExecResult, SimError> {
    let mut overlay = BlockingOverlay::new();
    exec_stmt_ov(ctx, &mut overlay, stmt, pending, display_output)
}

/// Executes `stmt` within a process segment whose blocking writes so far are
/// recorded in `overlay`.
fn exec_stmt_ov(
    ctx: &EvalContext<'_>,
    overlay: &mut BlockingOverlay,
    stmt: &Statement,
    pending: &mut Vec<PendingUpdate>,
    display_output: &mut Vec<String>,
) -> Result<ExecResult, SimError> {
    match stmt {
        Statement::Assign {
            target,
            value,
            kind,
            ..
        } => {
            // The right-hand side is evaluated now for both kinds, reading
            // this segment's own earlier blocking writes.
            let val = eval_expr_ov(ctx, overlay, value)?;
            let first_new = pending.len();
            collect_assign_updates(ctx, target, &val, *kind, pending)?;
            if *kind == AssignKind::Blocking {
                for update in &pending[first_new..] {
                    apply_to_overlay(ctx, overlay, update);
                }
            }
            Ok(ExecResult::Continue)
        }

        Statement::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            let cond = eval_expr_ov(ctx, overlay, condition)?;
            if logic_is_true(&cond) {
                exec_stmt_ov(ctx, overlay, then_body, pending, display_output)
            } else if let Some(else_b) = else_body {
                exec_stmt_ov(ctx, overlay, else_b, pending, display_output)
            } else {
                Ok(ExecResult::Continue)
            }
        }

        Statement::Case {
            subject,
            arms,
            default,
            ..
        } => {
            let subj = eval_expr_ov(ctx, overlay, subject)?;
            for arm in arms {
                for pat in &arm.patterns {
                    let pat_val = eval_expr_ov(ctx, overlay, pat)?;
                    if values_equal(&subj, &pat_val) {
                        return exec_stmt_ov(ctx, overlay, &arm.body, pending, display_output);
                    }
                }
            }
            if let Some(def) = default {
                exec_stmt_ov(ctx, overlay, def, pending, display_output)
            } else {
                Ok(ExecResult::Continue)
            }
        }

        Statement::Block { stmts, span } => {
            for (i, s) in stmts.iter().enumerate() {
                let result = exec_stmt_ov(ctx, overlay, s, pending, display_output)?;
                match result {
                    ExecResult::Finish => return Ok(ExecResult::Finish),
                    ExecResult::Suspend {
                        delay_fs,
                        continuation,
                    } => {
                        // Build continuation: remaining stmts after the current one
                        let remaining: Vec<Statement> = stmts[i + 1..].to_vec();
                        if remaining.is_empty() {
                            return Ok(ExecResult::Suspend {
                                delay_fs,
                                continuation,
                            });
                        }
                        // Wrap: execute the delay's body, then the remaining stmts
                        let mut combined = vec![*continuation];
                        combined.extend(remaining);
                        let cont = Statement::Block {
                            stmts: combined,
                            span: *span,
                        };
                        return Ok(ExecResult::Suspend {
                            delay_fs,
                            continuation: Box::new(cont),
                        });
                    }
                    ExecResult::Continue => {}
                }
            }
            Ok(ExecResult::Continue)
        }

        Statement::Wait { .. } => {
            // Wait is handled by the kernel (scheduling); here we just continue
            Ok(ExecResult::Continue)
        }

        Statement::Assertion {
            kind,
            condition,
            message,
            ..
        } => {
            let cond = eval_expr_ov(ctx, overlay, condition)?;
            match kind {
                AssertionKind::Assert => {
                    if !logic_is_true(&cond) {
                        let msg = message.clone().unwrap_or_else(|| "assertion failed".into());
                        display_output.push(format!("ASSERTION FAILED: {msg}"));
                    }
                }
                AssertionKind::Assume | AssertionKind::Cover => {
                    // Assume and cover are formal verification constructs; skip in sim
                }
            }
            Ok(ExecResult::Continue)
        }

        Statement::Display { format, args, .. } => {
            let mut evaluated_args: Vec<LogicVec> = Vec::new();
            for arg in args {
                evaluated_args.push(eval_expr_ov(ctx, overlay, arg)?);
            }
            let output = format_display(format, &evaluated_args);
            display_output.push(output);
            Ok(ExecResult::Continue)
        }

        Statement::Delay {
            duration_fs, body, ..
        } => Ok(ExecResult::Suspend {
            delay_fs: *duration_fs,
            continuation: body.clone(),
        }),

        Statement::Forever { body, span } => {
            // Execute the body once; if it suspends (has a delay), wrap the
            // continuation back into the forever loop for re-entry
            let result = exec_stmt_ov(ctx, overlay, body, pending, display_output)?;
            match result {
                ExecResult::Suspend {
                    delay_fs,
                    continuation,
                } => {
                    // After the delay, execute the continuation then re-enter forever
                    let reentry = Statement::Forever {
                        body: body.clone(),
                        span: *span,
                    };
                    let cont = Statement::Block {
                        stmts: vec![*continuation, reentry],
                        span: *span,
                    };
                    Ok(ExecResult::Suspend {
                        delay_fs,
                        continuation: Box::new(cont),
                    })
                }
                ExecResult::Finish => Ok(ExecResult::Finish),
                ExecResult::Continue => {
                    // The body finished without suspending. Signals only change
                    // when this process yields, so every further iteration
                    // would be identical: a zero-time infinite loop. Never
                    // silently fall through (the old behaviour ran the body
                    // once and continued, producing a wrong result).
                    if contains_wait(body) {
                        Err(SimError::Unsupported {
                            reason: "`wait` inside a `forever` loop with no delay; \
                                     wait statements are not yet supported by the \
                                     simulator"
                                .into(),
                        })
                    } else {
                        Err(SimError::ZeroTimeLoop)
                    }
                }
            }
        }

        Statement::Finish { .. } => Ok(ExecResult::Finish),

        Statement::Nop => Ok(ExecResult::Continue),
    }
}

/// Returns `true` if `stmt` contains a [`Statement::Wait`] anywhere inside it.
///
/// The simulator executes `Wait` as a no-op, so a `forever` body that
/// contains one but never reaches a delay is reported as an unsupported
/// construct rather than as a zero-time loop in the user's design.
fn contains_wait(stmt: &Statement) -> bool {
    match stmt {
        Statement::Wait { .. } => true,
        Statement::Block { stmts, .. } => stmts.iter().any(contains_wait),
        Statement::If {
            then_body,
            else_body,
            ..
        } => contains_wait(then_body) || else_body.as_deref().is_some_and(contains_wait),
        Statement::Case { arms, default, .. } => {
            arms.iter().any(|a| contains_wait(&a.body))
                || default.as_deref().is_some_and(contains_wait)
        }
        Statement::Delay { body, .. } | Statement::Forever { body, .. } => contains_wait(body),
        Statement::Assign { .. }
        | Statement::Assertion { .. }
        | Statement::Display { .. }
        | Statement::Finish { .. }
        | Statement::Nop => false,
    }
}

/// Returns `current` with a write of `value` applied, keeping the width of
/// `current` (the signal's declared width).
///
/// - A full write (`range == None`) is resized to the signal: a narrower
///   value is zero-extended and a wider one truncated, as for a Verilog
///   assignment of an unsigned value.
/// - A partial write (`Some((high, low))`) replaces only bits `low..=high`;
///   bits outside the signal are ignored instead of panicking.
///
/// The overlay, the kernel's update merging and the immediate commit of
/// initial-block writes all use this, so a process reads exactly what is
/// later committed.
pub fn apply_write(current: &LogicVec, value: &LogicVec, range: Option<(u32, u32)>) -> LogicVec {
    let mut new_val = current.clone();
    match range {
        None => {
            for i in 0..new_val.width() {
                let bit = if i < value.width() {
                    value.get(i)
                } else {
                    Logic::Zero
                };
                new_val.set(i, bit);
            }
        }
        Some((high, low)) => {
            for i in 0..(high.saturating_sub(low) + 1) {
                if i < value.width() && low + i < new_val.width() {
                    new_val.set(low + i, value.get(i));
                }
            }
        }
    }
    new_val
}

/// Applies a blocking update to the process-local overlay, so later reads in
/// the same segment see it (with the same result the kernel later commits;
/// see [`apply_write`]).
fn apply_to_overlay(ctx: &EvalContext<'_>, overlay: &mut BlockingOverlay, update: &PendingUpdate) {
    let current = read_signal(ctx, overlay, update.target);
    let new_val = apply_write(current, &update.value, update.range);
    overlay.insert(update.target, new_val);
}

/// Collects pending updates of the given `kind` for a signal assignment
/// target.
fn collect_assign_updates(
    ctx: &EvalContext<'_>,
    target: &SignalRef,
    value: &LogicVec,
    kind: AssignKind,
    pending: &mut Vec<PendingUpdate>,
) -> Result<(), SimError> {
    match target {
        SignalRef::Signal(sig_id) => {
            let sim_id = ctx
                .signal_map
                .get(sig_id)
                .ok_or_else(|| SimError::InvalidSignalRef {
                    reason: format!("unmapped signal ID {}", sig_id.as_raw()),
                })?;
            pending.push(PendingUpdate {
                target: *sim_id,
                value: value.clone(),
                range: None,
                kind,
            });
            Ok(())
        }
        SignalRef::Slice { signal, high, low } => {
            let sim_id = ctx
                .signal_map
                .get(signal)
                .ok_or_else(|| SimError::InvalidSignalRef {
                    reason: format!("unmapped signal ID {}", signal.as_raw()),
                })?;
            pending.push(PendingUpdate {
                target: *sim_id,
                value: value.clone(),
                range: Some((*high, *low)),
                kind,
            });
            Ok(())
        }
        SignalRef::Concat(refs) => {
            // Assign to concatenation: split value across parts (LSB-first)
            let mut offset = 0u32;
            for r in refs.iter().rev() {
                let part_width = signal_ref_width(ctx, r)?;
                let mut part_val = LogicVec::new(part_width);
                for i in 0..part_width {
                    if offset + i < value.width() {
                        part_val.set(i, value.get(offset + i));
                    }
                }
                collect_assign_updates(ctx, r, &part_val, kind, pending)?;
                offset += part_width;
            }
            Ok(())
        }
        SignalRef::Const(_) => {
            // Assigning to a constant is a no-op
            Ok(())
        }
    }
}

/// Computes the width of a signal reference.
fn signal_ref_width(ctx: &EvalContext<'_>, signal_ref: &SignalRef) -> Result<u32, SimError> {
    match signal_ref {
        SignalRef::Signal(sig_id) => {
            let sim_id = ctx
                .signal_map
                .get(sig_id)
                .ok_or_else(|| SimError::InvalidSignalRef {
                    reason: format!("unmapped signal ID {}", sig_id.as_raw()),
                })?;
            Ok(ctx.signals.get(*sim_id).width)
        }
        SignalRef::Slice { high, low, .. } => Ok(high - low + 1),
        SignalRef::Concat(refs) => {
            let mut total = 0u32;
            for r in refs {
                total += signal_ref_width(ctx, r)?;
            }
            Ok(total)
        }
        SignalRef::Const(lv) => Ok(lv.width()),
    }
}

/// Formats a `$display` string with evaluated arguments.
fn format_display(format: &str, args: &[LogicVec]) -> String {
    let mut result = String::new();
    let mut arg_idx = 0;
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&spec) = chars.peek() {
                chars.next();
                if arg_idx < args.len() {
                    match spec {
                        'd' | 'D' => {
                            if let Some(v) = args[arg_idx].to_u64() {
                                result.push_str(&v.to_string());
                            } else {
                                result.push('x');
                            }
                        }
                        'b' | 'B' => {
                            result.push_str(&format!("{}", args[arg_idx]));
                        }
                        'h' | 'H' | 'x' | 'X' => {
                            if let Some(v) = args[arg_idx].to_u64() {
                                result.push_str(&format!("{v:x}"));
                            } else {
                                result.push('x');
                            }
                        }
                        '%' => {
                            result.push('%');
                            continue; // Don't consume an arg
                        }
                        _ => {
                            result.push('%');
                            result.push(spec);
                        }
                    }
                    arg_idx += 1;
                } else {
                    result.push('%');
                    result.push(spec);
                }
            } else {
                result.push('%');
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Checks if a LogicVec contains any X or Z values.
fn has_xz(lv: &LogicVec) -> bool {
    (0..lv.width()).any(|i| matches!(lv.get(i), Logic::X | Logic::Z))
}

/// Creates an all-X LogicVec of the given width.
fn all_x(width: u32) -> LogicVec {
    let mut v = LogicVec::new(width);
    for i in 0..width {
        v.set(i, Logic::X);
    }
    v
}

/// Arithmetic operation helper: converts both operands to u64, applies op.
fn arith_op(
    lhs: &LogicVec,
    rhs: &LogicVec,
    op: impl Fn(u64, u64) -> u64,
) -> Result<LogicVec, SimError> {
    let width = lhs.width().max(rhs.width());
    match (lhs.to_u64(), rhs.to_u64()) {
        (Some(a), Some(b)) => Ok(LogicVec::from_u64(op(a, b), width)),
        _ => Ok(all_x(width)),
    }
}

/// Comparison operation helper: converts both to u64, applies comparison.
fn cmp_op(
    lhs: &LogicVec,
    rhs: &LogicVec,
    op: impl Fn(u64, u64) -> bool,
) -> Result<LogicVec, SimError> {
    match (lhs.to_u64(), rhs.to_u64()) {
        (Some(a), Some(b)) => Ok(LogicVec::from_bool(op(a, b))),
        _ => {
            let mut r = LogicVec::new(1);
            r.set(0, Logic::X);
            Ok(r)
        }
    }
}

/// Width-matches two LogicVecs by zero-extending the shorter one.
fn match_widths(a: &LogicVec, b: &LogicVec) -> (LogicVec, LogicVec) {
    let w = a.width().max(b.width());
    (zero_extend(a, w), zero_extend(b, w))
}

/// Zero-extends a LogicVec to the target width.
fn zero_extend(v: &LogicVec, target_width: u32) -> LogicVec {
    if v.width() == target_width {
        return v.clone();
    }
    let mut result = LogicVec::new(target_width);
    for i in 0..v.width() {
        result.set(i, v.get(i));
    }
    result
}

/// Compares two LogicVecs for equality (bit-by-bit, X/Z-aware).
fn values_equal(a: &LogicVec, b: &LogicVec) -> bool {
    match (a.to_u64(), b.to_u64()) {
        (Some(av), Some(bv)) => av == bv,
        _ => false, // X/Z in either → no match
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_ir::{Arena, SignalId, TypeDb};
    use aion_source::Span;

    fn make_ctx_with_signals<'a>(
        signals: &'a mut Arena<SimSignalId, SimSignalState>,
        map: &'a HashMap<SignalId, SimSignalId>,
        types: &'a TypeDb,
    ) -> EvalContext<'a> {
        EvalContext {
            signals,
            signal_map: map,
            types,
        }
    }

    fn setup_one_signal(
        value: LogicVec,
    ) -> (
        Arena<SimSignalId, SimSignalState>,
        HashMap<SignalId, SimSignalId>,
        TypeDb,
    ) {
        let mut signals = Arena::<SimSignalId, SimSignalState>::new();
        let width = value.width();
        let sim_id = signals.alloc(SimSignalState::new("test".into(), width, value));
        let mut map = HashMap::new();
        map.insert(SignalId::from_raw(0), sim_id);
        let types = TypeDb::new();
        (signals, map, types)
    }

    // ---- eval_expr tests ----

    #[test]
    fn eval_literal() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Literal(LogicVec::from_u64(42, 8));
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(42));
    }

    #[test]
    fn eval_signal_ref_full() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::from_u64(0b1010, 4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)));
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(0b1010));
    }

    #[test]
    fn eval_signal_ref_slice() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::from_u64(0b11001010, 8));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Signal(SignalRef::Slice {
            signal: SignalId::from_raw(0),
            high: 3,
            low: 0,
        });
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(0b1010));
        assert_eq!(result.width(), 4);
    }

    #[test]
    fn eval_signal_ref_const() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Signal(SignalRef::Const(LogicVec::from_u64(7, 4)));
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(7));
    }

    #[test]
    fn eval_unary_not() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expr::Literal(LogicVec::from_u64(0b1010, 4))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(0b0101));
    }

    #[test]
    fn eval_unary_neg() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Unary {
            op: UnaryOp::Neg,
            operand: Box::new(Expr::Literal(LogicVec::from_u64(5, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        // -5 in 8-bit wrapping = 251
        assert_eq!(result.to_u64(), Some((-5i64 as u64) & 0xFF));
    }

    #[test]
    fn eval_unary_red_and() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Unary {
            op: UnaryOp::RedAnd,
            operand: Box::new(Expr::Literal(LogicVec::all_one(4))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(1));
    }

    #[test]
    fn eval_unary_red_or() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Unary {
            op: UnaryOp::RedOr,
            operand: Box::new(Expr::Literal(LogicVec::from_u64(0b1000, 4))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(1));
    }

    #[test]
    fn eval_unary_logic_not() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        // !0 == 1
        let expr = Expr::Unary {
            op: UnaryOp::LogicNot,
            operand: Box::new(Expr::Literal(LogicVec::all_zero(4))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(1));
    }

    #[test]
    fn eval_binary_add() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(3, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(4, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(7));
    }

    #[test]
    fn eval_binary_sub() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Sub,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(10, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(3, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(7));
    }

    #[test]
    fn eval_binary_mul() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(6, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(7, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(42));
    }

    #[test]
    fn eval_binary_div() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Div,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(42, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(6, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(7));
    }

    #[test]
    fn eval_binary_div_by_zero() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Div,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(42, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(0, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        assert!(matches!(
            eval_expr(&ctx, &expr),
            Err(SimError::DivisionByZero)
        ));
    }

    #[test]
    fn eval_binary_eq() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(5, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(5, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(1));
    }

    #[test]
    fn eval_binary_ne() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Ne,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(5, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(3, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(1));
    }

    #[test]
    fn eval_binary_lt() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Lt,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(3, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(5, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(1));
    }

    #[test]
    fn eval_binary_shl() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::Shl,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(1, 8))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(3, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(8));
    }

    #[test]
    fn eval_binary_bitwise_and() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::And,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(0b1100, 4))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(0b1010, 4))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(0b1000));
    }

    #[test]
    fn eval_binary_logic_and() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Binary {
            op: BinaryOp::LogicAnd,
            lhs: Box::new(Expr::Literal(LogicVec::from_u64(1, 1))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(1, 1))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(1));
    }

    #[test]
    fn eval_binary_xz_propagation() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let mut x_val = LogicVec::new(4);
        x_val.set(0, Logic::X);
        let expr = Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(Expr::Literal(x_val)),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(5, 4))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        // X propagation → all X
        for i in 0..result.width() {
            assert_eq!(result.get(i), Logic::X);
        }
    }

    #[test]
    fn eval_ternary_true() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Ternary {
            condition: Box::new(Expr::Literal(LogicVec::from_bool(true))),
            true_val: Box::new(Expr::Literal(LogicVec::from_u64(10, 8))),
            false_val: Box::new(Expr::Literal(LogicVec::from_u64(20, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(10));
    }

    #[test]
    fn eval_ternary_false() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Ternary {
            condition: Box::new(Expr::Literal(LogicVec::from_bool(false))),
            true_val: Box::new(Expr::Literal(LogicVec::from_u64(10, 8))),
            false_val: Box::new(Expr::Literal(LogicVec::from_u64(20, 8))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(20));
    }

    #[test]
    fn eval_concat() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Concat(vec![
            Expr::Literal(LogicVec::from_u64(0b11, 2)), // MSB
            Expr::Literal(LogicVec::from_u64(0b00, 2)), // LSB
        ]);
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.width(), 4);
        assert_eq!(result.to_u64(), Some(0b1100));
    }

    #[test]
    fn eval_repeat() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Repeat {
            expr: Box::new(Expr::Literal(LogicVec::from_u64(0b10, 2))),
            count: 3,
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.width(), 6);
        assert_eq!(result.to_u64(), Some(0b101010));
    }

    #[test]
    fn eval_index() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Index {
            expr: Box::new(Expr::Literal(LogicVec::from_u64(0b1010, 4))),
            index: Box::new(Expr::Literal(LogicVec::from_u64(1, 4))),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.to_u64(), Some(1)); // bit 1 of 0b1010 is 1
    }

    #[test]
    fn eval_slice() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let expr = Expr::Slice {
            expr: Box::new(Expr::Literal(LogicVec::from_u64(0b11001010, 8))),
            high: Box::new(Expr::Literal(LogicVec::from_u64(5, 4))),
            low: Box::new(Expr::Literal(LogicVec::from_u64(2, 4))),
            span: Span::DUMMY,
        };
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(result.width(), 4);
        // bits [5:2] of 0b11001010 = 0b0010
        assert_eq!(result.to_u64(), Some(0b0010));
    }

    // ---- exec_statement tests ----

    #[test]
    fn exec_assign() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Assign {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Literal(LogicVec::from_u64(0b1010, 4)),
            kind: AssignKind::Blocking,
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert!(matches!(result, ExecResult::Continue));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].value.to_u64(), Some(0b1010));
    }

    #[test]
    fn exec_if_true_branch() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::If {
            condition: Expr::Literal(LogicVec::from_bool(true)),
            then_body: Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(1, 4)),
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            }),
            else_body: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(2, 4)),
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            })),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].value.to_u64(), Some(1));
    }

    #[test]
    fn exec_if_false_branch() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::If {
            condition: Expr::Literal(LogicVec::from_bool(false)),
            then_body: Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(1, 4)),
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            }),
            else_body: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(2, 4)),
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            })),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].value.to_u64(), Some(2));
    }

    #[test]
    fn exec_case_match() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Case {
            subject: Expr::Literal(LogicVec::from_u64(2, 4)),
            arms: vec![
                aion_ir::CaseArm {
                    patterns: vec![Expr::Literal(LogicVec::from_u64(1, 4))],
                    body: Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(0)),
                        value: Expr::Literal(LogicVec::from_u64(10, 4)),
                        kind: AssignKind::Blocking,
                        span: Span::DUMMY,
                    },
                    span: Span::DUMMY,
                },
                aion_ir::CaseArm {
                    patterns: vec![Expr::Literal(LogicVec::from_u64(2, 4))],
                    body: Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(0)),
                        value: Expr::Literal(LogicVec::from_u64(20, 8)),
                        kind: AssignKind::Blocking,
                        span: Span::DUMMY,
                    },
                    span: Span::DUMMY,
                },
            ],
            default: None,
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].value.to_u64(), Some(20));
    }

    #[test]
    fn exec_case_default() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Case {
            subject: Expr::Literal(LogicVec::from_u64(99, 8)),
            arms: vec![aion_ir::CaseArm {
                patterns: vec![Expr::Literal(LogicVec::from_u64(1, 8))],
                body: Statement::Nop,
                span: Span::DUMMY,
            }],
            default: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(0, 4)),
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            })),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].value.to_u64(), Some(0));
    }

    #[test]
    fn exec_block() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Block {
            stmts: vec![
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_u64(1, 4)),
                    kind: AssignKind::Blocking,
                    span: Span::DUMMY,
                },
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_u64(2, 4)),
                    kind: AssignKind::Blocking,
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn exec_finish() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Finish { span: Span::DUMMY };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert!(matches!(result, ExecResult::Finish));
    }

    #[test]
    fn exec_display() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Display {
            format: "count = %d".into(),
            args: vec![Expr::Literal(LogicVec::from_u64(42, 8))],
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(display.len(), 1);
        assert_eq!(display[0], "count = 42");
    }

    #[test]
    fn exec_assertion_pass() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Assertion {
            kind: AssertionKind::Assert,
            condition: Expr::Literal(LogicVec::from_bool(true)),
            message: Some("should pass".into()),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert!(display.is_empty()); // No failure output
    }

    #[test]
    fn exec_assertion_fail() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Assertion {
            kind: AssertionKind::Assert,
            condition: Expr::Literal(LogicVec::from_bool(false)),
            message: Some("count != 5".into()),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(display.len(), 1);
        assert!(display[0].contains("count != 5"));
    }

    #[test]
    fn exec_nop() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &Statement::Nop, &mut pending, &mut display).unwrap();
        assert!(matches!(result, ExecResult::Continue));
        assert!(pending.is_empty());
    }

    #[test]
    fn exec_block_with_finish_stops() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Block {
            stmts: vec![
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_u64(1, 4)),
                    kind: AssignKind::Blocking,
                    span: Span::DUMMY,
                },
                Statement::Finish { span: Span::DUMMY },
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_u64(2, 4)),
                    kind: AssignKind::Blocking,
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert!(matches!(result, ExecResult::Finish));
        assert_eq!(pending.len(), 1); // Only first assign
    }

    #[test]
    fn logic_is_true_one() {
        assert!(logic_is_true(&LogicVec::from_bool(true)));
    }

    #[test]
    fn logic_is_true_zero() {
        assert!(!logic_is_true(&LogicVec::from_bool(false)));
    }

    #[test]
    fn format_display_decimal() {
        let result = format_display("val=%d", &[LogicVec::from_u64(42, 8)]);
        assert_eq!(result, "val=42");
    }

    #[test]
    fn format_display_binary() {
        let result = format_display("val=%b", &[LogicVec::from_u64(0b101, 3)]);
        assert_eq!(result, "val=101");
    }

    #[test]
    fn format_display_hex() {
        let result = format_display("val=%h", &[LogicVec::from_u64(0xFF, 8)]);
        assert_eq!(result, "val=ff");
    }

    #[test]
    fn eval_unmapped_signal_errors() {
        let signals = Arena::<SimSignalId, SimSignalState>::new();
        let map = HashMap::new();
        let types = TypeDb::new();
        let ctx = EvalContext {
            signals: &signals,
            signal_map: &map,
            types: &types,
        };
        let expr = Expr::Signal(SignalRef::Signal(SignalId::from_raw(99)));
        assert!(matches!(
            eval_expr(&ctx, &expr),
            Err(SimError::InvalidSignalRef { .. })
        ));
    }

    #[test]
    fn exec_assign_slice() {
        let mut signals = Arena::<SimSignalId, SimSignalState>::new();
        let sim_id = signals.alloc(SimSignalState::new("test".into(), 8, LogicVec::new(8)));
        let mut map = HashMap::new();
        map.insert(SignalId::from_raw(0), sim_id);
        let types = TypeDb::new();
        let ctx = EvalContext {
            signals: &signals,
            signal_map: &map,
            types: &types,
        };
        let stmt = Statement::Assign {
            target: SignalRef::Slice {
                signal: SignalId::from_raw(0),
                high: 3,
                low: 0,
            },
            value: Expr::Literal(LogicVec::from_u64(0b1111, 4)),
            kind: AssignKind::Blocking,
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].range.is_some());
        assert_eq!(pending[0].range.unwrap(), (3, 0));
    }

    // ---- delay/forever tests ----

    #[test]
    fn exec_delay_suspends() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Delay {
            duration_fs: 5_000_000,
            body: Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(1, 4)),
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        if let ExecResult::Suspend {
            delay_fs,
            continuation,
        } = result
        {
            assert_eq!(delay_fs, 5_000_000);
            assert!(matches!(*continuation, Statement::Assign { .. }));
        } else {
            panic!("expected Suspend");
        }
        // No pending updates yet (delay hasn't elapsed)
        assert!(pending.is_empty());
    }

    #[test]
    fn exec_forever_with_delay_suspends() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Forever {
            body: Box::new(Statement::Delay {
                duration_fs: 5_000_000,
                body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_u64(1, 4)),
                    kind: AssignKind::Blocking,
                    span: Span::DUMMY,
                }),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        if let ExecResult::Suspend { delay_fs, .. } = result {
            assert_eq!(delay_fs, 5_000_000);
        } else {
            panic!("expected Suspend");
        }
    }

    /// Executes `forever <body>` against a single 4-bit signal.
    fn exec_forever(body: Statement) -> Result<ExecResult, SimError> {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Forever {
            body: Box::new(body),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display)
    }

    fn assign_sig0(value: u64) -> Statement {
        Statement::Assign {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Literal(LogicVec::from_u64(value, 4)),
            kind: AssignKind::Blocking,
            span: Span::DUMMY,
        }
    }

    fn delay_5ns(body: Statement) -> Statement {
        Statement::Delay {
            duration_fs: 5_000_000,
            body: Box::new(body),
            span: Span::DUMMY,
        }
    }

    fn if_literal(cond: bool, then_body: Statement) -> Statement {
        Statement::If {
            condition: Expr::Literal(LogicVec::from_bool(cond)),
            then_body: Box::new(then_body),
            else_body: None,
            span: Span::DUMMY,
        }
    }

    #[test]
    fn exec_forever_empty_body_is_zero_time_loop() {
        assert!(matches!(
            exec_forever(Statement::Nop),
            Err(SimError::ZeroTimeLoop)
        ));
    }

    #[test]
    fn exec_forever_assign_without_delay_is_zero_time_loop() {
        // forever x = 1;  — used to run once and silently continue.
        assert!(matches!(
            exec_forever(assign_sig0(1)),
            Err(SimError::ZeroTimeLoop)
        ));
    }

    #[test]
    fn exec_forever_untaken_delay_branch_is_zero_time_loop() {
        // forever if (0) #5 x = 1;  — the delay exists but is never reached,
        // and the condition cannot change because the loop never yields.
        assert!(matches!(
            exec_forever(if_literal(false, delay_5ns(assign_sig0(1)))),
            Err(SimError::ZeroTimeLoop)
        ));
    }

    #[test]
    fn exec_forever_taken_delay_branch_suspends() {
        let result = exec_forever(if_literal(true, delay_5ns(assign_sig0(1)))).unwrap();
        assert!(matches!(
            result,
            ExecResult::Suspend {
                delay_fs: 5_000_000,
                ..
            }
        ));
    }

    #[test]
    fn exec_forever_finish_without_delay_finishes() {
        // forever $finish;  — terminates, so it is not a zero-time loop.
        let result = exec_forever(Statement::Finish { span: Span::DUMMY }).unwrap();
        assert!(matches!(result, ExecResult::Finish));
    }

    #[test]
    fn exec_forever_wait_without_delay_is_unsupported() {
        let body = Statement::Block {
            stmts: vec![
                Statement::Wait {
                    duration: None,
                    span: Span::DUMMY,
                },
                assign_sig0(1),
            ],
            span: Span::DUMMY,
        };
        match exec_forever(body) {
            Err(SimError::Unsupported { reason }) => assert!(reason.contains("wait")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn contains_wait_finds_nested_wait() {
        let wait = Statement::Wait {
            duration: None,
            span: Span::DUMMY,
        };
        assert!(contains_wait(&wait));
        assert!(contains_wait(&if_literal(true, delay_5ns(wait))));
        assert!(contains_wait(&Statement::Case {
            subject: Expr::Literal(LogicVec::from_u64(0, 4)),
            arms: Vec::new(),
            default: Some(Box::new(Statement::Wait {
                duration: None,
                span: Span::DUMMY,
            })),
            span: Span::DUMMY,
        }));
    }

    #[test]
    fn contains_wait_false_without_wait() {
        assert!(!contains_wait(&Statement::Nop));
        assert!(!contains_wait(&delay_5ns(assign_sig0(1))));
        assert!(!contains_wait(&Statement::Block {
            stmts: vec![assign_sig0(1), Statement::Finish { span: Span::DUMMY }],
            span: Span::DUMMY,
        }));
    }

    #[test]
    fn exec_block_suspends_captures_remaining() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Block {
            stmts: vec![
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_u64(0, 4)),
                    kind: AssignKind::Blocking,
                    span: Span::DUMMY,
                },
                Statement::Delay {
                    duration_fs: 10_000_000,
                    body: Box::new(Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(0)),
                        value: Expr::Literal(LogicVec::from_u64(1, 4)),
                        kind: AssignKind::Blocking,
                        span: Span::DUMMY,
                    }),
                    span: Span::DUMMY,
                },
                Statement::Finish { span: Span::DUMMY },
            ],
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        // First assign should have been executed
        assert_eq!(pending.len(), 1);
        // Should be suspended at the delay
        if let ExecResult::Suspend {
            delay_fs,
            continuation,
        } = result
        {
            assert_eq!(delay_fs, 10_000_000);
            // Continuation should be a block containing the assign body + Finish
            assert!(matches!(*continuation, Statement::Block { .. }));
        } else {
            panic!("expected Suspend");
        }
    }

    #[test]
    fn exec_delay_zero_suspends() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Delay {
            duration_fs: 0,
            body: Box::new(Statement::Nop),
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        if let ExecResult::Suspend { delay_fs, .. } = result {
            assert_eq!(delay_fs, 0);
        } else {
            panic!("expected Suspend even for zero delay");
        }
    }

    #[test]
    fn exec_nested_delay_in_if() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(4));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::If {
            condition: Expr::Literal(LogicVec::from_bool(true)),
            then_body: Box::new(Statement::Delay {
                duration_fs: 1_000_000,
                body: Box::new(Statement::Nop),
                span: Span::DUMMY,
            }),
            else_body: None,
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        if let ExecResult::Suspend { delay_fs, .. } = result {
            assert_eq!(delay_fs, 1_000_000);
        } else {
            panic!("expected Suspend from nested delay");
        }
    }

    #[test]
    fn exec_block_finish_after_delay_unreachable() {
        // When a block hits a delay, it suspends before reaching finish
        let (mut signals, map, types) = setup_one_signal(LogicVec::new(1));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = Statement::Block {
            stmts: vec![
                Statement::Delay {
                    duration_fs: 100,
                    body: Box::new(Statement::Nop),
                    span: Span::DUMMY,
                },
                Statement::Finish { span: Span::DUMMY },
            ],
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        // Should suspend, not finish
        assert!(matches!(result, ExecResult::Suspend { .. }));
    }

    // ---- blocking overlay / non-blocking assignment tests ----

    /// Creates `n` 8-bit signals (IR ids 0..n) with the given initial values.
    fn setup_byte_signals(
        values: &[u64],
    ) -> (
        Arena<SimSignalId, SimSignalState>,
        HashMap<SignalId, SimSignalId>,
        TypeDb,
    ) {
        let mut signals = Arena::<SimSignalId, SimSignalState>::new();
        let mut map = HashMap::new();
        for (i, &v) in values.iter().enumerate() {
            let id = signals.alloc(SimSignalState::new(
                format!("s{i}"),
                8,
                LogicVec::from_u64(v, 8),
            ));
            map.insert(SignalId::from_raw(i as u32), id);
        }
        (signals, map, TypeDb::new())
    }

    fn sig(i: u32) -> SignalRef {
        SignalRef::Signal(SignalId::from_raw(i))
    }

    fn read(i: u32) -> Expr {
        Expr::Signal(sig(i))
    }

    fn assign(target: SignalRef, value: Expr, kind: AssignKind) -> Statement {
        Statement::Assign {
            target,
            value,
            kind,
            span: Span::DUMMY,
        }
    }

    fn block(stmts: Vec<Statement>) -> Statement {
        Statement::Block {
            stmts,
            span: Span::DUMMY,
        }
    }

    /// Runs `stmt` as one process segment, returning its updates and output.
    fn run_segment(ctx: &EvalContext<'_>, stmt: &Statement) -> (Vec<PendingUpdate>, Vec<String>) {
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(ctx, stmt, &mut pending, &mut display).unwrap();
        (pending, display)
    }

    /// The last update to `target` in `pending`, as `(value, kind)`.
    fn last_update(
        pending: &[PendingUpdate],
        map: &HashMap<SignalId, SimSignalId>,
        target: u32,
    ) -> (Option<u64>, AssignKind) {
        let id = map[&SignalId::from_raw(target)];
        let u = pending
            .iter()
            .rev()
            .find(|u| u.target == id)
            .expect("no update for target");
        (u.value.to_u64(), u.kind)
    }

    #[test]
    fn blocking_write_visible_to_next_statement() {
        // SIM-3: `a = 1; b = a;` must give b = 1, not the stale value.
        let (mut signals, map, types) = setup_byte_signals(&[0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(
                sig(0),
                Expr::Literal(LogicVec::from_u64(1, 8)),
                AssignKind::Blocking,
            ),
            assign(sig(1), read(0), AssignKind::Blocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(
            last_update(&pending, &map, 1),
            (Some(1), AssignKind::Blocking)
        );
        // The committed state itself is untouched; the kernel applies updates.
        assert_eq!(
            ctx.signals.get(map[&SignalId::from_raw(0)]).value.to_u64(),
            Some(0)
        );
    }

    #[test]
    fn blocking_temp_chain_computes_from_new_temp() {
        // tmp = x + y; z = tmp;  (x = 3, y = 4)
        let (mut signals, map, types) = setup_byte_signals(&[3, 4, 0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let sum = Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(read(0)),
            rhs: Box::new(read(1)),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        let stmt = block(vec![
            assign(sig(2), sum, AssignKind::Blocking),
            assign(sig(3), read(2), AssignKind::Blocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(last_update(&pending, &map, 3).0, Some(7));
    }

    #[test]
    fn nonblocking_write_not_visible_to_later_reads() {
        // a <= 5; b = a;  → b reads the old a (2), a's update is NBA.
        let (mut signals, map, types) = setup_byte_signals(&[2, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(
                sig(0),
                Expr::Literal(LogicVec::from_u64(5, 8)),
                AssignKind::NonBlocking,
            ),
            assign(sig(1), read(0), AssignKind::Blocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(
            last_update(&pending, &map, 0),
            (Some(5), AssignKind::NonBlocking)
        );
        assert_eq!(
            last_update(&pending, &map, 1),
            (Some(2), AssignKind::Blocking)
        );
    }

    #[test]
    fn nonblocking_swap_samples_old_values() {
        // a <= b; b <= a;  with a = 1, b = 2 → updates a := 2, b := 1.
        let (mut signals, map, types) = setup_byte_signals(&[1, 2]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(sig(0), read(1), AssignKind::NonBlocking),
            assign(sig(1), read(0), AssignKind::NonBlocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(
            last_update(&pending, &map, 0),
            (Some(2), AssignKind::NonBlocking)
        );
        assert_eq!(
            last_update(&pending, &map, 1),
            (Some(1), AssignKind::NonBlocking)
        );
    }

    #[test]
    fn blocking_swap_does_not_swap() {
        // a = b; b = a;  with a = 1, b = 2 → both end up 2.
        let (mut signals, map, types) = setup_byte_signals(&[1, 2]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(sig(0), read(1), AssignKind::Blocking),
            assign(sig(1), read(0), AssignKind::Blocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(last_update(&pending, &map, 0).0, Some(2));
        assert_eq!(last_update(&pending, &map, 1).0, Some(2));
    }

    #[test]
    fn display_after_blocking_assign_shows_new_value() {
        let (mut signals, map, types) = setup_byte_signals(&[0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(
                sig(0),
                Expr::Literal(LogicVec::from_u64(9, 8)),
                AssignKind::Blocking,
            ),
            assign(
                sig(1),
                Expr::Literal(LogicVec::from_u64(4, 8)),
                AssignKind::NonBlocking,
            ),
            Statement::Display {
                format: "a=%d b=%d".into(),
                args: vec![read(0), read(1)],
                span: Span::DUMMY,
            },
        ]);
        let (_, display) = run_segment(&ctx, &stmt);
        // a's blocking write is visible; b's non-blocking write is not yet.
        assert_eq!(display, vec!["a=9 b=0".to_string()]);
    }

    #[test]
    fn blocking_write_steers_later_condition() {
        // a = 1; if (a) b = 7; else b = 8;
        let (mut signals, map, types) = setup_byte_signals(&[0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(
                sig(0),
                Expr::Literal(LogicVec::from_u64(1, 8)),
                AssignKind::Blocking,
            ),
            Statement::If {
                condition: read(0),
                then_body: Box::new(assign(
                    sig(1),
                    Expr::Literal(LogicVec::from_u64(7, 8)),
                    AssignKind::Blocking,
                )),
                else_body: Some(Box::new(assign(
                    sig(1),
                    Expr::Literal(LogicVec::from_u64(8, 8)),
                    AssignKind::Blocking,
                ))),
                span: Span::DUMMY,
            },
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(last_update(&pending, &map, 1).0, Some(7));
    }

    #[test]
    fn blocking_partial_writes_visible_to_later_reads() {
        // x[3:0] = 4'hA; x[7:4] = 4'h5; y = x;  (x starts 0) → y = 0x5A.
        let (mut signals, map, types) = setup_byte_signals(&[0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let slice = |high, low| SignalRef::Slice {
            signal: SignalId::from_raw(0),
            high,
            low,
        };
        let stmt = block(vec![
            assign(
                slice(3, 0),
                Expr::Literal(LogicVec::from_u64(0xA, 4)),
                AssignKind::Blocking,
            ),
            assign(
                slice(7, 4),
                Expr::Literal(LogicVec::from_u64(0x5, 4)),
                AssignKind::Blocking,
            ),
            assign(sig(1), read(0), AssignKind::Blocking),
            // A slice read also goes through the overlay.
            assign(sig(1), Expr::Signal(slice(7, 4)), AssignKind::NonBlocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        let y = map[&SignalId::from_raw(1)];
        let ys: Vec<_> = pending.iter().filter(|u| u.target == y).collect();
        assert_eq!(ys[0].value.to_u64(), Some(0x5A));
        assert_eq!(ys[1].value.to_u64(), Some(0x5));
    }

    #[test]
    fn blocking_concat_target_visible_to_later_reads() {
        // {a, b} = 16'h1234; c = a; d = b;  → c = 0x12, d = 0x34.
        let (mut signals, map, types) = setup_byte_signals(&[0, 0, 0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(
                SignalRef::Concat(vec![sig(0), sig(1)]),
                Expr::Literal(LogicVec::from_u64(0x1234, 16)),
                AssignKind::Blocking,
            ),
            assign(sig(2), read(0), AssignKind::Blocking),
            assign(sig(3), read(1), AssignKind::Blocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(last_update(&pending, &map, 2).0, Some(0x12));
        assert_eq!(last_update(&pending, &map, 3).0, Some(0x34));
    }

    #[test]
    fn narrow_blocking_write_is_zero_extended() {
        // x = 4'hF on an 8-bit x holding 0xA0 → overlay 0x0F: the value is
        // zero-extended to the signal width, exactly as the kernel commits it.
        let (mut signals, map, types) = setup_byte_signals(&[0xA0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(
                sig(0),
                Expr::Literal(LogicVec::from_u64(0xF, 4)),
                AssignKind::Blocking,
            ),
            assign(sig(1), read(0), AssignKind::Blocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(last_update(&pending, &map, 1).0, Some(0x0F));
    }

    #[test]
    fn apply_write_full_resizes_to_signal_width() {
        let current = LogicVec::from_u64(0xA5, 8);
        // Narrower: zero-extended, width kept.
        let narrow = apply_write(&current, &LogicVec::from_u64(1, 1), None);
        assert_eq!((narrow.width(), narrow.to_u64()), (8, Some(1)));
        // Wider: truncated to the signal width.
        let wide = apply_write(&current, &LogicVec::from_u64(0x1FF, 12), None);
        assert_eq!((wide.width(), wide.to_u64()), (8, Some(0xFF)));
    }

    #[test]
    fn apply_write_slice_replaces_only_its_bits() {
        let current = LogicVec::from_u64(0xA5, 8);
        let v = apply_write(&current, &LogicVec::from_u64(0x3, 4), Some((7, 4)));
        assert_eq!(v.to_u64(), Some(0x35));
        // Out-of-range bits are ignored rather than panicking.
        let v = apply_write(&current, &LogicVec::from_u64(0xF, 4), Some((9, 6)));
        assert_eq!(v.to_u64(), Some(0xE5));
    }

    #[test]
    fn out_of_range_blocking_slice_is_ignored_in_overlay() {
        // x[11:8] = 4'hF on an 8-bit x must not panic; x reads unchanged.
        let (mut signals, map, types) = setup_byte_signals(&[0x12, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(
                SignalRef::Slice {
                    signal: SignalId::from_raw(0),
                    high: 11,
                    low: 8,
                },
                Expr::Literal(LogicVec::from_u64(0xF, 4)),
                AssignKind::Blocking,
            ),
            assign(sig(1), read(0), AssignKind::Blocking),
        ]);
        let (pending, _) = run_segment(&ctx, &stmt);
        assert_eq!(last_update(&pending, &map, 1).0, Some(0x12));
    }

    #[test]
    fn overlay_does_not_survive_between_segments() {
        // Each exec_statement call is a fresh segment: without the kernel
        // committing the first segment's writes, the second sees old state.
        let (mut signals, map, types) = setup_byte_signals(&[0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let first = assign(
            sig(0),
            Expr::Literal(LogicVec::from_u64(3, 8)),
            AssignKind::Blocking,
        );
        let second = assign(sig(1), read(0), AssignKind::Blocking);
        run_segment(&ctx, &first);
        let (pending, _) = run_segment(&ctx, &second);
        assert_eq!(last_update(&pending, &map, 1).0, Some(0));
        // eval_expr (no process context) reads committed state only.
        assert_eq!(eval_expr(&ctx, &read(0)).unwrap().to_u64(), Some(0));
    }

    #[test]
    fn blocking_writes_before_delay_are_returned_for_commit() {
        // a = 4; #1 b = a;  → the segment suspends; a's update is returned
        // for the kernel to commit, and b's assignment is in the continuation.
        let (mut signals, map, types) = setup_byte_signals(&[0, 0]);
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let stmt = block(vec![
            assign(
                sig(0),
                Expr::Literal(LogicVec::from_u64(4, 8)),
                AssignKind::Blocking,
            ),
            Statement::Delay {
                duration_fs: 1,
                body: Box::new(assign(sig(1), read(0), AssignKind::Blocking)),
                span: Span::DUMMY,
            },
        ]);
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert!(matches!(result, ExecResult::Suspend { .. }));
        assert_eq!(pending.len(), 1);
        assert_eq!(
            last_update(&pending, &map, 0),
            (Some(4), AssignKind::Blocking)
        );
    }
}
