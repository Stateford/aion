//! AST expression lowering to IR expressions.
//!
//! Converts language-specific AST expression trees (Verilog, SystemVerilog, VHDL)
//! into the unified [`Expr`](aion_ir::expr::Expr) representation. Names are
//! resolved through a [`SignalEnv`], which maps interned names to signal IDs and,
//! for parameters and generics, to constant values.

use std::collections::HashMap;

use aion_common::{Ident, Interner, LogicVec};
use aion_diagnostics::DiagnosticSink;
use aion_ir::expr::{BinaryOp, Expr as IrExpr, UnaryOp};
use aion_ir::ids::{SignalId, TypeId};
use aion_ir::signal::SignalRef;
use aion_ir::ConstValue;
use aion_source::{SourceDb, Span};

use crate::const_eval::{self, ConstEnv};
use crate::errors;

/// Name-resolution scope for expression lowering within one module.
///
/// Maps interned names to the module's signals and, separately, to its
/// compile-time constants (Verilog/SV `parameter`/`localparam`, VHDL
/// generics). An identifier is resolved as a signal first; only a name that
/// is not a signal falls back to a constant, which lowers to an
/// [`IrExpr::NamedConst`] carrying the constant's value at its width. A name
/// that is neither is an `E204` error.
///
/// Carrying the constants here means every lowering function that already
/// receives the scope can resolve parameter names, without a separate
/// constant environment threaded through each call.
#[derive(Debug, Clone, Default)]
pub struct SignalEnv {
    /// Signal names visible in this scope.
    signals: HashMap<Ident, SignalId>,
    /// Constant values, usable by the constant evaluator.
    consts: ConstEnv,
    /// Bit width of each constant when lowered to a literal.
    const_widths: HashMap<Ident, u32>,
}

impl SignalEnv {
    /// Creates an empty scope with no signals and no constants.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the signal bound to `name`, ignoring constants.
    pub fn get(&self, name: &Ident) -> Option<&SignalId> {
        self.signals.get(name)
    }

    /// Binds `name` to signal `sid`, returning the previous binding if any.
    ///
    /// A signal binding takes precedence over a constant of the same name.
    pub fn insert(&mut self, name: Ident, sid: SignalId) -> Option<SignalId> {
        self.signals.insert(name, sid)
    }

    /// Binds `name` to a constant `value` that lowers to a `width`-bit literal.
    ///
    /// `width` is the parameter's declared width, or its value's width for an
    /// untyped parameter (see `param_value_width`). A width of 0 is
    /// treated as 1.
    pub fn insert_const(&mut self, name: Ident, value: ConstValue, width: u32) {
        self.consts.insert(name, value);
        self.const_widths.insert(name, width.max(1));
    }

    /// Returns every constant in scope, for use with the constant evaluator
    /// (replication counts, delays, instance parameter overrides, indices).
    pub fn consts(&self) -> &ConstEnv {
        &self.consts
    }

    /// Lowers the constant `name` to a literal of its value and width, or
    /// returns `None` if `name` is not a constant in this scope.
    ///
    /// Integers are encoded in two's complement (sign-extended past 64
    /// bits); a real is truncated toward zero; a logic vector is
    /// zero-extended or truncated to the width; a boolean is 0 or 1; a
    /// string lowers to the same zero placeholder as a string literal.
    pub fn const_literal(&self, name: Ident) -> Option<LogicVec> {
        let value = self.consts.get(&name)?;
        let width = self.const_widths.get(&name).copied().unwrap_or(32);
        Some(const_value_to_logic(value, width))
    }
}

/// Encodes a constant value as a `width`-bit logic vector.
fn const_value_to_logic(value: &ConstValue, width: u32) -> LogicVec {
    use aion_common::Logic;
    let int = match value {
        ConstValue::Int(v) => *v,
        ConstValue::Real(f) => *f as i64,
        ConstValue::Bool(b) => i64::from(*b),
        ConstValue::Logic(lv) => {
            let mut out = LogicVec::all_zero(width);
            for i in 0..width.min(lv.width()) {
                out.set(i, lv.get(i));
            }
            return out;
        }
        ConstValue::String(_) => return LogicVec::all_zero(1),
    };
    let mut lv = logic_vec_from_u64(width, int as u64);
    if int < 0 {
        for i in 64..width {
            lv.set(i, Logic::One);
        }
    }
    lv
}

/// Returns the width a parameter lowers to when used in an expression.
///
/// `declared` is the width of an explicit range or type, which always wins
/// (IEEE 1364-2005 12.2, IEEE 1800-2017 6.20.2). Without one, the parameter
/// takes the width of its final value: `value_width` (the self-determined
/// width of the value expression) when the value is the declared default,
/// a logic vector's own width, and 32 bits for any integer (an unsized
/// integer value, or an instance override, which carries no width).
pub(crate) fn param_value_width(
    declared: Option<u32>,
    value: &ConstValue,
    value_width: Option<u32>,
) -> u32 {
    if let Some(w) = declared {
        return w.max(1);
    }
    match value {
        ConstValue::Logic(lv) => lv.width().max(1),
        ConstValue::Bool(_) => 1,
        ConstValue::Real(_) => 64,
        ConstValue::Int(_) | ConstValue::String(_) => value_width.unwrap_or(32).max(1),
    }
}

/// Returns the self-determined width of an untyped Verilog parameter value.
///
/// A sized literal (`2'b01`) has its explicit width and an unsized one is
/// 32 bits. A name of an earlier constant has that constant's width.
/// Parentheses, unary `-`/`+`/`~`, and binary arithmetic or bitwise
/// operators take the widest operand; anything else is treated as a 32-bit
/// integer.
pub(crate) fn verilog_value_width(
    expr: &aion_verilog_parser::ast::Expr,
    env: &SignalEnv,
    source_db: &SourceDb,
) -> u32 {
    use aion_verilog_parser::ast::{BinaryOp as B, Expr, UnaryOp as U};
    match expr {
        Expr::Literal { span } => literal_width(source_db.snippet(*span)),
        Expr::Identifier { name, .. } => env.const_widths.get(name).copied().unwrap_or(32),
        Expr::Paren { inner, .. } => verilog_value_width(inner, env, source_db),
        Expr::Unary {
            op: U::Minus | U::Plus | U::BitNot,
            operand,
            ..
        } => verilog_value_width(operand, env, source_db),
        Expr::Binary {
            left,
            op: B::Add | B::Sub | B::Mul | B::Div | B::Mod | B::BitAnd | B::BitOr | B::BitXor,
            right,
            ..
        } => verilog_value_width(left, env, source_db)
            .max(verilog_value_width(right, env, source_db)),
        _ => 32,
    }
}

/// Returns the self-determined width of an untyped SV parameter value.
///
/// Same rules as [`verilog_value_width`].
pub(crate) fn sv_value_width(
    expr: &aion_sv_parser::ast::Expr,
    env: &SignalEnv,
    source_db: &SourceDb,
) -> u32 {
    use aion_sv_parser::ast::{BinaryOp as B, Expr, UnaryOp as U};
    match expr {
        Expr::Literal { span } => literal_width(source_db.snippet(*span)),
        Expr::Identifier { name, .. } => env.const_widths.get(name).copied().unwrap_or(32),
        Expr::Paren { inner, .. } => sv_value_width(inner, env, source_db),
        Expr::Unary {
            op: U::Minus | U::Plus | U::BitNot,
            operand,
            ..
        } => sv_value_width(operand, env, source_db),
        Expr::Binary {
            left,
            op: B::Add | B::Sub | B::Mul | B::Div | B::Mod | B::BitAnd | B::BitOr | B::BitXor,
            right,
            ..
        } => sv_value_width(left, env, source_db).max(sv_value_width(right, env, source_db)),
        _ => 32,
    }
}

/// Returns a Verilog/SV literal's width: its explicit size, else 32 bits.
fn literal_width(text: &str) -> u32 {
    const_eval::parse_verilog_literal_with_width(text)
        .and_then(|(w, _)| w)
        .unwrap_or(32)
}

/// Looks up the bit width of a signal that is a plain packed vector.
///
/// Used to size SystemVerilog constructs whose width comes from their
/// assignment context (`'1`, `'x`, `'z`, `'{default: ...}`). Returns `None`
/// when the width is unknown or the signal is not a plain packed vector (for
/// example an unpacked array), in which case those constructs are reported as
/// unsupported instead of being guessed.
pub type WidthLookup<'a> = dyn Fn(SignalId) -> Option<u32> + 'a;

/// Builds the E210 diagnostic for a member/hierarchical selection on a
/// non-name base (`arr[2].valid`, `gen_blk[0].sig`, `inst[0].port`).
fn error_member_select(member: &str, span: Span) -> aion_diagnostics::Diagnostic {
    errors::error_unsupported(
        &format!(
            "member or hierarchical selection `.{member}` on an indexed or computed \
             expression (e.g. `arr[2].valid`, `gen_blk[0].sig`)"
        ),
        span,
    )
}

