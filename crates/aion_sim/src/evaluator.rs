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

use crate::arith::Bits;
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

/// Returns `true` if a condition value is definitely true: at least one bit
/// is `Logic::One`, so the value is a known nonzero number whatever its
/// other bits are.
///
/// An all-zero value is false, and so is a value whose only non-zero bits
/// are X/Z (IEEE 1364-2005 9.4: an `if` whose condition is 0, x or z takes
/// the `else` branch). Every bit counts, not just the LSB: `if (state)` with
/// `state == 2'b10` is true.
pub fn logic_is_true(lv: &LogicVec) -> bool {
    truth_value(lv) == Logic::One
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
/// The expression is evaluated *self-determined* (IEEE 1364-2005 5.4.1): the
/// result has the expression's own width, and context-determined operands
/// inside it are first extended to that width (sign-extended when every
/// operand is signed, zero-extended otherwise). Operations involving X or Z
/// values propagate unknown results following IEEE 1364 semantics; for
/// example `x / 0` is all-X rather than an error. Arithmetic, shifts and
/// comparisons work at any width, including above 64 bits.
pub fn eval_expr(ctx: &EvalContext<'_>, expr: &Expr) -> Result<LogicVec, SimError> {
    eval_self(ctx, &BlockingOverlay::new(), expr)
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

/// The largest vector width, in bits, that the evaluator builds for a
/// concatenation, replication or part-select (16 Mi bits, 4 MiB of storage).
///
/// Wider results come from corrupt or absurd IR (for example a replication
/// count near `u32::MAX`) and are reported as [`SimError::EvalError`]
/// instead of exhausting memory.
const MAX_EVAL_WIDTH: u32 = 1 << 24;

/// The width and signedness of an expression, as defined by the Verilog
/// expression sizing and typing rules (IEEE 1364-2005 5.4 and 5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExprType {
    /// The width in bits.
    width: u32,
    /// Whether the expression is signed (every context operand is signed).
    signed: bool,
}

impl ExprType {
    /// The type of a 1-bit unsigned result (comparisons, reductions, logical
    /// operators, bit-selects).
    const BIT: ExprType = ExprType {
        width: 1,
        signed: false,
    };

    /// Returns an unsigned type of `width` bits.
    fn unsigned(width: u32) -> Self {
        Self {
            width,
            signed: false,
        }
    }

    /// Combines the types of two context-determined operands: the wider
    /// width, signed only if both are signed.
    fn merge(self, other: Self) -> Self {
        Self {
            width: self.width.max(other.width),
            signed: self.signed && other.signed,
        }
    }
}

/// Returns `width`, or an evaluation error if it is missing (overflowed) or
/// exceeds [`MAX_EVAL_WIDTH`].
fn check_width(width: Option<u32>, what: &str) -> Result<u32, SimError> {
    match width {
        Some(w) if w <= MAX_EVAL_WIDTH => Ok(w),
        _ => Err(SimError::EvalError {
            reason: format!("{what} is wider than {MAX_EVAL_WIDTH} bits"),
        }),
    }
}

/// Computes the self-determined type of `expr` (its width and signedness
/// without any surrounding context).
fn expr_type(
    ctx: &EvalContext<'_>,
    ov: &BlockingOverlay,
    expr: &Expr,
) -> Result<ExprType, SimError> {
    Ok(match expr {
        Expr::Literal(lv) | Expr::NamedConst { value: lv, .. } => ExprType::unsigned(lv.width()),
        Expr::SignedLiteral { value, sized } => signed_literal_type(value, *sized),
        Expr::Signal(signal_ref) => signal_ref_type(ctx, signal_ref)?,
        Expr::Unary { op, operand, .. } => match op {
            UnaryOp::Not | UnaryOp::Neg => expr_type(ctx, ov, operand)?,
            UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor | UnaryOp::LogicNot => ExprType::BIT,
        },
        Expr::Binary { op, lhs, rhs, .. } => match op {
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::And
            | BinaryOp::Or
            | BinaryOp::Xor => expr_type(ctx, ov, lhs)?.merge(expr_type(ctx, ov, rhs)?),
            // The shift amount and the exponent are self-determined.
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::AShr | BinaryOp::Pow => {
                expr_type(ctx, ov, lhs)?
            }
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
            | BinaryOp::LogicAnd
            | BinaryOp::LogicOr => ExprType::BIT,
        },
        Expr::Ternary {
            true_val,
            false_val,
            ..
        } => expr_type(ctx, ov, true_val)?.merge(expr_type(ctx, ov, false_val)?),
        Expr::Concat(parts) => {
            let mut total = Some(0u32);
            for p in parts {
                let w = expr_type(ctx, ov, p)?.width;
                total = total.and_then(|t| t.checked_add(w));
            }
            ExprType::unsigned(check_width(total, "concatenation")?)
        }
        Expr::Repeat { expr, count, .. } => {
            let w = expr_type(ctx, ov, expr)?.width;
            ExprType::unsigned(check_width(w.checked_mul(*count), "replication")?)
        }
        Expr::Index { .. } => ExprType::BIT,
        // Sizing never fails on an operand that cannot be evaluated (a
        // part-select with unknown bounds, a function call): it counts as
        // zero bits wide, and the error is reported only if the operand is
        // actually evaluated. So `sel ? f(x) : y` with `sel == 0` still
        // simulates, as it did before sizing existed.
        Expr::Slice { high, low, .. } => {
            ExprType::unsigned(slice_bounds(ctx, ov, high, low).map_or(0, |(_, w)| w))
        }
        Expr::FuncCall { .. } => ExprType::unsigned(0),
    })
}

/// The width of an unsized integer literal or `integer` value (IEEE
/// 1364-2005 3.5.1, 4.8).
const INTEGER_WIDTH: u32 = 32;

/// Computes the self-determined type of a signed literal: signed, at its
/// declared width when sized, else a 32-bit integer.
///
/// An unsized literal whose magnitude needs more than 32 bits is widened by
/// one bit past the magnitude, so its sign bit stays 0 (an implementation
/// may widen unsized literals, IEEE 1364-2005 3.5.1). A magnitude of exactly
/// 32 bits is kept at 32 and reads as negative, as an `integer` would.
fn signed_literal_type(value: &LogicVec, sized: bool) -> ExprType {
    let width = if sized {
        value.width()
    } else if value.width() > INTEGER_WIDTH {
        value.width().saturating_add(1)
    } else {
        INTEGER_WIDTH
    };
    ExprType {
        width,
        signed: true,
    }
}

/// Computes the self-determined type of a signal reference read in an
/// expression. Only a whole signal can be signed; part-selects and
/// concatenations are unsigned (IEEE 1364-2005 5.5.1).
fn signal_ref_type(ctx: &EvalContext<'_>, signal_ref: &SignalRef) -> Result<ExprType, SimError> {
    match signal_ref {
        SignalRef::Signal(sig_id) => {
            let sig = ctx.signals.get(lookup_signal(ctx, *sig_id)?);
            Ok(ExprType {
                width: sig.width,
                signed: sig.signed,
            })
        }
        other => Ok(ExprType::unsigned(signal_ref_width(ctx, other)?)),
    }
}

/// Maps an IR signal ID to its flat simulation signal.
fn lookup_signal(ctx: &EvalContext<'_>, sig_id: SignalId) -> Result<SimSignalId, SimError> {
    ctx.signal_map
        .get(&sig_id)
        .copied()
        .ok_or_else(|| SimError::InvalidSignalRef {
            reason: format!("unmapped signal ID {}", sig_id.as_raw()),
        })
}

/// Evaluates the bounds of a part-select `[high:low]`, returning the low
/// bit offset and the width.
///
/// Both bounds are self-determined. Unknown (X/Z) bounds, `high < low`, or a
/// width above [`MAX_EVAL_WIDTH`] are evaluation errors, since the width of
/// the result cannot be determined.
fn slice_bounds(
    ctx: &EvalContext<'_>,
    ov: &BlockingOverlay,
    high: &Expr,
    low: &Expr,
) -> Result<(u64, u32), SimError> {
    let h = Bits::from_logic(&eval_self(ctx, ov, high)?).and_then(|b| b.as_u64());
    let l = Bits::from_logic(&eval_self(ctx, ov, low)?).and_then(|b| b.as_u64());
    match (h, l) {
        (Some(h), Some(l)) if h >= l => {
            let width = u32::try_from(h - l).ok().and_then(|w| w.checked_add(1));
            Ok((l, check_width(width, "part-select")?))
        }
        _ => Err(SimError::EvalError {
            reason: "invalid slice bounds".into(),
        }),
    }
}

/// Evaluates `expr` self-determined: at its own width and signedness.
fn eval_self(
    ctx: &EvalContext<'_>,
    ov: &BlockingOverlay,
    expr: &Expr,
) -> Result<LogicVec, SimError> {
    let ty = expr_type(ctx, ov, expr)?;
    eval_ctx(ctx, ov, expr, ty)
}

/// Resizes `v` to `ty.width`: truncates, or extends with copies of the MSB
/// when `ty.signed` (so a signed X MSB extends as X) and with zeros
/// otherwise.
fn extend(v: &LogicVec, ty: ExprType) -> LogicVec {
    if v.width() == ty.width {
        return v.clone();
    }
    let mut out = LogicVec::new(ty.width);
    let fill = if ty.signed && v.width() > 0 {
        v.get(v.width() - 1)
    } else {
        Logic::Zero
    };
    for i in 0..ty.width {
        out.set(i, if i < v.width() { v.get(i) } else { fill });
    }
    out
}

/// Evaluates `expr` in a context of type `ty` (IEEE 1364-2005 5.4.1): the
/// result is exactly `ty.width` bits. Context-determined operands are
/// evaluated at the context width, so `{cout, sum} = a + b` keeps the carry,
/// while self-determined operands (comparison operands, shift amounts,
/// concatenation parts, ...) are evaluated at their own width and the
/// result is then extended.
fn eval_ctx(
    ctx: &EvalContext<'_>,
    ov: &BlockingOverlay,
    expr: &Expr,
    ty: ExprType,
) -> Result<LogicVec, SimError> {
    match expr {
        Expr::Literal(lv) | Expr::NamedConst { value: lv, .. } => Ok(extend(lv, ty)),

        // Zero-extend the bits to the literal's own width first (a no-op
        // for a sized literal), so an unsized `1` is +1 before any sign
        // extension to the context width.
        Expr::SignedLiteral { value, sized } => {
            let own = signed_literal_type(value, *sized);
            Ok(extend(&extend(value, ExprType::unsigned(own.width)), ty))
        }

        Expr::Signal(signal_ref) => Ok(extend(&eval_signal_ref_ov(ctx, ov, signal_ref)?, ty)),

        Expr::Unary { op, operand, .. } => match op {
            UnaryOp::Not => Ok(!&eval_ctx(ctx, ov, operand, ty)?),
            UnaryOp::Neg => {
                let v = eval_ctx(ctx, ov, operand, ty)?;
                Ok(match Bits::from_logic(&v) {
                    Some(b) => b.neg().to_logic(),
                    None => all_x(ty.width),
                })
            }
            UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor | UnaryOp::LogicNot => {
                let v = eval_self(ctx, ov, operand)?;
                Ok(extend(&eval_reduction(*op, &v), ty))
            }
        },

        Expr::Binary { op, lhs, rhs, .. } => eval_binary(ctx, ov, *op, lhs, rhs, ty),

        Expr::Ternary {
            condition,
            true_val,
            false_val,
            ..
        } => {
            let cond = eval_self(ctx, ov, condition)?;
            match truth_value(&cond) {
                Logic::One => eval_ctx(ctx, ov, true_val, ty),
                Logic::Zero => eval_ctx(ctx, ov, false_val, ty),
                // Ambiguous condition: bits on which both branches agree keep
                // their value, the others become X (IEEE 1364-2005 5.1.13).
                Logic::X | Logic::Z => {
                    let t = eval_ctx(ctx, ov, true_val, ty)?;
                    let f = eval_ctx(ctx, ov, false_val, ty)?;
                    let mut result = LogicVec::new(ty.width);
                    for i in 0..ty.width {
                        let (a, b) = (t.get(i), f.get(i));
                        let bit = if a == b && matches!(a, Logic::Zero | Logic::One) {
                            a
                        } else {
                            Logic::X
                        };
                        result.set(i, bit);
                    }
                    Ok(result)
                }
            }
        }

        Expr::Concat(parts) => {
            let mut evaluated: Vec<LogicVec> = Vec::with_capacity(parts.len());
            for p in parts {
                evaluated.push(eval_self(ctx, ov, p)?);
            }
            Ok(extend(&concat_msb_first(&evaluated), ty))
        }

        Expr::Repeat { expr: inner, .. } => {
            let self_ty = expr_type(ctx, ov, expr)?;
            let val = eval_self(ctx, ov, inner)?;
            let mut result = LogicVec::new(self_ty.width);
            if val.width() > 0 {
                for i in 0..self_ty.width {
                    result.set(i, val.get(i % val.width()));
                }
            }
            Ok(extend(&result, ty))
        }

        Expr::Index {
            expr: inner, index, ..
        } => {
            let val = eval_self(ctx, ov, inner)?;
            let idx_val = eval_self(ctx, ov, index)?;
            // Compare the index as an unsigned integer of any width against
            // the operand width before narrowing it, so an index of 2^32 or
            // more cannot wrap to an in-range bit.
            let idx = Bits::from_logic(&idx_val).and_then(|b| b.as_u64());
            let mut bit = LogicVec::new(1);
            match idx {
                // The selected bit is returned unchanged, X and Z included.
                Some(i) if i < u64::from(val.width()) => bit.set(0, val.get(i as u32)),
                // Out-of-range or X/Z index → X (IEEE 1364-2005 5.2.1).
                _ => bit.set(0, Logic::X),
            }
            Ok(extend(&bit, ty))
        }

        Expr::Slice {
            expr: inner,
            high,
            low,
            ..
        } => {
            let val = eval_self(ctx, ov, inner)?;
            let (l, width) = slice_bounds(ctx, ov, high, low)?;
            let mut result = LogicVec::new(width);
            for i in 0..width {
                let src = l + u64::from(i);
                let bit = if src < u64::from(val.width()) {
                    val.get(src as u32)
                } else {
                    Logic::X
                };
                result.set(i, bit);
            }
            Ok(extend(&result, ty))
        }

        Expr::FuncCall { .. } => Err(SimError::Unsupported {
            reason: "function calls in simulation".into(),
        }),
    }
}

/// Concatenates `parts` with the first part in the most significant bits,
/// as in a Verilog `{a, b, c}`.
fn concat_msb_first(parts: &[LogicVec]) -> LogicVec {
    let total_width: u32 = parts.iter().map(|v| v.width()).sum();
    let mut result = LogicVec::new(total_width);
    let mut offset = 0u32;
    for part in parts.iter().rev() {
        for i in 0..part.width() {
            result.set(offset + i, part.get(i));
        }
        offset += part.width();
    }
    result
}

/// Evaluates a `SignalRef` to its current `LogicVec` value, as seen through
/// the executing process's blocking overlay `ov`.
///
/// Bits of a part-select that lie outside the signal read as X.
fn eval_signal_ref_ov(
    ctx: &EvalContext<'_>,
    ov: &BlockingOverlay,
    signal_ref: &SignalRef,
) -> Result<LogicVec, SimError> {
    match signal_ref {
        SignalRef::Signal(sig_id) => Ok(read_signal(ctx, ov, lookup_signal(ctx, *sig_id)?).clone()),
        SignalRef::Slice { signal, high, low } => {
            let full = read_signal(ctx, ov, lookup_signal(ctx, *signal)?);
            let width = slice_ref_width(*high, *low)?;
            let mut result = LogicVec::new(width);
            for i in 0..width {
                let src = u64::from(*low) + u64::from(i);
                let bit = if src < u64::from(full.width()) {
                    full.get(src as u32)
                } else {
                    Logic::X
                };
                result.set(i, bit);
            }
            Ok(result)
        }
        SignalRef::Concat(refs) => {
            let mut parts = Vec::with_capacity(refs.len());
            for r in refs {
                parts.push(eval_signal_ref_ov(ctx, ov, r)?);
            }
            Ok(concat_msb_first(&parts))
        }
        SignalRef::Const(lv) => Ok(lv.clone()),
    }
}

/// Returns the width of a constant part-select `[high:low]` of a signal
/// reference, or an error if `high < low` or the width is unreasonably
/// large.
fn slice_ref_width(high: u32, low: u32) -> Result<u32, SimError> {
    match high.checked_sub(low).and_then(|d| d.checked_add(1)) {
        Some(w) if w <= MAX_EVAL_WIDTH => Ok(w),
        _ => Err(SimError::InvalidSignalRef {
            reason: format!("invalid part-select [{high}:{low}]"),
        }),
    }
}

/// Reduces a value to its truth value for a condition or logical operator:
/// `1` if any bit is `1` (the value is definitely nonzero), `0` if every bit
/// is `0`, and `X` otherwise.
fn truth_value(v: &LogicVec) -> Logic {
    let mut unknown = false;
    for i in 0..v.width() {
        match v.get(i) {
            Logic::One => return Logic::One,
            Logic::X | Logic::Z => unknown = true,
            Logic::Zero => {}
        }
    }
    if unknown {
        Logic::X
    } else {
        Logic::Zero
    }
}

/// Evaluates a reduction (`&`, `|`, `^`) or logical negation (`!`) of a
/// self-determined operand to a 1-bit result.
fn eval_reduction(op: UnaryOp, val: &LogicVec) -> LogicVec {
    let bit = match op {
        UnaryOp::RedAnd => (0..val.width()).fold(Logic::One, |acc, i| acc & val.get(i)),
        UnaryOp::RedOr => (0..val.width()).fold(Logic::Zero, |acc, i| acc | val.get(i)),
        UnaryOp::RedXor => (0..val.width()).fold(Logic::Zero, |acc, i| acc ^ val.get(i)),
        UnaryOp::LogicNot => !truth_value(val),
        // Not and Neg are context-determined; eval_ctx handles them.
        UnaryOp::Not | UnaryOp::Neg => Logic::X,
    };
    let mut r = LogicVec::new(1);
    r.set(0, bit);
    r
}

/// Applies a two-state arithmetic operation to operands of equal width,
/// giving all-X when either operand has an X/Z bit or `op` returns `None`
/// (division by zero).
fn arith(
    l: &LogicVec,
    r: &LogicVec,
    width: u32,
    op: impl Fn(&Bits, &Bits) -> Option<Bits>,
) -> LogicVec {
    match (Bits::from_logic(l), Bits::from_logic(r)) {
        (Some(a), Some(b)) => op(&a, &b).map_or_else(|| all_x(width), |v| v.to_logic()),
        _ => all_x(width),
    }
}

/// Shifts `v` left (`left == true`) or logically right by `amount` bits,
/// keeping its width and filling with zeros. X/Z bits of `v` move with the
/// shift.
fn shift_logic(v: &LogicVec, amount: u64, left: bool) -> LogicVec {
    let width = v.width();
    let mut out = LogicVec::new(width);
    if amount >= u64::from(width) {
        return out;
    }
    let n = amount as u32;
    for i in 0..width {
        let src = if left {
            i.checked_sub(n)
        } else {
            i.checked_add(n).filter(|&s| s < width)
        };
        if let Some(s) = src {
            out.set(i, v.get(s));
        }
    }
    out
}

/// Shifts `v` right by `amount` bits, filling the vacated high bits with
/// copies of its most significant bit (the arithmetic `>>>` of a signed
/// operand, IEEE 1364-2005 5.1.12). An X/Z sign bit fills with that X/Z.
fn shift_right_arith(v: &LogicVec, amount: u64) -> LogicVec {
    let width = v.width();
    if width == 0 {
        return v.clone();
    }
    let sign = v.get(width - 1);
    let mut out = shift_logic(v, amount, false);
    let kept = u64::from(width).saturating_sub(amount) as u32;
    for i in kept..width {
        out.set(i, sign);
    }
    out
}

/// Evaluates a binary operation in a context of type `ty`.
fn eval_binary(
    ctx: &EvalContext<'_>,
    ov: &BlockingOverlay,
    op: BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    ty: ExprType,
) -> Result<LogicVec, SimError> {
    let w = ty.width;
    match op {
        BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Xor
        | BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Mod => {
            let l = eval_ctx(ctx, ov, lhs, ty)?;
            let r = eval_ctx(ctx, ov, rhs, ty)?;
            Ok(match op {
                BinaryOp::And => &l & &r,
                BinaryOp::Or => &l | &r,
                BinaryOp::Xor => &l ^ &r,
                BinaryOp::Add => arith(&l, &r, w, |a, b| Some(a.add(b))),
                BinaryOp::Sub => arith(&l, &r, w, |a, b| Some(a.sub(b))),
                BinaryOp::Mul => arith(&l, &r, w, |a, b| Some(a.mul(b))),
                // x / 0 and x % 0 are all-X (IEEE 1364-2005 5.1.5).
                BinaryOp::Div => arith(&l, &r, w, |a, b| divrem(a, b, ty.signed).map(|qr| qr.0)),
                _ => arith(&l, &r, w, |a, b| divrem(a, b, ty.signed).map(|qr| qr.1)),
            })
        }

        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::AShr => {
            let l = eval_ctx(ctx, ov, lhs, ty)?;
            let amount = eval_self(ctx, ov, rhs)?;
            // The shift amount is always unsigned; an X/Z amount gives X.
            Ok(match Bits::from_logic(&amount) {
                Some(n) => {
                    let n = n.as_u64().unwrap_or(u64::MAX);
                    if op == BinaryOp::AShr && ty.signed {
                        shift_right_arith(&l, n)
                    } else {
                        shift_logic(&l, n, op == BinaryOp::Shl)
                    }
                }
                None => all_x(w),
            })
        }

        BinaryOp::Pow => {
            let base = eval_ctx(ctx, ov, lhs, ty)?;
            let exp_ty = expr_type(ctx, ov, rhs)?;
            let exp = eval_ctx(ctx, ov, rhs, exp_ty)?;
            Ok(match (Bits::from_logic(&base), Bits::from_logic(&exp)) {
                (Some(b), Some(e)) if exp_ty.signed && e.msb() => b
                    .pow_negative_exp(&e, ty.signed)
                    .map_or_else(|| all_x(w), |v| v.to_logic()),
                (Some(b), Some(e)) => b.pow(&e).to_logic(),
                _ => all_x(w),
            })
        }

        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            // The operands size each other; the 1-bit result is unsigned.
            let operand_ty = expr_type(ctx, ov, lhs)?.merge(expr_type(ctx, ov, rhs)?);
            let l = eval_ctx(ctx, ov, lhs, operand_ty)?;
            let r = eval_ctx(ctx, ov, rhs, operand_ty)?;
            let bit = compare(op, &l, &r, operand_ty.signed);
            let mut result = LogicVec::new(1);
            result.set(0, bit);
            Ok(extend(&result, ty))
        }

        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            let l = truth_value(&eval_self(ctx, ov, lhs)?);
            let r = truth_value(&eval_self(ctx, ov, rhs)?);
            let bit = if op == BinaryOp::LogicAnd {
                l & r
            } else {
                l | r
            };
            let mut result = LogicVec::new(1);
            result.set(0, bit);
            Ok(extend(&result, ty))
        }
    }
}

