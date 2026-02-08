//! Expression evaluator and statement executor for the simulation kernel.
//!
//! [`eval_expr`] recursively evaluates an IR [`Expr`] tree into a [`LogicVec`],
//! reading signal values from the simulation state. [`exec_statement`] executes
//! a [`Statement`] tree, collecting deferred [`PendingUpdate`]s for the kernel
//! to apply after the process completes.

use std::collections::HashMap;

use aion_common::{Logic, LogicVec};
use aion_ir::arena::Arena;
use aion_ir::{AssertionKind, BinaryOp, Expr, SignalId, SignalRef, Statement, TypeDb, UnaryOp};

use crate::error::SimError;
use crate::value::{SimSignalId, SimSignalState};

/// A deferred signal update collected during statement execution.
///
/// Updates are not applied immediately; they are batched and applied
/// after the entire process body finishes executing, preserving
/// delta-cycle semantics.
#[derive(Debug, Clone)]
pub struct PendingUpdate {
    /// The flat simulation signal to update.
    pub target: SimSignalId,
    /// The new value to drive.
    pub value: LogicVec,
    /// Optional bit range for partial updates `(high, low)` inclusive.
    pub range: Option<(u32, u32)>,
}

/// The result of executing a single statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecResult {
    /// Continue executing subsequent statements.
    Continue,
    /// Stop simulation (`$finish` was encountered).
    Finish,
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

/// Evaluates an IR expression into a `LogicVec`.
///
/// Recursively walks the expression tree, reading signal values from the
/// evaluation context. Operations involving X or Z values propagate
/// unknown results following IEEE 1364 semantics.
pub fn eval_expr(ctx: &EvalContext<'_>, expr: &Expr) -> Result<LogicVec, SimError> {
    match expr {
        Expr::Literal(lv) => Ok(lv.clone()),

        Expr::Signal(signal_ref) => eval_signal_ref(ctx, signal_ref),

        Expr::Unary { op, operand, .. } => {
            let val = eval_expr(ctx, operand)?;
            eval_unary(*op, &val)
        }

        Expr::Binary { op, lhs, rhs, .. } => {
            let l = eval_expr(ctx, lhs)?;
            let r = eval_expr(ctx, rhs)?;
            eval_binary(*op, &l, &r)
        }

        Expr::Ternary {
            condition,
            true_val,
            false_val,
            ..
        } => {
            let cond = eval_expr(ctx, condition)?;
            if has_xz(&cond) {
                // Unknown condition → all X result
                let tv = eval_expr(ctx, true_val)?;
                let mut result = LogicVec::new(tv.width());
                for i in 0..tv.width() {
                    result.set(i, Logic::X);
                }
                Ok(result)
            } else if logic_is_true(&cond) {
                eval_expr(ctx, true_val)
            } else {
                eval_expr(ctx, false_val)
            }
        }

        Expr::Concat(parts) => {
            let mut evaluated: Vec<LogicVec> = Vec::with_capacity(parts.len());
            for p in parts {
                evaluated.push(eval_expr(ctx, p)?);
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
            let val = eval_expr(ctx, expr)?;
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
            let val = eval_expr(ctx, expr)?;
            let idx_val = eval_expr(ctx, index)?;
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
            let val = eval_expr(ctx, expr)?;
            let h_val = eval_expr(ctx, high)?;
            let l_val = eval_expr(ctx, low)?;
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

/// Evaluates a `SignalRef` to its current `LogicVec` value.
fn eval_signal_ref(ctx: &EvalContext<'_>, signal_ref: &SignalRef) -> Result<LogicVec, SimError> {
    match signal_ref {
        SignalRef::Signal(sig_id) => {
            let sim_id = ctx
                .signal_map
                .get(sig_id)
                .ok_or_else(|| SimError::InvalidSignalRef {
                    reason: format!("unmapped signal ID {}", sig_id.as_raw()),
                })?;
            Ok(ctx.signals.get(*sim_id).value.clone())
        }
        SignalRef::Slice { signal, high, low } => {
            let sim_id = ctx
                .signal_map
                .get(signal)
                .ok_or_else(|| SimError::InvalidSignalRef {
                    reason: format!("unmapped signal ID {}", signal.as_raw()),
                })?;
            let full = &ctx.signals.get(*sim_id).value;
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
                parts.push(eval_signal_ref(ctx, r)?);
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
/// Returns `ExecResult::Finish` if a `$finish` is encountered.
pub fn exec_statement(
    ctx: &EvalContext<'_>,
    stmt: &Statement,
    pending: &mut Vec<PendingUpdate>,
    display_output: &mut Vec<String>,
) -> Result<ExecResult, SimError> {
    match stmt {
        Statement::Assign { target, value, .. } => {
            let val = eval_expr(ctx, value)?;
            collect_assign_updates(ctx, target, &val, pending)?;
            Ok(ExecResult::Continue)
        }

        Statement::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            let cond = eval_expr(ctx, condition)?;
            if logic_is_true(&cond) {
                exec_statement(ctx, then_body, pending, display_output)
            } else if let Some(else_b) = else_body {
                exec_statement(ctx, else_b, pending, display_output)
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
            let subj = eval_expr(ctx, subject)?;
            for arm in arms {
                for pat in &arm.patterns {
                    let pat_val = eval_expr(ctx, pat)?;
                    if values_equal(&subj, &pat_val) {
                        return exec_statement(ctx, &arm.body, pending, display_output);
                    }
                }
            }
            if let Some(def) = default {
                exec_statement(ctx, def, pending, display_output)
            } else {
                Ok(ExecResult::Continue)
            }
        }

        Statement::Block { stmts, .. } => {
            for s in stmts {
                let result = exec_statement(ctx, s, pending, display_output)?;
                if result == ExecResult::Finish {
                    return Ok(ExecResult::Finish);
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
            let cond = eval_expr(ctx, condition)?;
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
                evaluated_args.push(eval_expr(ctx, arg)?);
            }
            let output = format_display(format, &evaluated_args);
            display_output.push(output);
            Ok(ExecResult::Continue)
        }

        Statement::Finish { .. } => Ok(ExecResult::Finish),

        Statement::Nop => Ok(ExecResult::Continue),
    }
}

/// Collects pending updates for a signal assignment target.
fn collect_assign_updates(
    ctx: &EvalContext<'_>,
    target: &SignalRef,
    value: &LogicVec,
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
                collect_assign_updates(ctx, r, &part_val, pending)?;
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
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(result, ExecResult::Continue);
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
                span: Span::DUMMY,
            }),
            else_body: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(2, 4)),
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
                span: Span::DUMMY,
            }),
            else_body: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_u64(2, 4)),
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
                        span: Span::DUMMY,
                    },
                    span: Span::DUMMY,
                },
                aion_ir::CaseArm {
                    patterns: vec![Expr::Literal(LogicVec::from_u64(2, 4))],
                    body: Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(0)),
                        value: Expr::Literal(LogicVec::from_u64(20, 8)),
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
                    span: Span::DUMMY,
                },
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_u64(2, 4)),
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
        assert_eq!(result, ExecResult::Finish);
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
        assert_eq!(result, ExecResult::Continue);
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
                    span: Span::DUMMY,
                },
                Statement::Finish { span: Span::DUMMY },
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_u64(2, 4)),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        let result = exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(result, ExecResult::Finish);
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
            span: Span::DUMMY,
        };
        let mut pending = Vec::new();
        let mut display = Vec::new();
        exec_statement(&ctx, &stmt, &mut pending, &mut display).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].range.is_some());
        assert_eq!(pending[0].range.unwrap(), (3, 0));
    }
}