/// Lowers a Verilog AST expression to an IR expression.
///
/// Identifiers are resolved through `sig_env`. Unknown identifiers produce
/// an `E204` diagnostic and a poison zero literal.
pub fn lower_verilog_expr(
    expr: &aion_verilog_parser::ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    use aion_verilog_parser::ast::Expr;
    match expr {
        Expr::Identifier { name, span } => resolve_signal(*name, *span, sig_env, interner, sink),
        Expr::HierarchicalName { parts, span } => {
            // Use last part as the signal name
            if let Some(last) = parts.last() {
                resolve_signal(*last, *span, sig_env, interner, sink)
            } else {
                poison(*span)
            }
        }
        Expr::Literal { span } => lower_verilog_literal(*span, source_db),
        Expr::RealLiteral { span } => lower_verilog_literal(*span, source_db),
        Expr::StringLiteral { span: _ } => {
            // Strings → zero literal placeholder
            IrExpr::Literal(LogicVec::all_zero(1))
        }
        Expr::Unary { op, operand, span } => {
            let ir_operand = lower_verilog_expr(operand, sig_env, source_db, interner, sink);
            // Unary plus is the identity: lower to the operand itself.
            let Some(ir_op) = map_verilog_unary_op(*op) else {
                return ir_operand;
            };
            IrExpr::Unary {
                op: ir_op,
                operand: Box::new(ir_operand),
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Binary {
            left,
            op,
            right,
            span,
        } => {
            let ir_op = map_verilog_binary_op(*op);
            let lhs = lower_verilog_expr(left, sig_env, source_db, interner, sink);
            let rhs = lower_verilog_expr(right, sig_env, source_db, interner, sink);
            IrExpr::Binary {
                op: ir_op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            let cond = lower_verilog_expr(condition, sig_env, source_db, interner, sink);
            let t = lower_verilog_expr(then_expr, sig_env, source_db, interner, sink);
            let f = lower_verilog_expr(else_expr, sig_env, source_db, interner, sink);
            IrExpr::Ternary {
                condition: Box::new(cond),
                true_val: Box::new(t),
                false_val: Box::new(f),
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Concat { elements, span: _ } => {
            let parts: Vec<_> = elements
                .iter()
                .map(|e| lower_verilog_expr(e, sig_env, source_db, interner, sink))
                .collect();
            IrExpr::Concat(parts)
        }
        Expr::Repeat {
            count,
            elements,
            span,
        } => {
            // Evaluate count as constant
            let count_val =
                const_eval::eval_verilog_expr(count, source_db, interner, sig_env.consts(), sink)
                    .and_then(|v| const_eval::const_to_i64(&v))
                    .unwrap_or(1) as u32;
            let inner = if elements.len() == 1 {
                lower_verilog_expr(&elements[0], sig_env, source_db, interner, sink)
            } else {
                let parts: Vec<_> = elements
                    .iter()
                    .map(|e| lower_verilog_expr(e, sig_env, source_db, interner, sink))
                    .collect();
                IrExpr::Concat(parts)
            };
            IrExpr::Repeat {
                expr: Box::new(inner),
                count: count_val,
                span: *span,
            }
        }
        Expr::Index { base, index, span } => {
            let base_ir = lower_verilog_expr(base, sig_env, source_db, interner, sink);
            let idx_ir = lower_verilog_expr(index, sig_env, source_db, interner, sink);
            IrExpr::Index {
                expr: Box::new(base_ir),
                index: Box::new(idx_ir),
                span: *span,
            }
        }
        Expr::RangeSelect {
            base,
            msb,
            lsb,
            span,
        } => {
            let base_ir = lower_verilog_expr(base, sig_env, source_db, interner, sink);
            let hi = lower_verilog_expr(msb, sig_env, source_db, interner, sink);
            let lo = lower_verilog_expr(lsb, sig_env, source_db, interner, sink);
            IrExpr::Slice {
                expr: Box::new(base_ir),
                high: Box::new(hi),
                low: Box::new(lo),
                span: *span,
            }
        }
        Expr::PartSelect {
            base,
            index,
            ascending: _,
            width,
            span,
        } => {
            // Approximate as Slice — the exact semantics depend on ascending/descending
            let base_ir = lower_verilog_expr(base, sig_env, source_db, interner, sink);
            let idx = lower_verilog_expr(index, sig_env, source_db, interner, sink);
            let w = lower_verilog_expr(width, sig_env, source_db, interner, sink);
            IrExpr::Slice {
                expr: Box::new(base_ir),
                high: Box::new(idx),
                low: Box::new(w),
                span: *span,
            }
        }
        Expr::FuncCall { name, args, span } => {
            let func_name = extract_func_name(name, interner);
            let ir_args: Vec<_> = args
                .iter()
                .map(|a| lower_verilog_expr(a, sig_env, source_db, interner, sink))
                .collect();
            IrExpr::FuncCall {
                name: func_name,
                args: ir_args,
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::SystemCall { name, args, span } => {
            let ir_args: Vec<_> = args
                .iter()
                .map(|a| lower_verilog_expr(a, sig_env, source_db, interner, sink))
                .collect();
            IrExpr::FuncCall {
                name: *name,
                args: ir_args,
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Paren { inner, .. } => lower_verilog_expr(inner, sig_env, source_db, interner, sink),
        Expr::Member { member, span, .. } => {
            sink.emit(error_member_select(interner.resolve(*member), *span));
            poison(*span)
        }
        Expr::Error(span) => poison(*span),
    }
}

/// Lowers a SystemVerilog AST expression to an IR expression.
pub fn lower_sv_expr(
    expr: &aion_sv_parser::ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    use aion_sv_parser::ast::Expr;
    match expr {
        Expr::Identifier { name, span } => resolve_signal(*name, *span, sig_env, interner, sink),
        Expr::HierarchicalName { parts, span } => {
            if let Some(last) = parts.last() {
                resolve_signal(*last, *span, sig_env, interner, sink)
            } else {
                poison(*span)
            }
        }
        Expr::ScopedIdent { name, span, .. } => {
            resolve_signal(*name, *span, sig_env, interner, sink)
        }
        Expr::Literal { span } => lower_verilog_literal(*span, source_db),
        Expr::RealLiteral { span } => lower_verilog_literal(*span, source_db),
        Expr::TimeLiteral { span } => lower_sv_time_literal_value(*span, source_db, sink),
        Expr::StringLiteral { .. } => IrExpr::Literal(LogicVec::all_zero(1)),
        Expr::Unary { op, operand, span } => {
            if op.is_incr_decr() {
                // `++`/`--` have a side effect, which a pure IR expression
                // cannot express. Procedural assignments hoist them into
                // statements before reaching here (see `stmt::lower_sv_stmt`).
                sink.emit(errors::error_unsupported(
                    "`++`/`--` inside an expression; only the right-hand side \
                     of a procedural assignment (outside `?:`, `&&` and `||`) \
                     is supported",
                    *span,
                ));
                return poison(*span);
            }
            let ir_operand = lower_sv_expr(operand, sig_env, source_db, interner, sink);
            // Unary plus is the identity: lower to the operand itself.
            let Some(ir_op) = map_sv_unary_op(*op) else {
                return ir_operand;
            };
            IrExpr::Unary {
                op: ir_op,
                operand: Box::new(ir_operand),
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Binary {
            left,
            op,
            right,
            span,
        } => {
            let ir_op = map_sv_binary_op(*op);
            let lhs = lower_sv_expr(left, sig_env, source_db, interner, sink);
            let rhs = lower_sv_expr(right, sig_env, source_db, interner, sink);
            IrExpr::Binary {
                op: ir_op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            let cond = lower_sv_expr(condition, sig_env, source_db, interner, sink);
            let t = lower_sv_expr(then_expr, sig_env, source_db, interner, sink);
            let f = lower_sv_expr(else_expr, sig_env, source_db, interner, sink);
            IrExpr::Ternary {
                condition: Box::new(cond),
                true_val: Box::new(t),
                false_val: Box::new(f),
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Concat { elements, span: _ } => {
            let parts: Vec<_> = elements
                .iter()
                .map(|e| lower_sv_expr(e, sig_env, source_db, interner, sink))
                .collect();
            IrExpr::Concat(parts)
        }
        Expr::Repeat {
            count,
            elements,
            span,
        } => {
            let count_val =
                const_eval::eval_sv_expr(count, source_db, interner, sig_env.consts(), sink)
                    .and_then(|v| const_eval::const_to_i64(&v))
                    .unwrap_or(1) as u32;
            let inner = if elements.len() == 1 {
                lower_sv_expr(&elements[0], sig_env, source_db, interner, sink)
            } else {
                let parts: Vec<_> = elements
                    .iter()
                    .map(|e| lower_sv_expr(e, sig_env, source_db, interner, sink))
                    .collect();
                IrExpr::Concat(parts)
            };
            IrExpr::Repeat {
                expr: Box::new(inner),
                count: count_val,
                span: *span,
            }
        }
        Expr::Index { base, index, span } => {
            let base_ir = lower_sv_expr(base, sig_env, source_db, interner, sink);
            let idx_ir = lower_sv_expr(index, sig_env, source_db, interner, sink);
            IrExpr::Index {
                expr: Box::new(base_ir),
                index: Box::new(idx_ir),
                span: *span,
            }
        }
        Expr::RangeSelect {
            base,
            msb,
            lsb,
            span,
        } => {
            let base_ir = lower_sv_expr(base, sig_env, source_db, interner, sink);
            let hi = lower_sv_expr(msb, sig_env, source_db, interner, sink);
            let lo = lower_sv_expr(lsb, sig_env, source_db, interner, sink);
            IrExpr::Slice {
                expr: Box::new(base_ir),
                high: Box::new(hi),
                low: Box::new(lo),
                span: *span,
            }
        }
        Expr::PartSelect {
            base,
            index,
            width,
            span,
            ..
        } => {
            let base_ir = lower_sv_expr(base, sig_env, source_db, interner, sink);
            let idx = lower_sv_expr(index, sig_env, source_db, interner, sink);
            let w = lower_sv_expr(width, sig_env, source_db, interner, sink);
            IrExpr::Slice {
                expr: Box::new(base_ir),
                high: Box::new(idx),
                low: Box::new(w),
                span: *span,
            }
        }
        Expr::Inside { expr, .. } => {
            // Unsupported — lower the expression, ignore the ranges
            lower_sv_expr(expr, sig_env, source_db, interner, sink)
        }
        Expr::Cast { expr, .. } => {
            // Lower just the inner expression
            lower_sv_expr(expr, sig_env, source_db, interner, sink)
        }
        Expr::FuncCall { name, args, span } => {
            let func_name = extract_sv_func_name(name, interner);
            let ir_args: Vec<_> = args
                .iter()
                .map(|a| lower_sv_expr(a, sig_env, source_db, interner, sink))
                .collect();
            IrExpr::FuncCall {
                name: func_name,
                args: ir_args,
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::SystemCall { name, args, span } => {
            let ir_args: Vec<_> = args
                .iter()
                .map(|a| lower_sv_expr(a, sig_env, source_db, interner, sink))
                .collect();
            IrExpr::FuncCall {
                name: *name,
                args: ir_args,
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Paren { inner, .. } => lower_sv_expr(inner, sig_env, source_db, interner, sink),
        Expr::Member { member, span, .. } => {
            sink.emit(error_member_select(interner.resolve(*member), *span));
            poison(*span)
        }
        Expr::UnbasedUnsized { value, span } => lower_sv_fill(*value, None, *span, sink),
        Expr::AssignmentPattern { span, .. } => {
            sink.emit(errors::error_unsupported(
                "assignment pattern outside an assignment to a packed vector",
                *span,
            ));
            poison(*span)
        }
        Expr::Error(span) => poison(*span),
    }
}

/// Lowers the right-hand side of an SV assignment whose target is `target`.
///
/// Unlike [`lower_sv_expr`], this sizes context-dependent constructs from the
/// target width: unbased unsized literals (`'1`, `'x`, `'z`) and assignment
/// patterns (`'{default: v}`, positional `'{b3, b2, b1, b0}`), including when
/// they appear as a `?:` branch or inside parentheses. When the target width
/// cannot be determined these constructs get an E210 diagnostic rather than a
/// guessed width (`'0` is always correct as a 1-bit zero, which zero-extends).
#[allow(clippy::too_many_arguments)]
pub fn lower_sv_assign_value(
    value: &aion_sv_parser::ast::Expr,
    target: &SignalRef,
    widths: &WidthLookup<'_>,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    let ctx_width = signal_ref_width(target, widths);
    lower_sv_sized(value, ctx_width, sig_env, source_db, interner, sink)
}

/// Returns the bit width of an assignment target, if every part of it has a
/// known width (see [`WidthLookup`]).
///
/// A slice counts only when its signal is itself a plain packed vector, since
/// the IR flattens unpacked arrays and a constant select on one is not a bit
/// slice. A `Const` target (an unresolved or unsupported target) has no width.
pub fn signal_ref_width(target: &SignalRef, widths: &WidthLookup<'_>) -> Option<u32> {
    match target {
        SignalRef::Signal(sid) => widths(*sid),
        SignalRef::Slice { signal, high, low } => {
            widths(*signal)?;
            Some(high.abs_diff(*low) + 1)
        }
        SignalRef::Concat(parts) => parts
            .iter()
            .try_fold(0u32, |acc, p| acc.checked_add(signal_ref_width(p, widths)?)),
        SignalRef::Const(_) => None,
    }
}

/// Lowers `expr` in a context of width `ctx_width` (see [`lower_sv_assign_value`]).
fn lower_sv_sized(
    expr: &aion_sv_parser::ast::Expr,
    ctx_width: Option<u32>,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    use aion_sv_parser::ast::Expr;
    match expr {
        Expr::Paren { inner, .. } => {
            lower_sv_sized(inner, ctx_width, sig_env, source_db, interner, sink)
        }
        // The branches of `?:` are context-determined, so they inherit the
        // assignment width. (The condition is self-determined.)
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => IrExpr::Ternary {
            condition: Box::new(lower_sv_expr(condition, sig_env, source_db, interner, sink)),
            true_val: Box::new(lower_sv_sized(
                then_expr, ctx_width, sig_env, source_db, interner, sink,
            )),
            false_val: Box::new(lower_sv_sized(
                else_expr, ctx_width, sig_env, source_db, interner, sink,
            )),
            ty: TypeId::from_raw(0),
            span: *span,
        },
        Expr::UnbasedUnsized { value, span } => lower_sv_fill(*value, ctx_width, *span, sink),
        Expr::AssignmentPattern { items, span } => {
            lower_sv_assignment_pattern(items, ctx_width, *span, source_db, interner, sink)
        }
        _ => lower_sv_expr(expr, sig_env, source_db, interner, sink),
    }
}

/// Maps an SV fill value to the 4-state logic value of each bit.
fn fill_logic(value: aion_sv_parser::ast::FillValue) -> aion_common::Logic {
    use aion_common::Logic;
    use aion_sv_parser::ast::FillValue;
    match value {
        FillValue::Zero => Logic::Zero,
        FillValue::One => Logic::One,
        FillValue::X => Logic::X,
        FillValue::Z => Logic::Z,
    }
}

/// Builds a `width`-bit literal with every bit set to `bit`.
fn filled_literal(bit: aion_common::Logic, width: u32) -> LogicVec {
    let mut lv = LogicVec::all_zero(width.max(1));
    for i in 0..lv.width() {
        lv.set(i, bit);
    }
    lv
}

/// Lowers an unbased unsized literal (`'0`, `'1`, `'x`, `'z`).
///
/// With a known context width every bit is filled. Without one, `'0` lowers
/// to a 1-bit zero (correct, since zero-extension fills the rest with 0s);
/// `'1`/`'x`/`'z` get an E210 diagnostic, because a 1-bit literal would be
/// zero-extended and silently produce the wrong value.
fn lower_sv_fill(
    value: aion_sv_parser::ast::FillValue,
    ctx_width: Option<u32>,
    span: Span,
    sink: &DiagnosticSink,
) -> IrExpr {
    use aion_sv_parser::ast::FillValue;
    match (ctx_width, value) {
        (Some(w), _) => IrExpr::Literal(filled_literal(fill_logic(value), w)),
        (None, FillValue::Zero) => IrExpr::Literal(LogicVec::all_zero(1)),
        (None, _) => {
            let text = match value {
                FillValue::One => "'1",
                FillValue::X => "'x",
                _ => "'z",
            };
            sink.emit(errors::error_unsupported(
                &format!(
                    "`{text}` where its width cannot be determined; it is only \
                     supported as the value (or a `?:` branch) of an assignment \
                     to a packed vector"
                ),
                span,
            ));
            poison(span)
        }
    }
}

/// Evaluates an assignment pattern element value to the single bit it gives
/// a packed-vector element (its least significant bit), if it is constant.
fn sv_pattern_bit(
    expr: &aion_sv_parser::ast::Expr,
    source_db: &SourceDb,
    interner: &Interner,
) -> Option<aion_common::Logic> {
    use aion_common::Logic;
    use aion_sv_parser::ast::Expr;
    match expr {
        Expr::UnbasedUnsized { value, .. } => Some(fill_logic(*value)),
        Expr::Paren { inner, .. } => sv_pattern_bit(inner, source_db, interner),
        Expr::Literal { span } => {
            // Sized literals may carry x/z digits: take the literal's LSB.
            match lower_verilog_literal(*span, source_db) {
                IrExpr::Literal(lv) if lv.width() > 0 => Some(lv.get(0)),
                _ => None,
            }
        }
        _ => {
            // Constant expressions over parameters are not in scope here; a
            // scratch sink keeps "non-constant" noise out of the user's report.
            let scratch = DiagnosticSink::new();
            let v =
                const_eval::eval_sv_expr(expr, source_db, interner, &Default::default(), &scratch)
                    .and_then(|v| const_eval::const_to_i64(&v))?;
            Some(if v & 1 == 1 { Logic::One } else { Logic::Zero })
        }
    }
}

/// Lowers an assignment pattern assigned to a packed vector of `ctx_width` bits.
///
/// The elements of a one-dimensional packed vector are its bits, so
/// `'{default: v}` fills every bit with `v`'s LSB and a positional pattern
/// must list exactly one constant item per bit, MSB first. Keyed items,
/// non-constant items, and targets of unknown width get an E210 diagnostic.
fn lower_sv_assignment_pattern(
    items: &[aion_sv_parser::ast::PatternItem],
    ctx_width: Option<u32>,
    span: Span,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    use aion_sv_parser::ast::PatternItem;
    let unsupported = |what: &str| {
        sink.emit(errors::error_unsupported(what, span));
        poison(span)
    };
    let Some(width) = ctx_width else {
        return unsupported(
            "assignment pattern whose target is not a packed vector of known width \
             (unpacked arrays and structs are not supported yet)",
        );
    };
    if items.iter().any(|i| matches!(i, PatternItem::Keyed { .. })) {
        return unsupported("keyed assignment pattern items (`key: value`)");
    }
    if let [PatternItem::Default { value, .. }] = items {
        return match sv_pattern_bit(value, source_db, interner) {
            Some(bit) => IrExpr::Literal(filled_literal(bit, width)),
            None => unsupported("`'{default: v}` with a non-constant `v`"),
        };
    }
    let positional: Vec<_> = items
        .iter()
        .filter_map(|i| match i {
            PatternItem::Positional(e) => Some(e),
            _ => None,
        })
        .collect();
    if positional.len() != items.len() {
        // Mixed positional/default: already a parse error.
        return poison(span);
    }
    if positional.len() != width as usize {
        sink.emit(errors::error_type_mismatch(
            &format!(
                "assignment pattern has {} item(s) but the target has {width} bit(s)",
                positional.len()
            ),
            span,
        ));
        return poison(span);
    }
    let mut lv = LogicVec::all_zero(width);
    for (i, item) in positional.iter().enumerate() {
        let Some(bit) = sv_pattern_bit(item, source_db, interner) else {
            return unsupported("positional assignment pattern with non-constant items");
        };
        // Items are listed MSB first.
        lv.set(width - 1 - i as u32, bit);
    }
    IrExpr::Literal(lv)
}

/// Lowers a VHDL AST expression to an IR expression.
pub fn lower_vhdl_expr(
    expr: &aion_vhdl_parser::ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    use aion_vhdl_parser::ast::Expr;
    match expr {
        Expr::Name(name) => lower_vhdl_name(name, sig_env, source_db, interner, sink),
        Expr::IntLiteral { span } => lower_vhdl_literal(*span, source_db),
        Expr::RealLiteral { span } => lower_vhdl_literal(*span, source_db),
        Expr::CharLiteral { span } => {
            // Character literals like '0', '1' → single-bit logic
            let text = source_db.snippet(*span);
            let ch = text.trim_matches('\'');
            match ch {
                "0" => IrExpr::Literal(LogicVec::all_zero(1)),
                "1" => IrExpr::Literal(LogicVec::all_one(1)),
                _ => IrExpr::Literal(LogicVec::all_zero(1)),
            }
        }
        Expr::StringLiteral { .. } => IrExpr::Literal(LogicVec::all_zero(1)),
        Expr::BitStringLiteral { span } => {
            let text = source_db.snippet(*span);
            match parse_vhdl_bit_string(text) {
                Ok(lv) => IrExpr::Literal(lv),
                Err(msg) => {
                    sink.emit(errors::error_invalid_bit_string(&msg, *span));
                    IrExpr::Literal(LogicVec::all_zero(1))
                }
            }
        }
        Expr::Binary {
            left,
            op,
            right,
            span,
        } => {
            let ir_op = map_vhdl_binary_op(*op);
            let lhs = lower_vhdl_expr(left, sig_env, source_db, interner, sink);
            let rhs = lower_vhdl_expr(right, sig_env, source_db, interner, sink);
            IrExpr::Binary {
                op: ir_op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Unary { op, operand, span } => {
            let ir_operand = lower_vhdl_expr(operand, sig_env, source_db, interner, sink);
            // Unary plus is the identity: lower to the operand itself.
            let Some((ir_op, complement)) = map_vhdl_unary_op(*op) else {
                return ir_operand;
            };
            let reduced = IrExpr::Unary {
                op: ir_op,
                operand: Box::new(ir_operand),
                ty: TypeId::from_raw(0),
                span: *span,
            };
            if complement {
                // `nand v` / `nor v` / `xnor v` = not (and/or/xor v)
                IrExpr::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(reduced),
                    ty: TypeId::from_raw(0),
                    span: *span,
                }
            } else {
                reduced
            }
        }
        Expr::Paren { inner, .. } => lower_vhdl_expr(inner, sig_env, source_db, interner, sink),
        Expr::Aggregate { elements, span: _ } => {
            // Lower aggregate elements as a concat
            let parts: Vec<_> = elements
                .iter()
                .map(|e| lower_vhdl_expr(&e.value, sig_env, source_db, interner, sink))
                .collect();
            if parts.len() == 1 {
                parts.into_iter().next().unwrap()
            } else {
                IrExpr::Concat(parts)
            }
        }
        Expr::Qualified { expr, .. } => lower_vhdl_expr(expr, sig_env, source_db, interner, sink),
        Expr::TypeConversion { expr, .. } => {
            lower_vhdl_expr(expr, sig_env, source_db, interner, sink)
        }
        Expr::FunctionCall { name, args, span } => {
            let func_name = extract_vhdl_func_name(name, interner);
            let ir_args: Vec<_> = args
                .elements
                .iter()
                .map(|a| lower_vhdl_expr(&a.actual, sig_env, source_db, interner, sink))
                .collect();
            IrExpr::FuncCall {
                name: func_name,
                args: ir_args,
                ty: TypeId::from_raw(0),
                span: *span,
            }
        }
        Expr::Attribute { prefix, .. } => {
            // Lower the prefix; attribute handling is limited
            lower_vhdl_expr(prefix, sig_env, source_db, interner, sink)
        }
        Expr::Others { span: _ } => IrExpr::Literal(LogicVec::all_zero(1)),
        Expr::Open { span: _ } => IrExpr::Literal(LogicVec::all_zero(1)),
        Expr::Error(span) => poison(*span),
    }
}

/// Converts a Verilog AST expression into a [`SignalRef`] for use as an
/// assignment target. Handles identifiers, bit/range selects, and concatenations.
pub fn lower_to_signal_ref(
    expr: &aion_verilog_parser::ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> SignalRef {
    use aion_verilog_parser::ast::Expr;
    match expr {
        Expr::Identifier { name, span } => {
            if let Some(&sid) = sig_env.get(name) {
                SignalRef::Signal(sid)
            } else {
                sink.emit(errors::error_unknown_signal(interner.resolve(*name), *span));
                SignalRef::Const(LogicVec::all_zero(1))
            }
        }
        Expr::Index { base, index, .. } => {
            if let Some(sid) = extract_base_signal_verilog(base, sig_env) {
                if let Some(idx) = try_const_index_verilog(index, sig_env, source_db, interner) {
                    SignalRef::Slice {
                        signal: sid,
                        high: idx,
                        low: idx,
                    }
                } else {
                    SignalRef::Signal(sid)
                }
            } else {
                SignalRef::Const(LogicVec::all_zero(1))
            }
        }
        Expr::RangeSelect { base, msb, lsb, .. } => {
            if let Some(sid) = extract_base_signal_verilog(base, sig_env) {
                if let (Some(hi), Some(lo)) = (
                    try_const_index_verilog(msb, sig_env, source_db, interner),
                    try_const_index_verilog(lsb, sig_env, source_db, interner),
                ) {
                    SignalRef::Slice {
                        signal: sid,
                        high: hi,
                        low: lo,
                    }
                } else {
                    SignalRef::Signal(sid)
                }
            } else {
                SignalRef::Const(LogicVec::all_zero(1))
            }
        }
        Expr::Concat { elements, .. } => {
            let parts: Vec<_> = elements
                .iter()
                .map(|e| lower_to_signal_ref(e, sig_env, source_db, interner, sink))
                .collect();
            SignalRef::Concat(parts)
        }
        Expr::Member { member, span, .. } => {
            sink.emit(error_member_select(interner.resolve(*member), *span));
            SignalRef::Const(LogicVec::all_zero(1))
        }
        _ => SignalRef::Const(LogicVec::all_zero(1)),
    }
}

/// Converts an SV AST expression into a [`SignalRef`] for assignment targets.
/// Handles identifiers, bit/range selects, and concatenations.
pub fn lower_sv_to_signal_ref(
    expr: &aion_sv_parser::ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> SignalRef {
    use aion_sv_parser::ast::Expr;
    match expr {
        Expr::Identifier { name, span } => {
            if let Some(&sid) = sig_env.get(name) {
                SignalRef::Signal(sid)
            } else {
                sink.emit(errors::error_unknown_signal(interner.resolve(*name), *span));
                SignalRef::Const(LogicVec::all_zero(1))
            }
        }
        Expr::Index { base, index, .. } => {
            if let Some(sid) = extract_base_signal_sv(base, sig_env) {
                if let Some(idx) = try_const_index_sv(index, sig_env, source_db, interner) {
                    SignalRef::Slice {
                        signal: sid,
                        high: idx,
                        low: idx,
                    }
                } else {
                    SignalRef::Signal(sid)
                }
            } else {
                SignalRef::Const(LogicVec::all_zero(1))
            }
        }
        Expr::RangeSelect { base, msb, lsb, .. } => {
            if let Some(sid) = extract_base_signal_sv(base, sig_env) {
                if let (Some(hi), Some(lo)) = (
                    try_const_index_sv(msb, sig_env, source_db, interner),
                    try_const_index_sv(lsb, sig_env, source_db, interner),
                ) {
                    SignalRef::Slice {
                        signal: sid,
                        high: hi,
                        low: lo,
                    }
                } else {
                    SignalRef::Signal(sid)
                }
            } else {
                SignalRef::Const(LogicVec::all_zero(1))
            }
        }
        Expr::Concat { elements, .. } => {
            let parts: Vec<_> = elements
                .iter()
                .map(|e| lower_sv_to_signal_ref(e, sig_env, source_db, interner, sink))
                .collect();
            SignalRef::Concat(parts)
        }
        Expr::Member { member, span, .. } => {
            sink.emit(error_member_select(interner.resolve(*member), *span));
            SignalRef::Const(LogicVec::all_zero(1))
        }
        // A port actual of `'0` is a zero constant at any width.
        Expr::UnbasedUnsized {
            value: aion_sv_parser::ast::FillValue::Zero,
            ..
        } => SignalRef::Const(LogicVec::all_zero(1)),
        Expr::UnbasedUnsized { span, .. } | Expr::AssignmentPattern { span, .. } => {
            sink.emit(errors::error_unsupported(
                "`'1`/`'x`/`'z` or an assignment pattern as an assignment target or \
                 port connection",
                *span,
            ));
            SignalRef::Const(LogicVec::all_zero(1))
        }
        _ => SignalRef::Const(LogicVec::all_zero(1)),
    }
}

/// Extracts the base [`SignalId`] from a Verilog `Identifier` expression.
fn extract_base_signal_verilog(
    expr: &aion_verilog_parser::ast::Expr,
    sig_env: &SignalEnv,
) -> Option<SignalId> {
    if let aion_verilog_parser::ast::Expr::Identifier { name, .. } = expr {
        sig_env.get(name).copied()
    } else {
        None
    }
}

/// Extracts the base [`SignalId`] from an SV `Identifier` expression.
fn extract_base_signal_sv(
    expr: &aion_sv_parser::ast::Expr,
    sig_env: &SignalEnv,
) -> Option<SignalId> {
    if let aion_sv_parser::ast::Expr::Identifier { name, .. } = expr {
        sig_env.get(name).copied()
    } else {
        None
    }
}

/// Tries to const-evaluate a Verilog expression to a `u32` index value.
///
/// Literals and constant expressions over the parameters in `sig_env`
/// (`q[IDLE]`, `q[W-1:0]`) evaluate; anything else (a signal index) returns
/// `None` without diagnostics, since a non-constant index is legal there.
fn try_const_index_verilog(
    expr: &aion_verilog_parser::ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
) -> Option<u32> {
    if let aion_verilog_parser::ast::Expr::Identifier { name, .. } = expr {
        if sig_env.get(name).is_some() {
            return None; // a signal shadows a parameter of the same name
        }
    }
    let scratch = DiagnosticSink::new();
    const_eval::eval_verilog_expr(expr, source_db, interner, sig_env.consts(), &scratch)
        .and_then(|v| const_eval::const_to_i64(&v))
        .and_then(|v| u32::try_from(v).ok())
}

/// Tries to const-evaluate an SV expression to a `u32` index value.
///
/// Same rules as [`try_const_index_verilog`].
fn try_const_index_sv(
    expr: &aion_sv_parser::ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
) -> Option<u32> {
    if let aion_sv_parser::ast::Expr::Identifier { name, .. } = expr {
        if sig_env.get(name).is_some() {
            return None; // a signal shadows a parameter of the same name
        }
    }
    let scratch = DiagnosticSink::new();
    const_eval::eval_sv_expr(expr, source_db, interner, sig_env.consts(), &scratch)
        .and_then(|v| const_eval::const_to_i64(&v))
        .and_then(|v| u32::try_from(v).ok())
}

/// Converts a VHDL AST expression into a [`SignalRef`] for assignment targets.
pub fn lower_vhdl_to_signal_ref(
    expr: &aion_vhdl_parser::ast::Expr,
    sig_env: &SignalEnv,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> SignalRef {
    use aion_vhdl_parser::ast::Expr;
    match expr {
        Expr::Name(name) => {
            if let Some(&sid) = sig_env.get(&name.primary) {
                SignalRef::Signal(sid)
            } else {
                sink.emit(errors::error_unknown_signal(
                    interner.resolve(name.primary),
                    name.span,
                ));
                SignalRef::Const(LogicVec::all_zero(1))
            }
        }
        // Positional aggregate target `(a, b) <= v;`: elements take the
        // value's elements left to right, exactly like the concatenation
        // `a & b`, so it lowers to a concat whose first part is the MSB.
        Expr::Aggregate { elements, span } => {
            if elements.iter().any(|e| !e.choices.is_empty()) {
                sink.emit(errors::error_unsupported(
                    "named association in an aggregate assignment target",
                    *span,
                ));
                return SignalRef::Const(LogicVec::all_zero(1));
            }
            SignalRef::Concat(
                elements
                    .iter()
                    .map(|e| lower_vhdl_to_signal_ref(&e.value, sig_env, interner, sink))
                    .collect(),
            )
        }
        _ => SignalRef::Const(LogicVec::all_zero(1)),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolves an identifier to an IR signal reference or, for a name that is
/// not a signal but a constant in scope, to an [`IrExpr::NamedConst`] of the
/// constant's value and width. Emits an E204 diagnostic for a name that is
/// neither.
fn resolve_signal(
    name: Ident,
    span: Span,
    sig_env: &SignalEnv,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    if let Some(&sid) = sig_env.get(&name) {
        IrExpr::Signal(SignalRef::Signal(sid))
    } else if let Some(value) = sig_env.const_literal(name) {
        IrExpr::NamedConst { name, value }
    } else {
        sink.emit(errors::error_unknown_signal(interner.resolve(name), span));
        poison(span)
    }
}

/// Lowers a SystemVerilog time literal used as a value (`x = 5ns;`,
/// `$time > 10ns`) to a 64-bit count of time units.
///
/// The elaborator assumes a 1 ns time unit (see
/// [`DEFAULT_TIMESCALE_FS`](crate::stmt::DEFAULT_TIMESCALE_FS)), so `10ns`
/// lowers to `10`. A literal that is not a whole number of nanoseconds
/// (`100ps`, `1step`) is rounded half-up with a W207 warning; an
/// out-of-range literal is an E209 error and lowers to 0.
fn lower_sv_time_literal_value(span: Span, source_db: &SourceDb, sink: &DiagnosticSink) -> IrExpr {
    let text = source_db.snippet(span);
    let unit = crate::stmt::DEFAULT_TIMESCALE_FS;
    let count = match const_eval::parse_time_literal(text) {
        Some(const_eval::TimeLiteralValue::Fs { fs, exact }) => {
            let count = fs / unit + u64::from(fs % unit >= unit / 2);
            if !exact || fs % unit != 0 {
                sink.emit(errors::warn_time_literal_assumed(
                    text,
                    &format!(
                        "not a whole number of the assumed 1 ns time unit; rounded to {count}"
                    ),
                    span,
                ));
            }
            count
        }
        Some(const_eval::TimeLiteralValue::Step) => {
            sink.emit(errors::warn_time_literal_assumed(
                text,
                "`1step` depends on the unknown time precision; it rounds to 0 time units",
                span,
            ));
            0
        }
        None => {
            sink.emit(errors::error_param_not_const(
                &format!("time literal `{text}` is out of range"),
                span,
            ));
            0
        }
    };
    IrExpr::Literal(logic_vec_from_u64(64, count))
}

/// Produces a poison zero-width literal for error recovery.
fn poison(_span: Span) -> IrExpr {
    IrExpr::Literal(LogicVec::all_zero(1))
}

/// Creates a `LogicVec` from a `u64` value with the given width.
pub(crate) fn logic_vec_from_u64(width: u32, val: u64) -> LogicVec {
    use aion_common::Logic;
    let mut lv = LogicVec::all_zero(width);
    for i in 0..width.min(64) {
        if (val >> i) & 1 == 1 {
            lv.set(i, Logic::One);
        }
    }
    lv
}

/// Parses a Verilog/SV numeric literal from source text into a `LogicVec`.
///
/// For sized literals like `24'h000000`, the explicit width is used.
/// For unsized literals like `42`, width is inferred from the value.
fn lower_verilog_literal(span: Span, source_db: &SourceDb) -> IrExpr {
    let text = source_db.snippet(span);
    if let Some((explicit_width, val)) = const_eval::parse_verilog_literal_with_width(text) {
        let width = if let Some(w) = explicit_width {
            w
        } else if val == 0 {
            1
        } else {
            64 - val.unsigned_abs().leading_zeros()
        };
        let width = width.max(1);
        IrExpr::Literal(logic_vec_from_u64(width, val as u64))
    } else {
        IrExpr::Literal(LogicVec::all_zero(32))
    }
}

/// Parses a VHDL integer literal from source text into a `LogicVec`.
fn lower_vhdl_literal(span: Span, source_db: &SourceDb) -> IrExpr {
    let text = source_db.snippet(span).replace('_', "");
    if let Ok(val) = text.parse::<i64>() {
        let width = if val == 0 {
            1
        } else {
            64 - val.unsigned_abs().leading_zeros()
        };
        let width = width.max(1);
        IrExpr::Literal(logic_vec_from_u64(width, val as u64))
    } else {
        IrExpr::Literal(LogicVec::all_zero(32))
    }
}

/// Parses a VHDL bit string literal into a `LogicVec`.
///
/// Handles every VHDL-2008 form: unsized `X"FF"`, `B"1010"`, `O"17"`,
/// `UX`/`SX`/`UB`/`SB`/`UO`/`SO` prefixes, decimal `D"255"`, and sized
/// literals such as `8UX"F"` (zero-extended to `00001111`), `8SX"F"`
/// (sign-extended to `11111111`), and `16D"255"`. Expansion, padding, and
/// truncation follow IEEE 1076-2008 §15.8 via
/// [`aion_vhdl_parser::bit_string::expand_bit_string`]. Elements map to
/// 4-state values: `0`/`L` → 0, `1`/`H` → 1, `Z` → Z, and `U`/`X`/`W`/`-`
/// → X. The leftmost element is the most significant bit.
///
/// Returns an error message for a malformed literal (including a truncation
/// that would drop significant bits), a null literal (`X""`), or an element
/// with no 4-state equivalent.
pub(crate) fn parse_vhdl_bit_string(text: &str) -> Result<LogicVec, String> {
    let expanded = aion_vhdl_parser::bit_string::expand_bit_string(text).map_err(|e| e.message)?;
    let bits: Vec<char> = expanded.chars().collect();
    if bits.is_empty() {
        return Err("null (zero-width) bit string literals are not supported".to_string());
    }
    let width = bits.len() as u32;
    let mut lv = LogicVec::new(width);
    for (i, &c) in bits.iter().enumerate() {
        let bit = crate::vhdl::std_logic_char(c)
            .ok_or_else(|| format!("element '{c}' has no 4-state logic equivalent"))?;
        lv.set(width - 1 - i as u32, bit);
    }
    Ok(lv)
}

/// Known VHDL built-in functions and type conversion names.
///
/// When the VHDL parser encounters `rising_edge(clk)` or `std_logic_vector(x)`,
/// it emits them as `Name { primary, parts: [Index(...)] }`. Without this list,
/// `lower_vhdl_name` would try to resolve these as signals and emit E204.
const VHDL_BUILTINS: &[&str] = &[
    "rising_edge",
    "falling_edge",
    "to_unsigned",
    "to_signed",
    "to_integer",
    "unsigned",
    "signed",
    "std_logic_vector",
    "std_ulogic_vector",
    "resize",
    "shift_left",
    "shift_right",
    "to_stdulogicvector",
    "to_stdlogicvector",
    "to_bitvector",
    "integer",
    "natural",
    "positive",
];

/// Returns `true` if `name` is a known VHDL built-in function or type name.
fn is_vhdl_builtin(name: &str) -> bool {
    let lower = name.to_lowercase();
    VHDL_BUILTINS.iter().any(|b| *b == lower)
}

/// Lowers a VHDL `Name` to an IR expression.
fn lower_vhdl_name(
    name: &aion_vhdl_parser::ast::Name,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    use aion_vhdl_parser::ast::NameSuffix;

    let primary_text = interner.resolve(name.primary);

    // Check if the primary is a VHDL built-in function or type conversion
    if is_vhdl_builtin(primary_text) {
        // If there are Index suffix parts, treat as a function call with arguments
        if let Some(NameSuffix::Index(args, span)) = name.parts.first() {
            let ir_args: Vec<_> = args
                .iter()
                .map(|a| lower_vhdl_expr(a, sig_env, source_db, interner, sink))
                .collect();
            return IrExpr::FuncCall {
                name: name.primary,
                args: ir_args,
                ty: TypeId::from_raw(0),
                span: *span,
            };
        }
        // No arguments — return a passthrough zero literal (type name used as value)
        return IrExpr::Literal(LogicVec::all_zero(1));
    }

    let base = resolve_signal(name.primary, name.span, sig_env, interner, sink);

    if name.parts.is_empty() {
        return base;
    }

    // Apply suffixes
    let mut result = base;
    for suffix in &name.parts {
        match suffix {
            NameSuffix::Index(exprs, span) => {
                if let Some(idx_expr) = exprs.first() {
                    let idx = lower_vhdl_expr(idx_expr, sig_env, source_db, interner, sink);
                    result = IrExpr::Index {
                        expr: Box::new(result),
                        index: Box::new(idx),
                        span: *span,
                    };
                }
            }
            NameSuffix::Slice(rc, span) => {
                let hi = lower_vhdl_expr(&rc.left, sig_env, source_db, interner, sink);
                let lo = lower_vhdl_expr(&rc.right, sig_env, source_db, interner, sink);
                result = IrExpr::Slice {
                    expr: Box::new(result),
                    high: Box::new(hi),
                    low: Box::new(lo),
                    span: *span,
                };
            }
            NameSuffix::Selected(_, _) | NameSuffix::Attribute(_, _, _) | NameSuffix::All(_) => {
                // These are handled elsewhere or ignored at this stage
            }
        }
    }
    result
}

/// Extracts a function name from a Verilog func call expression.
fn extract_func_name(expr: &aion_verilog_parser::ast::Expr, interner: &Interner) -> Ident {
    use aion_verilog_parser::ast::Expr;
    match expr {
        Expr::Identifier { name, .. } => *name,
        _ => interner.get_or_intern("<unknown>"),
    }
}

/// Extracts a function name from an SV func call expression.
fn extract_sv_func_name(expr: &aion_sv_parser::ast::Expr, interner: &Interner) -> Ident {
    use aion_sv_parser::ast::Expr;
    match expr {
        Expr::Identifier { name, .. } => *name,
        Expr::ScopedIdent { name, .. } => *name,
        _ => interner.get_or_intern("<unknown>"),
    }
}

/// Extracts a function name from a VHDL func call expression.
fn extract_vhdl_func_name(expr: &aion_vhdl_parser::ast::Expr, interner: &Interner) -> Ident {
    use aion_vhdl_parser::ast::Expr;
    match expr {
        Expr::Name(name) => name.primary,
        _ => interner.get_or_intern("<unknown>"),
    }
}

// ---------------------------------------------------------------------------
// Operator mapping
// ---------------------------------------------------------------------------

/// Maps a Verilog unary operator to an IR unary operator.
///
/// Returns `None` for unary plus, which is the identity and must be lowered
/// to its operand with no IR node.
fn map_verilog_unary_op(op: aion_verilog_parser::ast::UnaryOp) -> Option<UnaryOp> {
    use aion_verilog_parser::ast::UnaryOp as V;
    Some(match op {
        V::Plus => return None,
        V::Minus => UnaryOp::Neg,
        V::LogNot => UnaryOp::LogicNot,
        V::BitNot => UnaryOp::Not,
        V::RedAnd | V::RedNand => UnaryOp::RedAnd,
        V::RedOr | V::RedNor => UnaryOp::RedOr,
        V::RedXor | V::RedXnor => UnaryOp::RedXor,
    })
}

/// Maps a Verilog binary operator to an IR binary operator.
fn map_verilog_binary_op(op: aion_verilog_parser::ast::BinaryOp) -> BinaryOp {
    use aion_verilog_parser::ast::BinaryOp as V;
    match op {
        V::Add => BinaryOp::Add,
        V::Sub => BinaryOp::Sub,
        V::Mul => BinaryOp::Mul,
        V::Div => BinaryOp::Div,
        V::Mod => BinaryOp::Mod,
        V::Pow => BinaryOp::Pow,
        V::Eq | V::CaseEq => BinaryOp::Eq,
        V::Neq | V::CaseNeq => BinaryOp::Ne,
        V::Lt => BinaryOp::Lt,
        V::Le => BinaryOp::Le,
        V::Gt => BinaryOp::Gt,
        V::Ge => BinaryOp::Ge,
        V::LogAnd => BinaryOp::LogicAnd,
        V::LogOr => BinaryOp::LogicOr,
        V::BitAnd => BinaryOp::And,
        V::BitOr => BinaryOp::Or,
        V::BitXor | V::BitXnor => BinaryOp::Xor,
        V::Shl | V::AShl => BinaryOp::Shl,
        V::Shr | V::AShr => BinaryOp::Shr,
    }
}

/// Maps a SystemVerilog unary operator to an IR unary operator.
///
/// Returns `None` for unary plus, which is the identity and must be lowered
/// to its operand with no IR node. Also returns `None` for the side-effecting
/// `++`/`--` operators, which have no pure IR equivalent; callers must handle
/// those first (see [`aion_sv_parser::ast::UnaryOp::is_incr_decr`]), so that
/// `None` after that check always means unary plus.
fn map_sv_unary_op(op: aion_sv_parser::ast::UnaryOp) -> Option<UnaryOp> {
    use aion_sv_parser::ast::UnaryOp as S;
    Some(match op {
        S::Plus => return None,
        S::Minus => UnaryOp::Neg,
        S::LogNot => UnaryOp::LogicNot,
        S::BitNot => UnaryOp::Not,
        S::RedAnd | S::RedNand => UnaryOp::RedAnd,
        S::RedOr | S::RedNor => UnaryOp::RedOr,
        S::RedXor | S::RedXnor => UnaryOp::RedXor,
        S::PreIncr | S::PreDecr | S::PostIncr | S::PostDecr => return None,
    })
}

/// Maps a SystemVerilog binary operator to an IR binary operator.
fn map_sv_binary_op(op: aion_sv_parser::ast::BinaryOp) -> BinaryOp {
    use aion_sv_parser::ast::BinaryOp as S;
    match op {
        S::Add => BinaryOp::Add,
        S::Sub => BinaryOp::Sub,
        S::Mul => BinaryOp::Mul,
        S::Div => BinaryOp::Div,
        S::Mod => BinaryOp::Mod,
        S::Pow => BinaryOp::Pow,
        S::Eq | S::CaseEq | S::WildEq => BinaryOp::Eq,
        S::Neq | S::CaseNeq | S::WildNeq => BinaryOp::Ne,
        S::Lt => BinaryOp::Lt,
        S::Le => BinaryOp::Le,
        S::Gt => BinaryOp::Gt,
        S::Ge => BinaryOp::Ge,
        S::LogAnd => BinaryOp::LogicAnd,
        S::LogOr => BinaryOp::LogicOr,
        S::BitAnd => BinaryOp::And,
        S::BitOr => BinaryOp::Or,
        S::BitXor | S::BitXnor => BinaryOp::Xor,
        S::Shl | S::AShl => BinaryOp::Shl,
        S::Shr | S::AShr => BinaryOp::Shr,
    }
}

/// Maps a VHDL binary operator to an IR binary operator.
fn map_vhdl_binary_op(op: aion_vhdl_parser::ast::BinaryOp) -> BinaryOp {
    use aion_vhdl_parser::ast::BinaryOp as V;
    match op {
        V::And | V::Nand => BinaryOp::And,
        V::Or | V::Nor => BinaryOp::Or,
        V::Xor | V::Xnor => BinaryOp::Xor,
        V::Eq | V::MatchEq => BinaryOp::Eq,
        V::Neq | V::MatchNeq => BinaryOp::Ne,
        V::Lt | V::MatchLt => BinaryOp::Lt,
        V::Le | V::MatchLe => BinaryOp::Le,
        V::Gt | V::MatchGt => BinaryOp::Gt,
        V::Ge | V::MatchGe => BinaryOp::Ge,
        V::Sll | V::Sla | V::Rol => BinaryOp::Shl,
        V::Srl | V::Sra | V::Ror => BinaryOp::Shr,
        V::Add => BinaryOp::Add,
        V::Sub => BinaryOp::Sub,
        V::Concat => BinaryOp::And, // VHDL `&` is concat; approximate as concat below
        V::Mul => BinaryOp::Mul,
        V::Div => BinaryOp::Div,
        V::Mod | V::Rem2 => BinaryOp::Mod,
        V::Pow => BinaryOp::Pow,
    }
}

/// Maps a VHDL unary operator to an IR unary operator.
///
/// Returns the IR operator plus a flag that is `true` when the result must be
/// complemented with an outer `Not`: the IR has no NAND/NOR/XNOR reductions,
/// so the VHDL-2008 `nand v`, `nor v` and `xnor v` reductions lower to
/// `not (and v)`, `not (or v)` and `not (xor v)`.
///
/// Returns `None` for unary `+`, which is the identity and must be lowered
/// to its operand with no IR node.
fn map_vhdl_unary_op(op: aion_vhdl_parser::ast::UnaryOp) -> Option<(UnaryOp, bool)> {
    use aion_vhdl_parser::ast::UnaryOp as V;
    Some(match op {
        V::Not => (UnaryOp::Not, false),
        V::Abs => (UnaryOp::Neg, false), // approximate
        V::Pos => return None,
        V::Neg => (UnaryOp::Neg, false),
        // VHDL-2008 condition operator (??) treated as boolean test
        V::Condition => (UnaryOp::Not, false),
        V::RedAnd => (UnaryOp::RedAnd, false),
        V::RedOr => (UnaryOp::RedOr, false),
        V::RedXor => (UnaryOp::RedXor, false),
        V::RedNand => (UnaryOp::RedAnd, true),
        V::RedNor => (UnaryOp::RedOr, true),
        V::RedXnor => (UnaryOp::RedXor, true),
    })
}

/// Maps a SystemVerilog compound operator to an IR binary operator.
pub fn map_sv_compound_op(op: aion_sv_parser::ast::CompoundOp) -> BinaryOp {
    use aion_sv_parser::ast::CompoundOp as C;
    match op {
        C::Add => BinaryOp::Add,
        C::Sub => BinaryOp::Sub,
        C::Mul => BinaryOp::Mul,
        C::Div => BinaryOp::Div,
        C::Mod => BinaryOp::Mod,
        C::BitAnd => BinaryOp::And,
        C::BitOr => BinaryOp::Or,
        C::BitXor => BinaryOp::Xor,
        C::Shl | C::AShl => BinaryOp::Shl,
        C::Shr | C::AShr => BinaryOp::Shr,
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
    fn verilog_identifier_found() {
        let (sdb, interner, sink, mut env) = setup();
        let clk = interner.get_or_intern("clk");
        let sid = SignalId::from_raw(0);
        env.insert(clk, sid);

        let ast_expr = aion_verilog_parser::ast::Expr::Identifier {
            name: clk,
            span: Span::DUMMY,
        };
        let ir = lower_verilog_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Signal(SignalRef::Signal(s)) if s == sid));
        assert!(!sink.has_errors());
    }

    #[test]
    fn verilog_identifier_unknown() {
        let (sdb, interner, sink, env) = setup();
        let unknown = interner.get_or_intern("unknown");

        let ast_expr = aion_verilog_parser::ast::Expr::Identifier {
            name: unknown,
            span: Span::DUMMY,
        };
        let ir = lower_verilog_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Literal(_)));
        assert!(sink.has_errors());
    }

    #[test]
    fn verilog_literal_parses() {
        let (mut sdb, interner, sink, env) = setup();
        let fid = sdb.add_source("test.v", "42".to_string());
        let ast_expr = aion_verilog_parser::ast::Expr::Literal {
            span: aion_source::Span::new(fid, 0, 2),
        };
        let ir = lower_verilog_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Literal(_)));
    }

    #[test]
    fn sized_literal_preserves_width_24bit_zero() {
        let (mut sdb, _interner, _sink, _env) = setup();
        let fid = sdb.add_source("test.v", "24'h000000".to_string());
        let span = aion_source::Span::new(fid, 0, 10);
        let ir = lower_verilog_literal(span, &sdb);
        if let IrExpr::Literal(lv) = &ir {
            assert_eq!(lv.width(), 24, "24'h000000 should produce 24-bit LogicVec");
        } else {
            panic!("expected Literal");
        }
    }

    #[test]
    fn sized_literal_preserves_width_8bit_zero() {
        let (mut sdb, _interner, _sink, _env) = setup();
        let fid = sdb.add_source("test.v", "8'h00".to_string());
        let span = aion_source::Span::new(fid, 0, 5);
        let ir = lower_verilog_literal(span, &sdb);
        if let IrExpr::Literal(lv) = &ir {
            assert_eq!(lv.width(), 8, "8'h00 should produce 8-bit LogicVec");
        } else {
            panic!("expected Literal");
        }
    }

    #[test]
    fn sized_literal_preserves_width_24bit_one() {
        let (mut sdb, _interner, _sink, _env) = setup();
        let fid = sdb.add_source("test.v", "24'h000001".to_string());
        let span = aion_source::Span::new(fid, 0, 10);
        let ir = lower_verilog_literal(span, &sdb);
        if let IrExpr::Literal(lv) = &ir {
            assert_eq!(lv.width(), 24, "24'h000001 should produce 24-bit LogicVec");
        } else {
            panic!("expected Literal");
        }
    }

    #[test]
    fn unsized_literal_infers_width() {
        let (mut sdb, _interner, _sink, _env) = setup();
        let fid = sdb.add_source("test.v", "42".to_string());
        let span = aion_source::Span::new(fid, 0, 2);
        let ir = lower_verilog_literal(span, &sdb);
        if let IrExpr::Literal(lv) = &ir {
            // 42 = 0b101010, needs 6 bits
            assert_eq!(lv.width(), 6, "42 should produce 6-bit LogicVec");
        } else {
            panic!("expected Literal");
        }
    }

    #[test]
    fn verilog_binary_op() {
        let (sdb, interner, sink, mut env) = setup();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        env.insert(a, SignalId::from_raw(0));
        env.insert(b, SignalId::from_raw(1));

        let ast_expr = aion_verilog_parser::ast::Expr::Binary {
            left: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: a,
                span: Span::DUMMY,
            }),
            op: aion_verilog_parser::ast::BinaryOp::Add,
            right: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: b,
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let ir = lower_verilog_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(
            ir,
            IrExpr::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));
    }

    #[test]
    fn verilog_ternary() {
        let (sdb, interner, sink, mut env) = setup();
        let c = interner.get_or_intern("c");
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        env.insert(c, SignalId::from_raw(0));
        env.insert(a, SignalId::from_raw(1));
        env.insert(b, SignalId::from_raw(2));

        let ast_expr = aion_verilog_parser::ast::Expr::Ternary {
            condition: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: c,
                span: Span::DUMMY,
            }),
            then_expr: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: a,
                span: Span::DUMMY,
            }),
            else_expr: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: b,
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let ir = lower_verilog_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Ternary { .. }));
    }

    #[test]
    fn verilog_concat() {
        let (sdb, interner, sink, mut env) = setup();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        env.insert(a, SignalId::from_raw(0));
        env.insert(b, SignalId::from_raw(1));

        let ast_expr = aion_verilog_parser::ast::Expr::Concat {
            elements: vec![
                aion_verilog_parser::ast::Expr::Identifier {
                    name: a,
                    span: Span::DUMMY,
                },
                aion_verilog_parser::ast::Expr::Identifier {
                    name: b,
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let ir = lower_verilog_expr(&ast_expr, &env, &sdb, &interner, &sink);
        if let IrExpr::Concat(parts) = &ir {
            assert_eq!(parts.len(), 2);
        } else {
            panic!("expected Concat");
        }
    }

    #[test]
    fn sv_identifier_found() {
        let (sdb, interner, sink, mut env) = setup();
        let clk = interner.get_or_intern("clk");
        let sid = SignalId::from_raw(0);
        env.insert(clk, sid);

        let ast_expr = aion_sv_parser::ast::Expr::Identifier {
            name: clk,
            span: Span::DUMMY,
        };
        let ir = lower_sv_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Signal(SignalRef::Signal(s)) if s == sid));
    }

    #[test]
    fn vhdl_name_resolves() {
        let (sdb, interner, sink, mut env) = setup();
        let clk = interner.get_or_intern("clk");
        let sid = SignalId::from_raw(0);
        env.insert(clk, sid);

        let ast_expr = aion_vhdl_parser::ast::Expr::Name(aion_vhdl_parser::ast::Name {
            primary: clk,
            parts: vec![],
            span: Span::DUMMY,
        });
        let ir = lower_vhdl_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Signal(SignalRef::Signal(s)) if s == sid));
    }

    #[test]
    fn vhdl_char_literal() {
        let (mut sdb, interner, sink, env) = setup();
        let fid = sdb.add_source("test.vhd", "'1'".to_string());
        let ast_expr = aion_vhdl_parser::ast::Expr::CharLiteral {
            span: aion_source::Span::new(fid, 0, 3),
        };
        let ir = lower_vhdl_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Literal(_)));
    }

    #[test]
    fn signal_ref_lowering() {
        let (sdb, interner, sink, mut env) = setup();
        let a = interner.get_or_intern("a");
        env.insert(a, SignalId::from_raw(5));

        let ast_expr = aion_verilog_parser::ast::Expr::Identifier {
            name: a,
            span: Span::DUMMY,
        };
        let sr = lower_to_signal_ref(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(sr, SignalRef::Signal(s) if s == SignalId::from_raw(5)));
    }

    #[test]
    fn signal_ref_width_known_targets() {
        let widths = |sid: SignalId| (sid.as_raw() < 2).then_some(8);
        let s0 = SignalId::from_raw(0);
        let s1 = SignalId::from_raw(1);
        assert_eq!(signal_ref_width(&SignalRef::Signal(s0), &widths), Some(8));
        let slice = SignalRef::Slice {
            signal: s1,
            high: 3,
            low: 0,
        };
        assert_eq!(signal_ref_width(&slice, &widths), Some(4));
        let cat = SignalRef::Concat(vec![SignalRef::Signal(s0), slice]);
        assert_eq!(signal_ref_width(&cat, &widths), Some(12));
    }

    #[test]
    fn signal_ref_width_unknown_targets() {
        // Signal 5 is not a plain packed vector (e.g. an unpacked array).
        let widths = |sid: SignalId| (sid.as_raw() < 2).then_some(8);
        let s5 = SignalId::from_raw(5);
        assert_eq!(signal_ref_width(&SignalRef::Signal(s5), &widths), None);
        let slice = SignalRef::Slice {
            signal: s5,
            high: 1,
            low: 1,
        };
        assert_eq!(signal_ref_width(&slice, &widths), None);
        let cat = SignalRef::Concat(vec![SignalRef::Signal(SignalId::from_raw(0)), slice]);
        assert_eq!(signal_ref_width(&cat, &widths), None);
        let poison = SignalRef::Const(LogicVec::all_zero(1));
        assert_eq!(signal_ref_width(&poison, &widths), None);
    }

    #[test]
    fn sv_assign_value_fills_known_width() {
        let (sdb, interner, sink, env) = setup();
        let fill = aion_sv_parser::ast::Expr::UnbasedUnsized {
            value: aion_sv_parser::ast::FillValue::One,
            span: Span::DUMMY,
        };
        let target = SignalRef::Signal(SignalId::from_raw(0));
        let ir = lower_sv_assign_value(&fill, &target, &|_| Some(5), &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Literal(ref lv) if lv.width() == 5
            && (0..5).all(|i| lv.get(i) == aion_common::Logic::One)));
        assert!(!sink.has_errors());
    }

    #[test]
    fn sv_assign_value_unknown_width_is_diagnosed() {
        let (sdb, interner, sink, env) = setup();
        let fill = aion_sv_parser::ast::Expr::UnbasedUnsized {
            value: aion_sv_parser::ast::FillValue::X,
            span: Span::DUMMY,
        };
        let target = SignalRef::Signal(SignalId::from_raw(0));
        lower_sv_assign_value(&fill, &target, &|_| None, &env, &sdb, &interner, &sink);
        assert!(sink.has_errors());
        assert_eq!(sink.diagnostics()[0].code, errors::E210);
    }

    #[test]
    fn sv_incr_decr_in_plain_expression_is_not_negation() {
        let (sdb, interner, sink, mut env) = setup();
        let b = interner.get_or_intern("b");
        env.insert(b, SignalId::from_raw(0));
        let expr = aion_sv_parser::ast::Expr::Unary {
            op: aion_sv_parser::ast::UnaryOp::PostIncr,
            operand: Box::new(aion_sv_parser::ast::Expr::Identifier {
                name: b,
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let ir = lower_sv_expr(&expr, &env, &sdb, &interner, &sink);
        assert!(!matches!(ir, IrExpr::Unary { .. }), "{ir:?}");
        assert_eq!(sink.diagnostics()[0].code, errors::E210);
    }

    #[test]
    fn signal_ref_unknown_emits_error() {
        let (sdb, interner, sink, env) = setup();
        let unknown = interner.get_or_intern("unknown");

        let ast_expr = aion_verilog_parser::ast::Expr::Identifier {
            name: unknown,
            span: Span::DUMMY,
        };
        let sr = lower_to_signal_ref(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(sr, SignalRef::Const(_)));
        assert!(sink.has_errors());
    }

    #[test]
    fn signal_ref_verilog_index() {
        let (mut sdb, interner, sink, mut env) = setup();
        let fid = sdb.add_source("test.v", "3".to_string());
        let a = interner.get_or_intern("a");
        env.insert(a, SignalId::from_raw(2));

        let ast_expr = aion_verilog_parser::ast::Expr::Index {
            base: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: a,
                span: Span::DUMMY,
            }),
            index: Box::new(aion_verilog_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 0, 1),
            }),
            span: Span::DUMMY,
        };
        let sr = lower_to_signal_ref(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(
            sr,
            SignalRef::Slice {
                signal,
                high: 3,
                low: 3,
            } if signal == SignalId::from_raw(2)
        ));
    }

    #[test]
    fn signal_ref_verilog_range_select() {
        let (mut sdb, interner, sink, mut env) = setup();
        let fid = sdb.add_source("test.v", "7 0".to_string());
        let a = interner.get_or_intern("a");
        env.insert(a, SignalId::from_raw(1));

        let ast_expr = aion_verilog_parser::ast::Expr::RangeSelect {
            base: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: a,
                span: Span::DUMMY,
            }),
            msb: Box::new(aion_verilog_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 0, 1),
            }),
            lsb: Box::new(aion_verilog_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 2, 3),
            }),
            span: Span::DUMMY,
        };
        let sr = lower_to_signal_ref(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(
            sr,
            SignalRef::Slice {
                signal,
                high: 7,
                low: 0,
            } if signal == SignalId::from_raw(1)
        ));
    }

    #[test]
    fn signal_ref_sv_index() {
        let (mut sdb, interner, sink, mut env) = setup();
        let fid = sdb.add_source("test.sv", "5".to_string());
        let b = interner.get_or_intern("b");
        env.insert(b, SignalId::from_raw(3));

        let ast_expr = aion_sv_parser::ast::Expr::Index {
            base: Box::new(aion_sv_parser::ast::Expr::Identifier {
                name: b,
                span: Span::DUMMY,
            }),
            index: Box::new(aion_sv_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 0, 1),
            }),
            span: Span::DUMMY,
        };
        let sr = lower_sv_to_signal_ref(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(
            sr,
            SignalRef::Slice {
                signal,
                high: 5,
                low: 5,
            } if signal == SignalId::from_raw(3)
        ));
    }

    #[test]
    fn signal_ref_sv_range_select() {
        let (mut sdb, interner, sink, mut env) = setup();
        let fid = sdb.add_source("test.sv", "15 8".to_string());
        let c = interner.get_or_intern("c");
        env.insert(c, SignalId::from_raw(0));

        let ast_expr = aion_sv_parser::ast::Expr::RangeSelect {
            base: Box::new(aion_sv_parser::ast::Expr::Identifier {
                name: c,
                span: Span::DUMMY,
            }),
            msb: Box::new(aion_sv_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 0, 2),
            }),
            lsb: Box::new(aion_sv_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 3, 4),
            }),
            span: Span::DUMMY,
        };
        let sr = lower_sv_to_signal_ref(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(
            sr,
            SignalRef::Slice {
                signal,
                high: 15,
                low: 8,
            } if signal == SignalId::from_raw(0)
        ));
    }

    #[test]
    fn signal_ref_index_unknown_base() {
        let (mut sdb, interner, sink, env) = setup();
        let fid = sdb.add_source("test.v", "0".to_string());
        let unknown = interner.get_or_intern("unknown");

        let ast_expr = aion_verilog_parser::ast::Expr::Index {
            base: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: unknown,
                span: Span::DUMMY,
            }),
            index: Box::new(aion_verilog_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 0, 1),
            }),
            span: Span::DUMMY,
        };
        let sr = lower_to_signal_ref(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(sr, SignalRef::Const(_)));
    }

    #[test]
    fn vhdl_bit_string_hex() {
        let lv = parse_vhdl_bit_string("X\"FF\"").unwrap();
        assert_eq!(lv.width(), 8);
        assert_eq!(lv.to_u64(), Some(0xFF));
    }

    #[test]
    fn vhdl_bit_string_bin() {
        let lv = parse_vhdl_bit_string("B\"1010\"").unwrap();
        assert_eq!(lv.width(), 4);
        assert_eq!(lv.to_u64(), Some(0b1010));
    }

    #[test]
    fn vhdl_bit_string_sized_unsigned_zero_extends() {
        let lv = parse_vhdl_bit_string("8UX\"F\"").unwrap();
        assert_eq!(lv.width(), 8);
        assert_eq!(lv.to_u64(), Some(0x0F));
        let lv = parse_vhdl_bit_string("8X\"F\"").unwrap();
        assert_eq!(lv.to_u64(), Some(0x0F));
        let lv = parse_vhdl_bit_string("3UX\"5\"").unwrap();
        assert_eq!((lv.width(), lv.to_u64()), (3, Some(0b101)));
    }

    #[test]
    fn vhdl_bit_string_sized_signed_sign_extends() {
        let lv = parse_vhdl_bit_string("8SX\"F\"").unwrap();
        assert_eq!((lv.width(), lv.to_u64()), (8, Some(0xFF)));
        let lv = parse_vhdl_bit_string("8SX\"7\"").unwrap();
        assert_eq!(lv.to_u64(), Some(0x07));
        let lv = parse_vhdl_bit_string("12SB\"101\"").unwrap();
        assert_eq!((lv.width(), lv.to_u64()), (12, Some(0xFFD)));
        let lv = parse_vhdl_bit_string("3SX\"F\"").unwrap();
        assert_eq!((lv.width(), lv.to_u64()), (3, Some(0b111)));
    }

    #[test]
    fn vhdl_bit_string_decimal_and_octal() {
        let lv = parse_vhdl_bit_string("16D\"255\"").unwrap();
        assert_eq!((lv.width(), lv.to_u64()), (16, Some(255)));
        let lv = parse_vhdl_bit_string("D\"5\"").unwrap();
        assert_eq!((lv.width(), lv.to_u64()), (3, Some(5)));
        let lv = parse_vhdl_bit_string("O\"17\"").unwrap();
        assert_eq!((lv.width(), lv.to_u64()), (6, Some(0o17)));
    }

    #[test]
    fn vhdl_bit_string_wider_than_64_bits() {
        let lv = parse_vhdl_bit_string("X\"1_0000_0000_0000_0001\"").unwrap();
        assert_eq!(lv.width(), 68);
        assert_eq!(lv.get(0), aion_common::Logic::One);
        assert_eq!(lv.get(64), aion_common::Logic::One);
        assert_eq!(lv.get(63), aion_common::Logic::Zero);
    }

    #[test]
    fn vhdl_bit_string_metavalues() {
        use aion_common::Logic;
        let lv = parse_vhdl_bit_string("X\"Z\"").unwrap();
        assert!((0..4).all(|i| lv.get(i) == Logic::Z));
        let lv = parse_vhdl_bit_string("B\"1-0U\"").unwrap();
        assert_eq!(lv.get(3), Logic::One);
        assert_eq!(lv.get(2), Logic::X);
        assert_eq!(lv.get(1), Logic::Zero);
        assert_eq!(lv.get(0), Logic::X);
    }

    #[test]
    fn vhdl_bit_string_errors() {
        // Truncation that drops a '1' is an error (LRM 15.8).
        assert!(parse_vhdl_bit_string("3UX\"F\"")
            .unwrap_err()
            .contains("cannot be truncated"));
        assert!(parse_vhdl_bit_string("2SX\"5\"").is_err());
        assert!(parse_vhdl_bit_string("8D\"256\"").is_err());
        // Null literal and characters with no 4-state equivalent.
        assert!(parse_vhdl_bit_string("X\"\"").is_err());
        assert!(parse_vhdl_bit_string("B\"Q\"")
            .unwrap_err()
            .contains("no 4-state"));
    }

    #[test]
    fn vhdl_bit_string_lowering_reports_invalid_literal() {
        let (mut sdb, interner, sink, env) = setup();
        let fid = sdb.add_source("test.vhd", "3UX\"F\" 8SX\"F\"".to_string());
        let bad = aion_vhdl_parser::ast::Expr::BitStringLiteral {
            span: aion_source::Span::new(fid, 0, 6),
        };
        let _ = lower_vhdl_expr(&bad, &env, &sdb, &interner, &sink);
        let diags = sink.diagnostics();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, errors::E213);

        let good = aion_vhdl_parser::ast::Expr::BitStringLiteral {
            span: aion_source::Span::new(fid, 7, 13),
        };
        let ir = lower_vhdl_expr(&good, &env, &sdb, &interner, &sink);
        match ir {
            IrExpr::Literal(lv) => assert_eq!((lv.width(), lv.to_u64()), (8, Some(0xFF))),
            other => panic!("expected literal, got {other:?}"),
        }
        assert_eq!(sink.diagnostics().len(), 1);
    }

    #[test]
    fn map_all_verilog_binary_ops() {
        use aion_verilog_parser::ast::BinaryOp as V;
        let ops = [
            V::Add,
            V::Sub,
            V::Mul,
            V::Div,
            V::Mod,
            V::Pow,
            V::Eq,
            V::Neq,
            V::CaseEq,
            V::CaseNeq,
            V::Lt,
            V::Le,
            V::Gt,
            V::Ge,
            V::LogAnd,
            V::LogOr,
            V::BitAnd,
            V::BitOr,
            V::BitXor,
            V::BitXnor,
            V::Shl,
            V::Shr,
            V::AShl,
            V::AShr,
        ];
        for op in ops {
            let _ = map_verilog_binary_op(op);
        }
    }

    #[test]
    fn map_all_verilog_unary_ops() {
        use aion_verilog_parser::ast::UnaryOp as V;
        let ops = [
            V::Minus,
            V::LogNot,
            V::BitNot,
            V::RedAnd,
            V::RedNand,
            V::RedOr,
            V::RedNor,
            V::RedXor,
            V::RedXnor,
        ];
        for op in ops {
            assert!(map_verilog_unary_op(op).is_some(), "{op:?}");
        }
        assert_eq!(map_verilog_unary_op(V::Minus), Some(UnaryOp::Neg));
    }

    #[test]
    fn unary_plus_maps_to_no_ir_op() {
        assert_eq!(
            map_verilog_unary_op(aion_verilog_parser::ast::UnaryOp::Plus),
            None
        );
        assert_eq!(map_sv_unary_op(aion_sv_parser::ast::UnaryOp::Plus), None);
        assert_eq!(map_vhdl_unary_op(aion_vhdl_parser::ast::UnaryOp::Pos), None);
        assert_eq!(
            map_sv_unary_op(aion_sv_parser::ast::UnaryOp::Minus),
            Some(UnaryOp::Neg)
        );
        assert_eq!(
            map_vhdl_unary_op(aion_vhdl_parser::ast::UnaryOp::Neg),
            Some((UnaryOp::Neg, false))
        );
    }

    #[test]
    fn sv_incr_decr_map_to_no_ir_op_and_are_rejected_before_mapping() {
        use aion_sv_parser::ast::UnaryOp as S;
        // Both unary plus (#66) and `++`/`--` (#30) map to `None`; the
        // lowering checks `is_incr_decr` first, so `None` there means plus.
        for op in [S::PreIncr, S::PreDecr, S::PostIncr, S::PostDecr] {
            assert!(op.is_incr_decr());
            assert_eq!(map_sv_unary_op(op), None, "{op:?}");
        }
        assert!(!S::Plus.is_incr_decr());

        let (sdb, interner, sink, mut env) = setup();
        let b = interner.get_or_intern("b");
        env.insert(b, SignalId::from_raw(0));
        let ident = || aion_sv_parser::ast::Expr::Identifier {
            name: b,
            span: Span::DUMMY,
        };
        let unary = |op| aion_sv_parser::ast::Expr::Unary {
            op,
            operand: Box::new(ident()),
            span: Span::DUMMY,
        };
        // `+b` lowers to `b` itself, with no diagnostic.
        let plus = lower_sv_expr(&unary(S::Plus), &env, &sdb, &interner, &sink);
        assert!(
            matches!(plus, IrExpr::Signal(SignalRef::Signal(_))),
            "{plus:?}"
        );
        assert!(!sink.has_errors());
        // `b++` in a pure expression is reported, not lowered to `b`.
        lower_sv_expr(&unary(S::PostIncr), &env, &sdb, &interner, &sink);
        assert!(sink.has_errors());
    }

    #[test]
    fn vhdl_builtin_rising_edge_no_error() {
        let (sdb, interner, sink, mut env) = setup();
        let clk = interner.get_or_intern("clk");
        env.insert(clk, SignalId::from_raw(0));
        let rising = interner.get_or_intern("rising_edge");

        let ast_expr = aion_vhdl_parser::ast::Expr::Name(aion_vhdl_parser::ast::Name {
            primary: rising,
            parts: vec![aion_vhdl_parser::ast::NameSuffix::Index(
                vec![aion_vhdl_parser::ast::Expr::Name(
                    aion_vhdl_parser::ast::Name {
                        primary: clk,
                        parts: vec![],
                        span: Span::DUMMY,
                    },
                )],
                Span::DUMMY,
            )],
            span: Span::DUMMY,
        });
        let ir = lower_vhdl_expr(&ast_expr, &env, &sdb, &interner, &sink);
        // Should produce a FuncCall, not an error
        assert!(matches!(ir, IrExpr::FuncCall { .. }));
        assert!(!sink.has_errors(), "rising_edge should not emit E204");
    }

    #[test]
    fn vhdl_builtin_std_logic_vector_no_error() {
        let (sdb, interner, sink, mut env) = setup();
        let sig = interner.get_or_intern("count_reg");
        env.insert(sig, SignalId::from_raw(0));
        let slv = interner.get_or_intern("std_logic_vector");

        let ast_expr = aion_vhdl_parser::ast::Expr::Name(aion_vhdl_parser::ast::Name {
            primary: slv,
            parts: vec![aion_vhdl_parser::ast::NameSuffix::Index(
                vec![aion_vhdl_parser::ast::Expr::Name(
                    aion_vhdl_parser::ast::Name {
                        primary: sig,
                        parts: vec![],
                        span: Span::DUMMY,
                    },
                )],
                Span::DUMMY,
            )],
            span: Span::DUMMY,
        });
        let ir = lower_vhdl_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::FuncCall { .. }));
        assert!(!sink.has_errors(), "std_logic_vector should not emit E204");
    }

    #[test]
    fn vhdl_non_builtin_emits_error() {
        let (sdb, interner, sink, env) = setup();
        let unknown = interner.get_or_intern("not_a_builtin");

        let ast_expr = aion_vhdl_parser::ast::Expr::Name(aion_vhdl_parser::ast::Name {
            primary: unknown,
            parts: vec![],
            span: Span::DUMMY,
        });
        let _ = lower_vhdl_expr(&ast_expr, &env, &sdb, &interner, &sink);
        assert!(sink.has_errors(), "unknown name should still emit E204");
    }

    /// Lowers `<op> v` for a VHDL unary operator applied to signal `v`.
    fn lower_vhdl_unary(op: aion_vhdl_parser::ast::UnaryOp) -> (IrExpr, SignalId, bool) {
        let (sdb, interner, sink, mut env) = setup();
        let v = interner.get_or_intern("v");
        let sid = SignalId::from_raw(3);
        env.insert(v, sid);
        let ast_expr = aion_vhdl_parser::ast::Expr::Unary {
            op,
            operand: Box::new(aion_vhdl_parser::ast::Expr::Name(
                aion_vhdl_parser::ast::Name {
                    primary: v,
                    parts: vec![],
                    span: Span::DUMMY,
                },
            )),
            span: Span::DUMMY,
        };
        let ir = lower_vhdl_expr(&ast_expr, &env, &sdb, &interner, &sink);
        (ir, sid, sink.has_errors())
    }

    /// Returns `true` if `e` is `Unary { op, Signal(sid) }`.
    fn is_unary_of_signal(e: &IrExpr, op: UnaryOp, sid: SignalId) -> bool {
        match e {
            IrExpr::Unary {
                op: got, operand, ..
            } => {
                *got == op && matches!(**operand, IrExpr::Signal(SignalRef::Signal(s)) if s == sid)
            }
            _ => false,
        }
    }

    #[test]
    fn vhdl_reductions_lower_to_ir_reductions() {
        use aion_vhdl_parser::ast::UnaryOp as V;
        for (op, ir_op) in [
            (V::RedAnd, UnaryOp::RedAnd),
            (V::RedOr, UnaryOp::RedOr),
            (V::RedXor, UnaryOp::RedXor),
        ] {
            let (ir, sid, errs) = lower_vhdl_unary(op);
            assert!(!errs, "{op:?} should lower without errors");
            assert!(
                is_unary_of_signal(&ir, ir_op, sid),
                "{op:?} lowered to {ir:?}"
            );
        }
    }

    #[test]
    fn vhdl_complemented_reductions_lower_to_not_of_reduction() {
        use aion_vhdl_parser::ast::UnaryOp as V;
        for (op, inner_op) in [
            (V::RedNand, UnaryOp::RedAnd),
            (V::RedNor, UnaryOp::RedOr),
            (V::RedXnor, UnaryOp::RedXor),
        ] {
            let (ir, sid, errs) = lower_vhdl_unary(op);
            assert!(!errs, "{op:?} should lower without errors");
            match &ir {
                IrExpr::Unary {
                    op: UnaryOp::Not,
                    operand,
                    ..
                } => assert!(
                    is_unary_of_signal(operand, inner_op, sid),
                    "{op:?} inner lowered to {operand:?}"
                ),
                other => panic!("{op:?} should lower to Not(..), got {other:?}"),
            }
        }
    }

    #[test]
    fn vhdl_non_reduction_unary_ops_not_complemented() {
        use aion_vhdl_parser::ast::UnaryOp as V;
        assert_eq!(map_vhdl_unary_op(V::Not), Some((UnaryOp::Not, false)));
        assert_eq!(map_vhdl_unary_op(V::Neg), Some((UnaryOp::Neg, false)));
        let (ir, sid, _) = lower_vhdl_unary(V::Not);
        assert!(is_unary_of_signal(&ir, UnaryOp::Not, sid));
    }

    #[test]
    fn is_vhdl_builtin_case_insensitive() {
        assert!(is_vhdl_builtin("RISING_EDGE"));
        assert!(is_vhdl_builtin("Rising_Edge"));
        assert!(is_vhdl_builtin("to_unsigned"));
        assert!(!is_vhdl_builtin("my_signal"));
    }

    // ---- SignalEnv constants ----

    /// Returns `(width, value)` of an IR literal or named constant, panicking
    /// on anything else.
    fn lit(e: &IrExpr) -> (u32, u64) {
        match e {
            IrExpr::Literal(lv) | IrExpr::NamedConst { value: lv, .. } => {
                (lv.width(), lv.to_u64().expect("2-state literal"))
            }
            other => panic!("expected literal, got {other:?}"),
        }
    }

    #[test]
    fn signal_env_new_is_empty() {
        let (_, interner, _, env) = setup();
        let a = interner.get_or_intern("a");
        assert!(env.get(&a).is_none());
        assert!(env.const_literal(a).is_none());
        assert!(env.consts().is_empty());
    }

    #[test]
    fn signal_env_insert_returns_previous_binding() {
        let (_, interner, _, mut env) = setup();
        let a = interner.get_or_intern("a");
        assert_eq!(env.insert(a, SignalId::from_raw(1)), None);
        assert_eq!(
            env.insert(a, SignalId::from_raw(2)),
            Some(SignalId::from_raw(1))
        );
        assert_eq!(env.get(&a), Some(&SignalId::from_raw(2)));
    }

    #[test]
    fn signal_env_const_is_not_a_signal_but_is_in_consts() {
        let (_, interner, _, mut env) = setup();
        let w = interner.get_or_intern("W");
        env.insert_const(w, ConstValue::Int(8), 32);
        assert!(env.get(&w).is_none(), "a constant is not a signal");
        assert_eq!(env.consts().get(&w), Some(&ConstValue::Int(8)));
        // A clone (used for VHDL process scopes) keeps the constants.
        assert_eq!(env.clone().const_literal(w), env.const_literal(w));
    }

    #[test]
    fn signal_env_const_literal_encodes_each_value_kind() {
        let (_, interner, _, mut env) = setup();
        let names: Vec<_> = ["i", "n", "b", "r", "l", "s", "z", "wide"]
            .iter()
            .map(|n| interner.get_or_intern(n))
            .collect();
        env.insert_const(names[0], ConstValue::Int(5), 3);
        env.insert_const(names[1], ConstValue::Int(-2), 4);
        env.insert_const(names[2], ConstValue::Bool(true), 1);
        env.insert_const(names[3], ConstValue::Real(2.9), 8);
        env.insert_const(
            names[4],
            ConstValue::Logic(LogicVec::from_u64(0b1101, 4)),
            2,
        );
        env.insert_const(names[5], ConstValue::String("hi".into()), 16);
        env.insert_const(names[6], ConstValue::Int(1), 0);
        env.insert_const(names[7], ConstValue::Int(-1), 70);
        let get = |i: usize| env.const_literal(names[i]).unwrap();
        assert_eq!(get(0).to_u64(), Some(5));
        assert_eq!((get(1).width(), get(1).to_u64()), (4, Some(0b1110)));
        assert_eq!(get(2).to_u64(), Some(1));
        assert_eq!(get(3).to_u64(), Some(2), "real truncates toward zero");
        assert_eq!((get(4).width(), get(4).to_u64()), (2, Some(0b01)));
        assert_eq!(get(5), LogicVec::all_zero(1), "string placeholder");
        assert_eq!(get(6).width(), 1, "width 0 is clamped to 1");
        let wide = get(7);
        assert_eq!(wide.width(), 70);
        assert!(
            (0..70).all(|i| wide.get(i) == aion_common::Logic::One),
            "negative values sign-extend past 64 bits"
        );
    }

    #[test]
    fn identifier_naming_constant_lowers_to_literal() {
        let (sdb, interner, sink, mut env) = setup();
        let w = interner.get_or_intern("W");
        env.insert_const(w, ConstValue::Int(8), 32);
        let v = aion_verilog_parser::ast::Expr::Identifier {
            name: w,
            span: Span::DUMMY,
        };
        let sv = aion_sv_parser::ast::Expr::Identifier {
            name: w,
            span: Span::DUMMY,
        };
        let vhdl = aion_vhdl_parser::ast::Expr::Name(aion_vhdl_parser::ast::Name {
            primary: w,
            parts: vec![],
            span: Span::DUMMY,
        });
        let ir = lower_verilog_expr(&v, &env, &sdb, &interner, &sink);
        assert!(
            matches!(ir, IrExpr::NamedConst { name, .. } if name == w),
            "{ir:?}"
        );
        assert_eq!(lit(&ir), (32, 8));
        assert_eq!(
            lit(&lower_sv_expr(&sv, &env, &sdb, &interner, &sink)),
            (32, 8)
        );
        assert_eq!(
            lit(&lower_vhdl_expr(&vhdl, &env, &sdb, &interner, &sink)),
            (32, 8)
        );
        assert!(!sink.has_errors());
    }

    #[test]
    fn signal_takes_precedence_over_constant() {
        let (sdb, interner, sink, mut env) = setup();
        let a = interner.get_or_intern("a");
        env.insert_const(a, ConstValue::Int(3), 32);
        env.insert(a, SignalId::from_raw(4));
        let e = aion_verilog_parser::ast::Expr::Identifier {
            name: a,
            span: Span::DUMMY,
        };
        let ir = lower_verilog_expr(&e, &env, &sdb, &interner, &sink);
        assert!(matches!(ir, IrExpr::Signal(SignalRef::Signal(s)) if s == SignalId::from_raw(4)));
    }

    #[test]
    fn param_value_width_rules() {
        // A declared width always wins.
        assert_eq!(param_value_width(Some(4), &ConstValue::Int(1), Some(2)), 4);
        // Untyped: the value's width, else 32 bits for an integer.
        assert_eq!(param_value_width(None, &ConstValue::Int(1), Some(2)), 2);
        assert_eq!(param_value_width(None, &ConstValue::Int(1), None), 32);
        let lv = ConstValue::Logic(LogicVec::all_zero(5));
        assert_eq!(param_value_width(None, &lv, None), 5);
        assert_eq!(param_value_width(None, &ConstValue::Bool(true), None), 1);
        assert_eq!(param_value_width(None, &ConstValue::Real(1.0), None), 64);
        assert_eq!(param_value_width(Some(0), &ConstValue::Int(0), None), 1);
    }

    #[test]
    fn verilog_value_width_of_expressions() {
        use aion_verilog_parser::ast::{BinaryOp, Expr, UnaryOp};
        let (mut sdb, interner, _, mut env) = setup();
        let file = sdb.add_source("w.v", "2'b01 42 4'hF".to_string());
        let sized = Expr::Literal {
            span: Span::new(file, 0, 5),
        };
        let unsized_ = Expr::Literal {
            span: Span::new(file, 6, 8),
        };
        let four = Expr::Literal {
            span: Span::new(file, 9, 13),
        };
        assert_eq!(verilog_value_width(&sized, &env, &sdb), 2);
        assert_eq!(verilog_value_width(&unsized_, &env, &sdb), 32);
        let add = Expr::Binary {
            left: Box::new(sized.clone()),
            op: BinaryOp::Add,
            right: Box::new(four.clone()),
            span: Span::DUMMY,
        };
        assert_eq!(verilog_value_width(&add, &env, &sdb), 4);
        let neg = Expr::Unary {
            op: UnaryOp::Minus,
            operand: Box::new(sized.clone()),
            span: Span::DUMMY,
        };
        assert_eq!(verilog_value_width(&neg, &env, &sdb), 2);
        // A comparison is not a width-preserving operator: 32-bit fallback.
        let eq = Expr::Binary {
            left: Box::new(sized),
            op: BinaryOp::Eq,
            right: Box::new(four),
            span: Span::DUMMY,
        };
        assert_eq!(verilog_value_width(&eq, &env, &sdb), 32);
        // A name of an earlier constant has that constant's width.
        let p = interner.get_or_intern("P");
        env.insert_const(p, ConstValue::Int(1), 6);
        let id = Expr::Identifier {
            name: p,
            span: Span::DUMMY,
        };
        assert_eq!(verilog_value_width(&id, &env, &sdb), 6);
        let unknown = Expr::Identifier {
            name: interner.get_or_intern("Q"),
            span: Span::DUMMY,
        };
        assert_eq!(verilog_value_width(&unknown, &env, &sdb), 32);
    }

    #[test]
    fn sv_value_width_of_expressions() {
        use aion_sv_parser::ast::Expr;
        let (mut sdb, interner, _, mut env) = setup();
        let file = sdb.add_source("w.sv", "3'd5 7".to_string());
        let sized = Expr::Literal {
            span: Span::new(file, 0, 4),
        };
        let unsized_ = Expr::Literal {
            span: Span::new(file, 5, 6),
        };
        assert_eq!(sv_value_width(&sized, &env, &sdb), 3);
        assert_eq!(sv_value_width(&unsized_, &env, &sdb), 32);
        let paren = Expr::Paren {
            inner: Box::new(sized),
            span: Span::DUMMY,
        };
        assert_eq!(sv_value_width(&paren, &env, &sdb), 3);
        let p = interner.get_or_intern("P");
        env.insert_const(p, ConstValue::Int(1), 9);
        let id = Expr::Identifier {
            name: p,
            span: Span::DUMMY,
        };
        assert_eq!(sv_value_width(&id, &env, &sdb), 9);
    }

    #[test]
    fn target_index_uses_constants_but_not_shadowing_signals() {
        let (sdb, interner, sink, mut env) = setup();
        let q = interner.get_or_intern("q");
        let k = interner.get_or_intern("K");
        let i = interner.get_or_intern("i");
        env.insert(q, SignalId::from_raw(0));
        env.insert_const(k, ConstValue::Int(3), 32);
        // `i` is both a constant and a signal: the signal wins, so the index
        // is not constant and the whole signal is the target.
        env.insert_const(i, ConstValue::Int(1), 32);
        env.insert(i, SignalId::from_raw(1));
        let index = |name| aion_verilog_parser::ast::Expr::Index {
            base: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name: q,
                span: Span::DUMMY,
            }),
            index: Box::new(aion_verilog_parser::ast::Expr::Identifier {
                name,
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let by_const = lower_to_signal_ref(&index(k), &env, &sdb, &interner, &sink);
        assert!(matches!(
            by_const,
            SignalRef::Slice {
                high: 3,
                low: 3,
                ..
            }
        ));
        let by_signal = lower_to_signal_ref(&index(i), &env, &sdb, &interner, &sink);
        assert!(matches!(by_signal, SignalRef::Signal(_)));
        assert!(!sink.has_errors(), "a non-constant index is not an error");
    }

    /// Parses `src` (language chosen by the extension of `file_name`),
    /// elaborates it with top `top`, and returns the value expression of the
    /// continuous assignment to `y` together with the [`SignalId`] of `a`.
    fn elaborate_assign_to_y(file_name: &str, src: &str) -> (IrExpr, SignalId) {
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
        let config: aion_config::ProjectConfig =
            toml::from_str("[project]\nname = \"uplus\"\nversion = \"0.1.0\"\ntop = \"top\"\n")
                .unwrap();
        let design = crate::elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        assert!(!sink.has_errors(), "errors: {:?}", sink.diagnostics());
        let top = &design.modules[design.top];
        let sig = |name: &str| {
            top.signals
                .iter()
                .find(|(_, s)| interner.resolve(s.name).eq_ignore_ascii_case(name))
                .map(|(id, _)| id)
                .unwrap_or_else(|| panic!("signal {name} not found"))
        };
        let (a, y) = (sig("a"), sig("y"));
        let assign = top
            .assignments
            .iter()
            .find(|asg| matches!(asg.target, SignalRef::Signal(t) if t == y))
            .expect("assignment to y");
        (assign.value.clone(), a)
    }

    /// Asserts that `expr` is exactly the signal `a`.
    fn assert_is_signal(expr: &IrExpr, a: SignalId) {
        assert!(
            matches!(expr, IrExpr::Signal(SignalRef::Signal(s)) if *s == a),
            "expected bare signal a, got {expr:?}"
        );
    }

    /// Asserts that `expr` is a single `Neg` applied directly to signal `a`.
    fn assert_single_neg(expr: &IrExpr, a: SignalId) {
        match expr {
            IrExpr::Unary {
                op: UnaryOp::Neg,
                operand,
                ..
            } => assert_is_signal(operand, a),
            other => panic!("expected Neg(a), got {other:?}"),
        }
    }

    const VERILOG_UNARY_TMPL: &str =
        "module top(input wire [7:0] a, output wire [7:0] y); assign y = EXPR; endmodule";

    const VHDL_UNARY_TMPL: &str =
        "entity top is port (a : in integer; y : out integer); end entity;\n\
         architecture rtl of top is begin y <= EXPR; end architecture;";

    fn verilog_assign(expr: &str) -> (IrExpr, SignalId) {
        elaborate_assign_to_y("top.v", &VERILOG_UNARY_TMPL.replace("EXPR", expr))
    }

    fn sv_assign(expr: &str) -> (IrExpr, SignalId) {
        let src = VERILOG_UNARY_TMPL.replace("wire", "logic");
        elaborate_assign_to_y("top.sv", &src.replace("EXPR", expr))
    }

    fn vhdl_assign(expr: &str) -> (IrExpr, SignalId) {
        elaborate_assign_to_y("top.vhd", &VHDL_UNARY_TMPL.replace("EXPR", expr))
    }

    #[test]
    fn verilog_unary_plus_is_identity() {
        let (e, a) = verilog_assign("+a");
        assert_is_signal(&e, a);
    }

    #[test]
    fn verilog_unary_minus_is_neg() {
        let (e, a) = verilog_assign("-a");
        assert_single_neg(&e, a);
    }

    #[test]
    fn verilog_plus_of_minus_is_single_neg() {
        let (e, a) = verilog_assign("+(-a)");
        assert_single_neg(&e, a);
    }

    #[test]
    fn verilog_minus_of_plus_is_single_neg() {
        let (e, a) = verilog_assign("-(+a)");
        assert_single_neg(&e, a);
    }

    #[test]
    fn sv_unary_plus_is_identity() {
        let (e, a) = sv_assign("+a");
        assert_is_signal(&e, a);
    }

    #[test]
    fn sv_unary_minus_is_neg() {
        let (e, a) = sv_assign("-a");
        assert_single_neg(&e, a);
    }

    #[test]
    fn sv_plus_of_minus_is_single_neg() {
        let (e, a) = sv_assign("+(-a)");
        assert_single_neg(&e, a);
    }

    #[test]
    fn sv_minus_of_plus_is_single_neg() {
        let (e, a) = sv_assign("-(+a)");
        assert_single_neg(&e, a);
    }

    #[test]
    fn vhdl_unary_plus_is_identity() {
        let (e, a) = vhdl_assign("+a");
        assert_is_signal(&e, a);
    }

    #[test]
    fn vhdl_unary_minus_is_neg() {
        let (e, a) = vhdl_assign("-a");
        assert_single_neg(&e, a);
    }

    #[test]
    fn vhdl_plus_of_minus_is_single_neg() {
        let (e, a) = vhdl_assign("+(-a)");
        assert_single_neg(&e, a);
    }

    #[test]
    fn vhdl_minus_of_plus_is_single_neg() {
        let (e, a) = vhdl_assign("-(+a)");
        assert_single_neg(&e, a);
    }
}