/// Divides `a` by `b` as signed or unsigned integers; `None` when `b` is 0.
fn divrem(a: &Bits, b: &Bits, signed: bool) -> Option<(Bits, Bits)> {
    if signed {
        a.sdivrem(b)
    } else {
        a.udivrem(b)
    }
}

/// Compares two operands of equal width with a comparison operator.
///
/// `==`/`!=` are decided bit by bit: a pair of known, different bits makes
/// the operands definitely unequal even if other bits are X/Z; otherwise
/// any X/Z bit makes the result X. Relational operators give X whenever an
/// operand has an X/Z bit.
fn compare(op: BinaryOp, l: &LogicVec, r: &LogicVec, signed: bool) -> Logic {
    let from_bool = |b: bool| if b { Logic::One } else { Logic::Zero };
    match op {
        BinaryOp::Eq | BinaryOp::Ne => {
            let mut unknown = false;
            for i in 0..l.width() {
                match (l.get(i), r.get(i)) {
                    (Logic::Zero, Logic::One) | (Logic::One, Logic::Zero) => {
                        return from_bool(op == BinaryOp::Ne);
                    }
                    (Logic::Zero, Logic::Zero) | (Logic::One, Logic::One) => {}
                    _ => unknown = true,
                }
            }
            if unknown {
                Logic::X
            } else {
                from_bool(op == BinaryOp::Eq)
            }
        }
        _ => match (Bits::from_logic(l), Bits::from_logic(r)) {
            (Some(a), Some(b)) => {
                let ord = if signed { a.scmp(&b) } else { a.ucmp(&b) };
                from_bool(match op {
                    BinaryOp::Lt => ord.is_lt(),
                    BinaryOp::Le => ord.is_le(),
                    BinaryOp::Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                })
            }
            _ => Logic::X,
        },
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
            // this segment's own earlier blocking writes. It is evaluated at
            // the wider of its own width and the target's (IEEE 1364-2005
            // 5.4.1), so `{cout, sum} = a + b` keeps the carry, and is then
            // fitted to the target: a narrower result is zero- or
            // sign-extended, a wider one truncated.
            let target_width = signal_ref_width(ctx, target)?;
            let rhs_ty = expr_type(ctx, overlay, value)?;
            let ctx_ty = ExprType {
                width: rhs_ty.width.max(target_width),
                signed: rhs_ty.signed,
            };
            let val = eval_ctx(ctx, overlay, value, ctx_ty)?;
            let val = extend(&val, ExprType::unsigned(target_width));
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
            let cond = eval_self(ctx, overlay, condition)?;
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
            // The subject and every item are sized to the widest of them
            // (IEEE 1364-2005 9.5) and compared bit for bit with X and Z
            // matching only themselves, as `===` does. This works at any
            // width.
            let mut case_ty = expr_type(ctx, overlay, subject)?;
            for pat in arms.iter().flat_map(|a| &a.patterns) {
                case_ty = case_ty.merge(expr_type(ctx, overlay, pat)?);
            }
            let subj = eval_ctx(ctx, overlay, subject, case_ty)?;
            for arm in arms {
                for pat in &arm.patterns {
                    let pat_val = eval_ctx(ctx, overlay, pat, case_ty)?;
                    if subj == pat_val {
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
            let cond = eval_self(ctx, overlay, condition)?;
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
            let mut evaluated_args: Vec<(LogicVec, bool)> = Vec::new();
            for arg in args {
                let ty = expr_type(ctx, overlay, arg)?;
                evaluated_args.push((eval_ctx(ctx, overlay, arg, ty)?, ty.signed));
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
/// - A partial write (`Some((high, low))`) replaces only bits `low..=high`,
///   with the value fitted to the slice width the same way (zero-extended or
///   truncated); bits outside the signal are ignored instead of panicking.
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
                if low + i >= new_val.width() {
                    break;
                }
                let bit = if i < value.width() {
                    value.get(i)
                } else {
                    Logic::Zero
                };
                new_val.set(low + i, bit);
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
        SignalRef::Signal(sig_id) => Ok(ctx.signals.get(lookup_signal(ctx, *sig_id)?).width),
        SignalRef::Slice { high, low, .. } => slice_ref_width(*high, *low),
        SignalRef::Concat(refs) => {
            let mut total = Some(0u32);
            for r in refs {
                let w = signal_ref_width(ctx, r)?;
                total = total.and_then(|t| t.checked_add(w));
            }
            check_width(total, "concatenation")
        }
        SignalRef::Const(lv) => Ok(lv.width()),
    }
}

/// Formats a `$display` string with evaluated arguments, each paired with
/// whether it is signed.
///
/// Each format specification is `%[-][width]<conv>` (IEEE 1364-2005
/// 17.1.1.2 and 17.1.1.3):
///
/// - `%d` (decimal), `%h`/`%x` (hex), `%o` (octal), `%b` (binary), `%t`
///   (time, printed as a decimal number) and `%c` (the low 8 bits as a
///   character) consume one argument; `%s` prints the argument's bytes as
///   text (MSB first, NUL bytes skipped).
/// - `%%` prints `%`, and `%m` prints `%m` (the evaluator does not know the
///   hierarchical scope); neither consumes an argument.
/// - Without a width, `%d`/`%h`/`%o`/`%t` print the value without leading
///   zeros or padding and `%b` prints every bit. A width of 0 (`%0d`,
///   `%0b`) prints the minimum number of digits. A non-zero width is a
///   minimum: decimal, time and text are padded with spaces and the other
///   radices with leading zeros, and `-` left-justifies with spaces instead.
///
/// All radices work at any width. A value with X/Z bits prints per IEEE
/// 1364-2005 17.1.1.3: for `%d`, `x`/`z` when every bit is X/Z and `X`/`Z`
/// when only some are; for `%h`/`%o`/`%b`, the same rule per digit. A signed
/// `%d` value prints with a minus sign when negative. An unknown conversion,
/// or one with no argument left, is printed as written.
fn format_display(format: &str, args: &[(LogicVec, bool)]) -> String {
    let mut result = String::new();
    let mut arg_idx = 0;
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        // The specification as written, echoed back when it is not valid.
        let mut raw = String::from('%');
        let left = chars.next_if_eq(&'-').is_some();
        if left {
            raw.push('-');
        }
        let mut digits = String::new();
        while let Some(d) = chars.next_if(char::is_ascii_digit) {
            digits.push(d);
        }
        raw.push_str(&digits);
        // An absurd width saturates; padding is then capped by `pad`.
        let width = (!digits.is_empty()).then(|| digits.parse::<usize>().unwrap_or(usize::MAX));
        let Some(conv) = chars.next() else {
            result.push_str(&raw);
            continue;
        };
        raw.push(conv);
        let spec = FormatSpec { left, width };
        match conv {
            '%' => {
                result.push('%');
                continue;
            }
            'm' | 'M' => {
                result.push_str(&raw);
                continue;
            }
            _ => {}
        }
        let Some((value, signed)) = args.get(arg_idx) else {
            result.push_str(&raw);
            continue;
        };
        let (text, zero_pad) = match conv {
            'd' | 'D' => (format_decimal(value, *signed), false),
            't' | 'T' => (format_decimal(value, false), false),
            'b' | 'B' => {
                // `%b` keeps every bit unless a width asks for the minimum.
                let trim = spec.width.is_some();
                (format_radix(value, 1, trim), true)
            }
            'o' | 'O' => (format_radix(value, 3, true), true),
            'h' | 'H' | 'x' | 'X' => (format_radix(value, 4, true), true),
            's' | 'S' => (format_string(value), false),
            'c' | 'C' => (format_char(value), false),
            _ => (raw, false),
        };
        result.push_str(&spec.pad(&text, zero_pad));
        arg_idx += 1;
    }
    result
}

/// The flags and field width of one `$display` format specification.
#[derive(Debug, Clone, Copy)]
struct FormatSpec {
    /// Whether `-` asked for left justification.
    left: bool,
    /// The explicit field width, if any (`Some(0)` for `%0d`).
    width: Option<usize>,
}

impl FormatSpec {
    /// The widest field [`FormatSpec::pad`] pads to, so a corrupt width
    /// such as `%99999999999d` cannot exhaust memory.
    const MAX_PAD: usize = 1 << 16;

    /// Pads `text` to the field width: on the right with spaces when
    /// left-justified, else on the left with zeros (`zero_pad`, for the
    /// binary, octal and hex radices) or spaces. A negative number keeps its
    /// sign in front of the zeros. Text already as wide is unchanged.
    fn pad(self, text: &str, zero_pad: bool) -> String {
        let width = self.width.unwrap_or(0).min(Self::MAX_PAD);
        let len = text.chars().count();
        if len >= width {
            return text.to_string();
        }
        let fill = width - len;
        if self.left {
            format!("{text}{}", " ".repeat(fill))
        } else if zero_pad {
            match text.strip_prefix('-') {
                Some(rest) => format!("-{}{rest}", "0".repeat(fill)),
                None => format!("{}{text}", "0".repeat(fill)),
            }
        } else {
            format!("{}{text}", " ".repeat(fill))
        }
    }
}

/// Returns the character for a group of bits that contains X or Z: `x`/`z`
/// if every bit is X or every bit is Z, else `X` if any bit is X, else `Z`.
fn unknown_digit(bits: &[Logic]) -> char {
    if bits.iter().all(|&b| b == Logic::X) {
        'x'
    } else if bits.iter().all(|&b| b == Logic::Z) {
        'z'
    } else if bits.contains(&Logic::X) {
        'X'
    } else {
        'Z'
    }
}

/// Formats a value for `%d` at any width.
fn format_decimal(value: &LogicVec, signed: bool) -> String {
    match Bits::from_logic(value) {
        Some(b) if signed && b.msb() => format!("-{}", b.neg().to_decimal()),
        Some(b) => b.to_decimal(),
        None => {
            let bits: Vec<Logic> = (0..value.width()).map(|i| value.get(i)).collect();
            unknown_digit(&bits).to_string()
        }
    }
}

/// Formats a value in a power-of-two radix at any width, one digit per
/// `bits_per_digit` bits (1 for `%b`, 3 for `%o`, 4 for `%h`; the top digit
/// may cover fewer). A digit with X/Z bits prints per [`unknown_digit`].
/// With `trim`, leading zero digits are dropped (keeping at least one).
fn format_radix(value: &LogicVec, bits_per_digit: u32, trim: bool) -> String {
    let w = value.width();
    let digits = w.div_ceil(bits_per_digit);
    let mut s = String::with_capacity(digits as usize);
    for d in (0..digits).rev() {
        let lo = d * bits_per_digit;
        let bits: Vec<Logic> = (lo..(lo + bits_per_digit).min(w))
            .map(|i| value.get(i))
            .collect();
        if bits.iter().any(|b| matches!(b, Logic::X | Logic::Z)) {
            s.push(unknown_digit(&bits));
        } else {
            let digit = bits
                .iter()
                .enumerate()
                .filter(|(_, &b)| b == Logic::One)
                .fold(0u32, |acc, (i, _)| acc | (1 << i));
            s.push(char::from_digit(digit, 16).unwrap_or('?'));
        }
    }
    if !trim {
        return s;
    }
    let trimmed = s.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".into()
    } else {
        trimmed.into()
    }
}

/// Returns the byte held by `bits[lo..lo + 8]` (bits past the end are 0),
/// or `None` if any of them is X/Z.
fn byte_at(value: &LogicVec, lo: u32) -> Option<u8> {
    let mut byte = 0u8;
    for i in 0..8 {
        if lo + i >= value.width() {
            break;
        }
        match value.get(lo + i) {
            Logic::One => byte |= 1 << i,
            Logic::Zero => {}
            Logic::X | Logic::Z => return None,
        }
    }
    Some(byte)
}

/// Formats a value for `%s`: one character per 8 bits, MSB first, skipping
/// NUL bytes (the zero padding of a string stored in a wider vector). A
/// byte with X/Z bits prints as `?`.
fn format_string(value: &LogicVec) -> String {
    let bytes = value.width().div_ceil(8);
    (0..bytes)
        .rev()
        .filter_map(|b| match byte_at(value, b * 8) {
            Some(0) => None,
            Some(byte) => Some(char::from(byte)),
            None => Some('?'),
        })
        .collect()
}

/// Formats a value for `%c`: its low 8 bits as a character, or `?` if any
/// of them is X/Z.
fn format_char(value: &LogicVec) -> String {
    byte_at(value, 0).map_or('?', char::from).to_string()
}

/// Creates an all-X LogicVec of the given width.
fn all_x(width: u32) -> LogicVec {
    let mut v = LogicVec::new(width);
    for i in 0..width {
        v.set(i, Logic::X);
    }
    v
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
    fn eval_named_const_is_its_value() {
        let (mut signals, map, types) = setup_one_signal(LogicVec::from_u64(1, 2));
        let ctx = make_ctx_with_signals(&mut signals, &map, &types);
        let run = Expr::NamedConst {
            name: aion_common::Ident::from_raw(7),
            value: LogicVec::from_u64(1, 32),
        };
        let result = eval_expr(&ctx, &run).unwrap();
        assert_eq!((result.width(), result.to_u64()), (32, Some(1)));
        // `state == RUN` with a 2-bit state and a 32-bit parameter.
        let eq = Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            rhs: Box::new(run),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        assert_eq!(eval_expr(&ctx, &eq).unwrap().to_u64(), Some(1));
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
        // SIM-7: x / 0 is all-X of the operand width, not an error.
        let result = eval_expr(&ctx, &expr).unwrap();
        assert_eq!(format!("{result}"), "XXXXXXXX");
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
        // The 8-bit value 20 is truncated to the 4-bit target: 20 & 0xF.
        assert_eq!(pending[0].value.width(), 4);
        assert_eq!(pending[0].value.to_u64(), Some(4));
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
        let result = format_display("val=%d", &[(LogicVec::from_u64(42, 8), false)]);
        assert_eq!(result, "val=42");
    }

    #[test]
    fn format_display_binary() {
        let result = format_display("val=%b", &[(LogicVec::from_u64(0b101, 3), false)]);
        assert_eq!(result, "val=101");
    }

    #[test]
    fn format_display_hex() {
        let result = format_display("val=%h", &[(LogicVec::from_u64(0xFF, 8), false)]);
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
    fn apply_write_slice_fits_value_to_slice_width() {
        // q = 8'hFF; q[7:4] = 1'b0;  → 0x0F (the narrow value is zero-extended
        // to the 4-bit slice, not merged into its low bit only).
        let current = LogicVec::from_u64(0xFF, 8);
        let v = apply_write(&current, &LogicVec::from_u64(0, 1), Some((7, 4)));
        assert_eq!(v.to_u64(), Some(0x0F));
        // A wider value is truncated to the slice.
        let v = apply_write(&current, &LogicVec::from_u64(0x1A, 8), Some((3, 0)));
        assert_eq!(v.to_u64(), Some(0xFA));
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

    /// Width-correct value semantics (issue #6: SIM-5 .. SIM-9, SIM-12).
    mod width_semantics {
        use super::*;

        /// Creates signals (IR ids 0..n) from `(value, signed)` pairs, each
        /// as wide as its value.
        fn setup(
            specs: &[(LogicVec, bool)],
        ) -> (
            Arena<SimSignalId, SimSignalState>,
            HashMap<SignalId, SimSignalId>,
            TypeDb,
        ) {
            let mut signals = Arena::<SimSignalId, SimSignalState>::new();
            let mut map = HashMap::new();
            for (i, (v, signed)) in specs.iter().enumerate() {
                let id = signals.alloc(
                    SimSignalState::new(format!("s{i}"), v.width(), v.clone()).with_signed(*signed),
                );
                map.insert(SignalId::from_raw(i as u32), id);
            }
            (signals, map, TypeDb::new())
        }

        /// Evaluates `expr` (which reads only literals) self-determined.
        fn ev(expr: &Expr) -> LogicVec {
            let (mut signals, map, types) = setup(&[]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            eval_expr(&ctx, expr).unwrap()
        }

        fn lit(v: u64, w: u32) -> Expr {
            Expr::Literal(LogicVec::from_u64(v, w))
        }

        /// A literal from a binary string such as `"1x0z"` (MSB first).
        fn bits(s: &str) -> Expr {
            Expr::Literal(LogicVec::from_binary_str(s).unwrap())
        }

        /// A `w`-bit literal with value `v << shift`.
        fn wide(v: u64, shift: u32, w: u32) -> Expr {
            let mut lv = LogicVec::new(w);
            for i in 0..64 {
                if (v >> i) & 1 == 1 {
                    lv.set(i + shift, Logic::One);
                }
            }
            Expr::Literal(lv)
        }

        fn bin(op: BinaryOp, l: Expr, r: Expr) -> Expr {
            Expr::Binary {
                op,
                lhs: Box::new(l),
                rhs: Box::new(r),
                ty: aion_ir::TypeId::from_raw(0),
                span: Span::DUMMY,
            }
        }

        fn un(op: UnaryOp, e: Expr) -> Expr {
            Expr::Unary {
                op,
                operand: Box::new(e),
                ty: aion_ir::TypeId::from_raw(0),
                span: Span::DUMMY,
            }
        }

        fn rd(i: u32) -> Expr {
            Expr::Signal(SignalRef::Signal(SignalId::from_raw(i)))
        }

        fn index(e: Expr, i: Expr) -> Expr {
            Expr::Index {
                expr: Box::new(e),
                index: Box::new(i),
                span: Span::DUMMY,
            }
        }

        fn s(v: &LogicVec) -> String {
            format!("{v}")
        }

        /// The expected rendering of a `w`-bit vector holding `v << shift`.
        fn wide_str(v: u64, shift: u32, w: u32) -> String {
            match wide(v, shift, w) {
                Expr::Literal(lv) => s(&lv),
                _ => unreachable!(),
            }
        }

        // ---- SIM-6: power ----

        #[test]
        fn pow_large_exponent_wraps_without_panic() {
            // Repro F: 32'd2 ** 32'd200 panicked with a multiply overflow.
            let r = ev(&bin(BinaryOp::Pow, lit(2, 32), lit(200, 32)));
            assert_eq!((r.width(), r.to_u64()), (32, Some(0)));
            let r = ev(&bin(BinaryOp::Pow, lit(3, 32), lit(5, 32)));
            assert_eq!(r.to_u64(), Some(243));
        }

        #[test]
        fn pow_exponent_is_not_truncated_to_32_bits() {
            // 2 ** (2^32) used to compute 2 ** 0 = 1 after `exp as u32`.
            let r = ev(&bin(BinaryOp::Pow, lit(2, 8), lit(1 << 32, 40)));
            assert_eq!(r.to_u64(), Some(0));
            // 1 ** huge is still 1; x ** 0 is 1.
            let r = ev(&bin(BinaryOp::Pow, lit(1, 8), wide(1, 100, 128)));
            assert_eq!(r.to_u64(), Some(1));
            let r = ev(&bin(BinaryOp::Pow, lit(0, 8), lit(0, 8)));
            assert_eq!(r.to_u64(), Some(1));
        }

        #[test]
        fn pow_with_x_operand_is_all_x() {
            let r = ev(&bin(BinaryOp::Pow, bits("10x1"), lit(2, 4)));
            assert_eq!(s(&r), "XXXX");
        }

        #[test]
        fn pow_negative_signed_exponent_follows_ieee_table() {
            // base signed 8'sd2, exponent signed -1: 2 ** -1 == 0.
            let (mut signals, map, types) = setup(&[
                (LogicVec::from_u64(2, 8), true),
                (LogicVec::from_u64(0xFF, 8), true),
                (LogicVec::from_u64(0, 8), true),
                (LogicVec::from_u64(0xFF, 8), true),
            ]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let r = eval_expr(&ctx, &bin(BinaryOp::Pow, rd(0), rd(1))).unwrap();
            assert_eq!(r.to_u64(), Some(0));
            // 0 ** -1 is X.
            let r = eval_expr(&ctx, &bin(BinaryOp::Pow, rd(2), rd(1))).unwrap();
            assert_eq!(s(&r), "XXXXXXXX");
            // -1 ** -1 is -1.
            let r = eval_expr(&ctx, &bin(BinaryOp::Pow, rd(3), rd(1))).unwrap();
            assert_eq!(r.to_u64(), Some(0xFF));
        }

        // ---- SIM-7: division by zero ----

        #[test]
        fn mod_by_zero_is_all_x() {
            let r = ev(&bin(BinaryOp::Mod, lit(42, 8), lit(0, 4)));
            assert_eq!(s(&r), "XXXXXXXX");
            let r = ev(&bin(BinaryOp::Div, wide(1, 70, 72), wide(0, 0, 72)));
            assert_eq!((r.width(), r.to_u64()), (72, None));
            assert!((0..72).all(|i| r.get(i) == Logic::X));
        }

        #[test]
        fn divide_known_values_and_x_divisor() {
            let r = ev(&bin(BinaryOp::Div, lit(42, 8), lit(5, 8)));
            assert_eq!(r.to_u64(), Some(8));
            let r = ev(&bin(BinaryOp::Mod, lit(42, 8), lit(5, 8)));
            assert_eq!(r.to_u64(), Some(2));
            let r = ev(&bin(BinaryOp::Div, lit(42, 8), bits("0000000x")));
            assert_eq!(s(&r), "XXXXXXXX");
        }

        #[test]
        fn signed_division_truncates_toward_zero() {
            // -7 / 2 == -3 and -7 % 2 == -1 for signed 8-bit operands.
            let (mut signals, map, types) = setup(&[
                (LogicVec::from_u64(0xF9, 8), true),
                (LogicVec::from_u64(2, 8), true),
            ]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let q = eval_expr(&ctx, &bin(BinaryOp::Div, rd(0), rd(1))).unwrap();
            let r = eval_expr(&ctx, &bin(BinaryOp::Mod, rd(0), rd(1))).unwrap();
            assert_eq!((q.to_u64(), r.to_u64()), (Some(0xFD), Some(0xFF)));
            // With an unsigned literal operand the division is unsigned.
            let q = eval_expr(&ctx, &bin(BinaryOp::Div, rd(0), lit(2, 8))).unwrap();
            assert_eq!(q.to_u64(), Some(0xF9 / 2));
        }

        // ---- SIM-8: bit-select ----

        #[test]
        fn bit_select_keeps_x_and_z() {
            // Repro H: Index(4'bxxxx, 0) evaluated to 0.
            assert_eq!(ev(&index(bits("xxxx"), lit(0, 2))).get(0), Logic::X);
            assert_eq!(ev(&index(bits("0z00"), lit(2, 2))).get(0), Logic::Z);
            assert_eq!(ev(&index(bits("0100"), lit(2, 2))).get(0), Logic::One);
        }

        #[test]
        fn bit_select_huge_or_unknown_index_is_x() {
            // 2^32 + 1 used to wrap to bit 1 of 4'b0010 and return 1.
            let r = ev(&index(bits("0010"), lit((1 << 32) + 1, 40)));
            assert_eq!(r.get(0), Logic::X);
            assert_eq!(ev(&index(bits("0010"), lit(4, 8))).get(0), Logic::X);
            assert_eq!(ev(&index(bits("0010"), bits("x1"))).get(0), Logic::X);
            // A wide index whose value is small still selects its bit.
            assert_eq!(ev(&index(bits("0010"), wide(1, 0, 72))).get(0), Logic::One);
            // So does an index above 2^64 not wrap.
            assert_eq!(ev(&index(bits("0010"), wide(1, 64, 72))).get(0), Logic::X);
        }

        // ---- SIM-9: wide values ----

        #[test]
        fn wide_add_and_equality() {
            // Repro I: 72'd1 + 72'd1 was all-X; 72'd1 == 72'd1 was X.
            let r = ev(&bin(BinaryOp::Add, wide(1, 0, 72), wide(1, 0, 72)));
            assert_eq!(s(&r), wide_str(2, 0, 72));
            let r = ev(&bin(BinaryOp::Eq, wide(1, 0, 72), wide(1, 0, 72)));
            assert_eq!(r.to_u64(), Some(1));
            let r = ev(&bin(BinaryOp::Ne, wide(1, 70, 72), wide(1, 0, 72)));
            assert_eq!(r.to_u64(), Some(1));
            // The carry crosses the 64-bit limb boundary.
            let r = ev(&bin(BinaryOp::Add, wide(u64::MAX, 0, 72), wide(1, 0, 72)));
            assert_eq!(s(&r), wide_str(1, 64, 72));
        }

        #[test]
        fn wide_sub_mul_div_and_neg() {
            let r = ev(&bin(BinaryOp::Sub, wide(1, 64, 72), wide(1, 0, 72)));
            assert_eq!(s(&r), wide_str(u64::MAX, 0, 72));
            let r = ev(&bin(BinaryOp::Mul, wide(1, 40, 100), wide(1, 40, 100)));
            assert_eq!(s(&r), wide_str(1, 80, 100));
            let r = ev(&bin(BinaryOp::Div, wide(1, 80, 100), wide(1, 40, 100)));
            assert_eq!(s(&r), wide_str(1, 40, 100));
            let r = ev(&bin(BinaryOp::Mod, wide(7, 70, 100), wide(2, 70, 100)));
            assert_eq!(s(&r), wide_str(1, 70, 100));
            // -1 at 72 bits is all ones.
            let r = ev(&un(UnaryOp::Neg, wide(1, 0, 72)));
            assert!((0..72).all(|i| r.get(i) == Logic::One));
            // ^ of a wide value with three ones is 1.
            let r = ev(&un(UnaryOp::RedXor, wide(0b111, 63, 72)));
            assert_eq!(r.to_u64(), Some(1));
        }

        #[test]
        fn wide_relational_and_shifts() {
            let r = ev(&bin(BinaryOp::Gt, wide(1, 70, 72), wide(5, 0, 72)));
            assert_eq!(r.to_u64(), Some(1));
            let r = ev(&bin(BinaryOp::Le, wide(1, 70, 72), wide(5, 0, 72)));
            assert_eq!(r.to_u64(), Some(0));
            let r = ev(&bin(BinaryOp::Shl, wide(1, 0, 100), lit(70, 8)));
            assert_eq!(s(&r), wide_str(1, 70, 100));
            let r = ev(&bin(BinaryOp::Shr, wide(0xFF, 64, 72), lit(64, 8)));
            assert_eq!(s(&r), wide_str(0xFF, 0, 72));
            // A huge shift amount clears the value.
            let r = ev(&bin(BinaryOp::Shl, wide(1, 0, 72), wide(1, 64, 72)));
            assert_eq!(s(&r), wide_str(0, 0, 72));
        }

        #[test]
        fn shift_moves_x_bits_and_unknown_amount_is_x() {
            let r = ev(&bin(BinaryOp::Shl, bits("001x"), lit(1, 2)));
            assert_eq!(s(&r), "01X0");
            let r = ev(&bin(BinaryOp::Shr, bits("1000"), bits("x")));
            assert_eq!(s(&r), "XXXX");
        }

        #[test]
        fn ashr_sign_fills_signed_and_zero_fills_unsigned() {
            let (mut signals, map, types) = setup(&[
                (LogicVec::from_u64(0xF8, 8), true),
                (LogicVec::from_binary_str("x0000000").unwrap(), true),
                (LogicVec::from_u64(0x40, 8), true),
            ]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let ashr = |l: Expr, r: Expr| eval_expr(&ctx, &bin(BinaryOp::AShr, l, r)).unwrap();
            // -8 >>> 1 == -4; an over-shift leaves only sign bits.
            assert_eq!(ashr(rd(0), lit(1, 4)).to_u64(), Some(0xFC));
            assert_eq!(ashr(rd(0), lit(200, 8)).to_u64(), Some(0xFF));
            assert_eq!(ashr(rd(0), lit(0, 1)).to_u64(), Some(0xF8));
            // An X sign bit fills with X; a positive value zero-fills.
            assert_eq!(s(&ashr(rd(1), lit(2, 2))), "XXX00000");
            assert_eq!(ashr(rd(2), lit(3, 2)).to_u64(), Some(0x08));
            // An unsigned operand (a literal) shifts logically, and an X
            // amount gives all-X.
            assert_eq!(
                ev(&bin(BinaryOp::AShr, lit(0xF8, 8), lit(1, 1))).to_u64(),
                Some(0x7C)
            );
            assert_eq!(s(&ashr(rd(0), bits("x"))), "XXXXXXXX");
        }

        #[test]
        fn equality_with_x_follows_ieee() {
            // A known mismatching bit decides `==` despite X elsewhere.
            let r = ev(&bin(BinaryOp::Eq, bits("1x00"), bits("0x00")));
            assert_eq!(r.get(0), Logic::Zero);
            let r = ev(&bin(BinaryOp::Ne, bits("1x00"), bits("0x00")));
            assert_eq!(r.get(0), Logic::One);
            // Otherwise any X/Z gives X.
            let r = ev(&bin(BinaryOp::Eq, bits("1x00"), bits("1x00")));
            assert_eq!(r.get(0), Logic::X);
            let r = ev(&bin(BinaryOp::Lt, bits("1x00"), bits("0000")));
            assert_eq!(r.get(0), Logic::X);
        }

        fn case_stmt(subject: Expr, patterns: Vec<Expr>) -> Statement {
            Statement::Case {
                subject,
                arms: patterns
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| aion_ir::CaseArm {
                        patterns: vec![p],
                        body: assign(sig(0), lit(i as u64 + 1, 8), AssignKind::Blocking),
                        span: Span::DUMMY,
                    })
                    .collect(),
                default: Some(Box::new(assign(sig(0), lit(0xEE, 8), AssignKind::Blocking))),
                span: Span::DUMMY,
            }
        }

        /// Runs a case statement and returns the arm it took (1-based; 0xEE
        /// for the default).
        fn case_arm(stmt: &Statement) -> Option<u64> {
            let (mut signals, map, types) = setup_byte_signals(&[0]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let (pending, _) = run_segment(&ctx, stmt);
            last_update(&pending, &map, 0).0
        }

        #[test]
        fn case_matches_wide_subject() {
            let stmt = case_stmt(wide(1, 0, 72), vec![wide(2, 0, 72), wide(1, 0, 72)]);
            assert_eq!(case_arm(&stmt), Some(2));
            let stmt = case_stmt(wide(1, 70, 72), vec![wide(1, 0, 72)]);
            assert_eq!(case_arm(&stmt), Some(0xEE));
        }

        #[test]
        fn case_items_are_sized_to_the_widest_and_match_x_exactly() {
            // A 4-bit subject matches an 8-bit item of the same value.
            let stmt = case_stmt(lit(5, 4), vec![lit(5, 8)]);
            assert_eq!(case_arm(&stmt), Some(1));
            // X matches only X (case equality), not 0 or 1.
            let stmt = case_stmt(bits("x1"), vec![bits("01"), bits("x1")]);
            assert_eq!(case_arm(&stmt), Some(2));
            let stmt = case_stmt(bits("x1"), vec![bits("11")]);
            assert_eq!(case_arm(&stmt), Some(0xEE));
        }

        #[test]
        fn display_formats_wide_values() {
            let big = match wide(0xFF, 64, 72) {
                Expr::Literal(lv) => lv,
                _ => unreachable!(),
            };
            assert_eq!(
                format_display("%d", &[(big.clone(), false)]),
                "4703919738795935662080"
            );
            assert_eq!(format_display("%h", &[(big, false)]), "ff0000000000000000");
            assert_eq!(
                format_display("%h", &[(LogicVec::from_u64(0, 72), false)]),
                "0"
            );
        }

        #[test]
        fn display_formats_x_z_and_signed_values() {
            let lv = |s: &str| LogicVec::from_binary_str(s).unwrap();
            assert_eq!(format_display("%d", &[(lv("xxxx"), false)]), "x");
            assert_eq!(format_display("%d", &[(lv("zzzz"), false)]), "z");
            assert_eq!(format_display("%d", &[(lv("10x1"), false)]), "X");
            assert_eq!(format_display("%d", &[(lv("10z1"), false)]), "Z");
            assert_eq!(format_display("%h", &[(lv("1010xxxx"), false)]), "ax");
            assert_eq!(format_display("%h", &[(lv("10x00101"), false)]), "X5");
            assert_eq!(format_display("%h", &[(lv("zzzz0001"), false)]), "z1");
            assert_eq!(format_display("%d", &[(lv("11111101"), true)]), "-3");
            assert_eq!(format_display("%d", &[(lv("11111101"), false)]), "253");
            // `%%` never consumes an argument, even when none are left.
            assert_eq!(format_display("100%%", &[]), "100%");
            assert_eq!(format_display("%d %d", &[(lv("1"), false)]), "1 %d");
        }

        // ---- $display format specifications ----

        #[test]
        fn display_zero_width_is_one_specification() {
            // Review repro: `%0d` used to print `%0d` and swallow `u`.
            let u = (LogicVec::from_u64(5, 8), false);
            assert_eq!(
                format_display("v=%0d h=%h", &[u.clone(), u.clone()]),
                "v=5 h=5"
            );
            assert_eq!(
                format_display("%0h|%0o|%0b", &[u.clone(), u.clone(), u]),
                "5|5|101"
            );
            let zero = (LogicVec::from_u64(0, 8), false);
            assert_eq!(format_display("%0b %0h", &[zero.clone(), zero]), "0 0");
        }

        #[test]
        fn display_default_width_is_unchanged() {
            // Without a width: no padding for %d/%h/%o, every bit for %b.
            let u = (LogicVec::from_u64(10, 8), false);
            assert_eq!(
                format_display("%d %h %o %b", &[u.clone(), u.clone(), u.clone(), u]),
                "10 a 12 00001010"
            );
        }

        #[test]
        fn display_explicit_width_pads() {
            let lv = |s: &str| LogicVec::from_binary_str(s).unwrap();
            let u = (LogicVec::from_u64(5, 8), false);
            assert_eq!(format_display("[%4d]", std::slice::from_ref(&u)), "[   5]");
            assert_eq!(format_display("[%-4d]", std::slice::from_ref(&u)), "[5   ]");
            assert_eq!(format_display("[%4h]", std::slice::from_ref(&u)), "[0005]");
            assert_eq!(format_display("[%-4h]", std::slice::from_ref(&u)), "[5   ]");
            assert_eq!(format_display("[%4o]", std::slice::from_ref(&u)), "[0005]");
            assert_eq!(format_display("[%5b]", std::slice::from_ref(&u)), "[00101]");
            // A width smaller than the value never truncates it.
            assert_eq!(format_display("[%1d]", &[(lv("11111111"), false)]), "[255]");
            // Negative signed values keep the sign in front.
            assert_eq!(format_display("[%4d]", &[(lv("11111101"), true)]), "[  -3]");
            // X/Z values pad like known ones.
            assert_eq!(format_display("[%3d]", &[(lv("xxxx"), false)]), "[  x]");
            assert_eq!(format_display("[%6b]", &[(lv("xx1z"), false)]), "[00xx1z]");
            assert_eq!(format_display("[%-3h]", &[(lv("zzzz"), false)]), "[z  ]");
            assert_eq!(format_display("[%0h]", &[(lv("0000x1x1"), false)]), "[X]");
            // A huge width is capped instead of exhausting memory.
            let out = format_display("%99999999999999999999d", &[u]);
            assert_eq!(out.len(), FormatSpec::MAX_PAD);
        }

        #[test]
        fn display_other_conversions() {
            let lv = |s: &str| LogicVec::from_binary_str(s).unwrap();
            // "Hi" stored right-aligned in 24 bits: the NUL byte is skipped.
            let hi = (LogicVec::from_u64(0x4869, 24), false);
            assert_eq!(
                format_display("%s|%4s|%-4s|", &[hi.clone(), hi.clone(), hi]),
                "Hi|  Hi|Hi  |"
            );
            assert_eq!(
                format_display("%c", &[(LogicVec::from_u64(0x141, 9), false)]),
                "A"
            );
            assert_eq!(
                format_display("%c%s", &[(lv("x1000001"), false), (lv("0100000x"), false)]),
                "??"
            );
            assert_eq!(
                format_display("t=%t", &[(LogicVec::from_u64(100, 64), true)]),
                "t=100"
            );
            assert_eq!(format_display("%o", &[(lv("1x0111"), false)]), "X7");
            // %m and %% take no argument; unknown or argument-less
            // specifications are echoed as written.
            let one = (LogicVec::from_u64(1, 1), false);
            assert_eq!(
                format_display("%m %d %5%", std::slice::from_ref(&one)),
                "%m 1 %"
            );
            // An unknown conversion still consumes its argument, as before.
            assert_eq!(format_display("%-3q %d", &[one.clone(), one]), "%-3q 1");
            assert_eq!(format_display("end%-", &[]), "end%-");
            assert_eq!(format_display("%4d", &[]), "%4d");
        }

        // ---- Signed literals (review follow-up) ----

        /// A sized signed literal `w'sd v`.
        fn slit(v: u64, w: u32) -> Expr {
            Expr::SignedLiteral {
                value: LogicVec::from_u64(v, w),
                sized: true,
            }
        }

        /// An unsized decimal literal, at its minimal width as the
        /// elaborator lowers it.
        fn int(v: u64) -> Expr {
            let w = (64 - v.leading_zeros()).max(1);
            Expr::SignedLiteral {
                value: LogicVec::from_u64(v, w),
                sized: false,
            }
        }

        #[test]
        fn unsized_literal_is_a_32_bit_signed_integer() {
            // `1` is +1 (its minimal 1-bit form is not read as -1), 32 bits.
            let r = ev(&int(1));
            assert_eq!((r.width(), r.to_u64()), (32, Some(1)));
            let r = ev(&un(UnaryOp::Neg, int(1)));
            assert_eq!((r.width(), r.to_u64()), (32, Some(0xFFFF_FFFF)));
            // A magnitude wider than 32 bits keeps a zero sign bit.
            let r = ev(&int(1 << 40));
            assert_eq!((r.width(), r.to_u64()), (42, Some(1 << 40)));
            // A sized signed literal keeps its width.
            let r = ev(&slit(0xFE, 8));
            assert_eq!((r.width(), r.to_u64()), (8, Some(0xFE)));
        }

        #[test]
        fn signed_literal_keeps_signed_operations_signed() {
            // Review repro: integer i = -1; reg signed [7:0] s = -2.
            let (mut signals, map, types) = setup(&[
                (LogicVec::from_u64(0xFFFF_FFFF, 32), true),
                (LogicVec::from_u64(0xFE, 8), true),
            ]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let lt = |l, r| eval_expr(&ctx, &bin(BinaryOp::Lt, l, r)).unwrap().to_u64();
            assert_eq!(lt(rd(0), int(0)), Some(1), "i < 0");
            assert_eq!(lt(rd(1), int(0)), Some(1), "s < 0");
            assert_eq!(lt(rd(1), un(UnaryOp::Neg, slit(1, 8))), Some(1), "-2 < -1");
            // `w16 = s + 1` sign-extends s: the RHS is a signed 32-bit sum.
            let sum = eval_expr(&ctx, &bin(BinaryOp::Add, rd(1), int(1))).unwrap();
            assert_eq!((sum.width(), sum.to_u64()), (32, Some(0xFFFF_FFFF)));
            // The literal is signed too, so it sign-extends in a signed
            // context: 8'sb1111_1110 + 16'sd0 is -2 at 16 bits.
            let (mut signals, map, types) = setup(&[(LogicVec::from_u64(0, 16), true)]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let r = eval_expr(&ctx, &bin(BinaryOp::Add, slit(0xFE, 8), rd(0))).unwrap();
            assert_eq!((r.width(), r.to_u64()), (16, Some(0xFFFE)));
        }

        #[test]
        fn unsigned_literal_still_makes_the_operation_unsigned() {
            // s = -2 compared with 8'd0 / 'h0: unsigned, so 254 < 0 is false.
            let (mut signals, map, types) = setup(&[(LogicVec::from_u64(0xFE, 8), true)]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let r = eval_expr(&ctx, &bin(BinaryOp::Lt, rd(0), lit(0, 8))).unwrap();
            assert_eq!(r.to_u64(), Some(0));
            let r = eval_expr(&ctx, &bin(BinaryOp::Add, rd(0), lit(1, 8))).unwrap();
            assert_eq!((r.width(), r.to_u64()), (8, Some(0xFF)));
            // An unsigned operand makes a signed literal zero-extend too.
            let r = ev(&bin(BinaryOp::Add, slit(0xFE, 8), lit(0, 16)));
            assert_eq!((r.width(), r.to_u64()), (16, Some(0x00FE)));
        }

        // ---- SIM-12 / SIM-5: context width and resize to target ----

        /// Runs `target = value` over the given signals and returns the
        /// single resulting update's value.
        fn assign_result(specs: &[(LogicVec, bool)], target: SignalRef, value: Expr) -> LogicVec {
            let (mut signals, map, types) = setup(specs);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let (pending, _) = run_segment(&ctx, &assign(target, value, AssignKind::Blocking));
            assert_eq!(pending.len(), 1);
            pending[0].value.clone()
        }

        #[test]
        fn add_is_evaluated_at_target_width_keeping_carry() {
            // wide9 = a + b with 8-bit a = 0xFF, b = 1 → 0x100, not 0x00.
            let specs = [
                (LogicVec::new(9), false),
                (LogicVec::from_u64(0xFF, 8), false),
                (LogicVec::from_u64(1, 8), false),
            ];
            let v = assign_result(&specs, sig(0), bin(BinaryOp::Add, rd(1), rd(2)));
            assert_eq!((v.width(), v.to_u64()), (9, Some(0x100)));
        }

        #[test]
        fn concat_target_receives_the_carry() {
            // {cout, sum} = a + b.
            let specs = [
                (LogicVec::new(1), false),
                (LogicVec::new(8), false),
                (LogicVec::from_u64(0xFF, 8), false),
                (LogicVec::from_u64(1, 8), false),
            ];
            let (mut signals, map, types) = setup(&specs);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let target = SignalRef::Concat(vec![sig(0), sig(1)]);
            let stmt = assign(
                target,
                bin(BinaryOp::Add, rd(2), rd(3)),
                AssignKind::Blocking,
            );
            let (pending, _) = run_segment(&ctx, &stmt);
            assert_eq!(last_update(&pending, &map, 0).0, Some(1));
            assert_eq!(last_update(&pending, &map, 1).0, Some(0));
        }

        #[test]
        fn concat_operand_is_self_determined() {
            // {a + b} is a concatenation: its operand is evaluated at 8 bits,
            // so a 9-bit target sees the carry dropped.
            let specs = [
                (LogicVec::new(9), false),
                (LogicVec::from_u64(0xFF, 8), false),
                (LogicVec::from_u64(1, 8), false),
            ];
            let value = Expr::Concat(vec![bin(BinaryOp::Add, rd(1), rd(2))]);
            let v = assign_result(&specs, sig(0), value);
            assert_eq!(v.to_u64(), Some(0));
        }

        #[test]
        fn update_is_resized_to_target_width() {
            // SIM-5: narrower RHS is zero-extended, wider is truncated, so a
            // PendingUpdate never carries a different width than its target.
            let specs = [(LogicVec::from_u64(0xFF, 8), false)];
            let v = assign_result(&specs, sig(0), lit(0x3, 2));
            assert_eq!((v.width(), v.to_u64()), (8, Some(0x03)));
            let v = assign_result(&specs, sig(0), lit(0xABC, 12));
            assert_eq!((v.width(), v.to_u64()), (8, Some(0xBC)));
            // A slice target gets a value of exactly the slice width.
            let slice = SignalRef::Slice {
                signal: SignalId::from_raw(0),
                high: 7,
                low: 4,
            };
            let v = assign_result(&specs, slice, lit(1, 1));
            assert_eq!((v.width(), v.to_u64()), (4, Some(1)));
        }

        #[test]
        fn signed_rhs_is_sign_extended_unsigned_is_not() {
            let specs = [
                (LogicVec::new(16), false),
                (LogicVec::from_u64(0xFD, 8), true),
                (LogicVec::from_u64(0xFD, 8), false),
            ];
            let v = assign_result(&specs, sig(0), rd(1));
            assert_eq!(v.to_u64(), Some(0xFFFD));
            let v = assign_result(&specs, sig(0), rd(2));
            assert_eq!(v.to_u64(), Some(0x00FD));
            // Mixed signed/unsigned operands make the expression unsigned.
            let v = assign_result(&specs, sig(0), bin(BinaryOp::Add, rd(1), lit(0, 8)));
            assert_eq!(v.to_u64(), Some(0x00FD));
        }

        #[test]
        fn comparison_operands_size_each_other() {
            // 4'hF == 8'h0F: the narrow operand is zero-extended.
            let r = ev(&bin(BinaryOp::Eq, lit(0xF, 4), lit(0x0F, 8)));
            assert_eq!(r.to_u64(), Some(1));
            // Signed: 4'sb1111 (-1) == 8'sb11111111 (-1), and -1 < 1.
            let (mut signals, map, types) = setup(&[
                (LogicVec::from_u64(0xF, 4), true),
                (LogicVec::from_u64(0xFF, 8), true),
                (LogicVec::from_u64(1, 8), true),
            ]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let r = eval_expr(&ctx, &bin(BinaryOp::Eq, rd(0), rd(1))).unwrap();
            assert_eq!(r.to_u64(), Some(1));
            let r = eval_expr(&ctx, &bin(BinaryOp::Lt, rd(0), rd(2))).unwrap();
            assert_eq!(r.to_u64(), Some(1));
            // The same bits compared unsigned: 15 < 1 is false.
            let r = eval_expr(&ctx, &bin(BinaryOp::Lt, rd(0), lit(1, 8))).unwrap();
            assert_eq!(r.to_u64(), Some(0));
        }

        // ---- conditions and logical operators at any width ----

        #[test]
        fn multi_bit_condition_is_true_if_any_bit_is_one() {
            let lv = |s: &str| LogicVec::from_binary_str(s).unwrap();
            assert!(logic_is_true(&lv("10")));
            assert!(logic_is_true(&lv("1x")));
            assert!(!logic_is_true(&lv("x0")));
            assert!(!logic_is_true(&lv("00")));
            assert!(!logic_is_true(&LogicVec::new(0)));
        }

        #[test]
        fn logical_operators_use_truth_values() {
            assert_eq!(ev(&un(UnaryOp::LogicNot, bits("1x"))).get(0), Logic::Zero);
            assert_eq!(ev(&un(UnaryOp::LogicNot, bits("0x"))).get(0), Logic::X);
            let r = ev(&bin(BinaryOp::LogicAnd, bits("10"), bits("01")));
            assert_eq!(r.get(0), Logic::One);
            let r = ev(&bin(BinaryOp::LogicAnd, bits("00"), bits("x")));
            assert_eq!(r.get(0), Logic::Zero);
            let r = ev(&bin(BinaryOp::LogicOr, bits("x"), bits("10")));
            assert_eq!(r.get(0), Logic::One);
        }

        #[test]
        fn ternary_with_unknown_condition_merges_branches() {
            let t = Expr::Ternary {
                condition: Box::new(bits("x")),
                true_val: Box::new(bits("1100")),
                false_val: Box::new(bits("1010")),
                ty: aion_ir::TypeId::from_raw(0),
                span: Span::DUMMY,
            };
            assert_eq!(s(&ev(&t)), "1XX0");
        }

        // ---- malformed IR is reported, never a panic ----

        #[test]
        fn absurd_widths_are_errors_not_panics() {
            let (mut signals, map, types) = setup(&[(LogicVec::new(8), false)]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            let rep = Expr::Repeat {
                expr: Box::new(lit(0, 8)),
                count: u32::MAX,
                span: Span::DUMMY,
            };
            assert!(matches!(
                eval_expr(&ctx, &rep),
                Err(SimError::EvalError { .. })
            ));
            let reversed = Expr::Signal(SignalRef::Slice {
                signal: SignalId::from_raw(0),
                high: 2,
                low: 5,
            });
            assert!(matches!(
                eval_expr(&ctx, &reversed),
                Err(SimError::InvalidSignalRef { .. })
            ));
            let huge_slice = Expr::Slice {
                expr: Box::new(rd(0)),
                high: Box::new(lit(u64::MAX, 64)),
                low: Box::new(lit(0, 1)),
                span: Span::DUMMY,
            };
            assert!(matches!(
                eval_expr(&ctx, &huge_slice),
                Err(SimError::EvalError { .. })
            ));
        }

        #[test]
        fn unevaluable_operand_in_untaken_branch_is_not_an_error() {
            let call = Expr::FuncCall {
                name: aion_common::Ident::from_raw(0),
                args: vec![],
                ty: aion_ir::TypeId::from_raw(0),
                span: Span::DUMMY,
            };
            let x_slice = Expr::Slice {
                expr: Box::new(lit(0, 8)),
                high: Box::new(bits("x")),
                low: Box::new(lit(0, 1)),
                span: Span::DUMMY,
            };
            for untaken in [call.clone(), x_slice.clone()] {
                let t = Expr::Ternary {
                    condition: Box::new(lit(0, 1)),
                    true_val: Box::new(untaken),
                    false_val: Box::new(lit(5, 4)),
                    ty: aion_ir::TypeId::from_raw(0),
                    span: Span::DUMMY,
                };
                assert_eq!(ev(&t).to_u64(), Some(5));
            }
            // Evaluating them is still reported.
            let (mut signals, map, types) = setup(&[]);
            let ctx = make_ctx_with_signals(&mut signals, &map, &types);
            assert!(matches!(
                eval_expr(&ctx, &call),
                Err(SimError::Unsupported { .. })
            ));
            assert!(matches!(
                eval_expr(&ctx, &x_slice),
                Err(SimError::EvalError { .. })
            ));
        }

        #[test]
        fn slice_beyond_operand_reads_x() {
            let slice = Expr::Slice {
                expr: Box::new(bits("0110")),
                high: Box::new(lit(5, 8)),
                low: Box::new(lit(2, 8)),
                span: Span::DUMMY,
            };
            assert_eq!(s(&ev(&slice)), "XX01");
        }
    }
}
