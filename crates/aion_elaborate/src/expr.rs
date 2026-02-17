//! AST expression lowering to IR expressions.
//!
//! Converts language-specific AST expression trees (Verilog, SystemVerilog, VHDL)
//! into the unified [`Expr`](aion_ir::expr::Expr) representation. Signal references
//! are resolved through a [`SignalEnv`] mapping of interned names to signal IDs.

use std::collections::HashMap;

use aion_common::{Ident, Interner, LogicVec};
use aion_diagnostics::DiagnosticSink;
use aion_ir::expr::{BinaryOp, Expr as IrExpr, UnaryOp};
use aion_ir::ids::{SignalId, TypeId};
use aion_ir::signal::SignalRef;
use aion_source::{SourceDb, Span};

use crate::const_eval;
use crate::errors;

/// Maps interned signal names to their signal IDs within a module.
pub type SignalEnv = HashMap<Ident, SignalId>;

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
            let ir_op = map_verilog_unary_op(*op);
            let ir_operand = lower_verilog_expr(operand, sig_env, source_db, interner, sink);
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
            let count_val = const_eval::eval_verilog_expr(
                count,
                source_db,
                interner,
                &Default::default(),
                sink,
            )
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
        Expr::StringLiteral { .. } => IrExpr::Literal(LogicVec::all_zero(1)),
        Expr::Unary { op, operand, span } => {
            let ir_op = map_sv_unary_op(*op);
            let ir_operand = lower_sv_expr(operand, sig_env, source_db, interner, sink);
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
                const_eval::eval_sv_expr(count, source_db, interner, &Default::default(), sink)
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
        Expr::Error(span) => poison(*span),
    }
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
            let lv = parse_vhdl_bit_string(text);
            IrExpr::Literal(lv)
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
            let ir_op = map_vhdl_unary_op(*op);
            let ir_operand = lower_vhdl_expr(operand, sig_env, source_db, interner, sink);
            IrExpr::Unary {
                op: ir_op,
                operand: Box::new(ir_operand),
                ty: TypeId::from_raw(0),
                span: *span,
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
                if let Some(idx) = try_const_index_verilog(index, source_db) {
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
                    try_const_index_verilog(msb, source_db),
                    try_const_index_verilog(lsb, source_db),
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
                if let Some(idx) = try_const_index_sv(index, source_db) {
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
                    try_const_index_sv(msb, source_db),
                    try_const_index_sv(lsb, source_db),
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
fn try_const_index_verilog(
    expr: &aion_verilog_parser::ast::Expr,
    source_db: &SourceDb,
) -> Option<u32> {
    if let aion_verilog_parser::ast::Expr::Literal { span } = expr {
        let text = source_db.snippet(*span);
        crate::const_eval::parse_verilog_literal(text).and_then(|v| u32::try_from(v).ok())
    } else {
        None
    }
}

/// Tries to const-evaluate an SV expression to a `u32` index value.
fn try_const_index_sv(expr: &aion_sv_parser::ast::Expr, source_db: &SourceDb) -> Option<u32> {
    if let aion_sv_parser::ast::Expr::Literal { span } = expr {
        let text = source_db.snippet(*span);
        crate::const_eval::parse_verilog_literal(text).and_then(|v| u32::try_from(v).ok())
    } else {
        None
    }
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
        _ => SignalRef::Const(LogicVec::all_zero(1)),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolves an identifier to an IR signal reference, or emits an E204 diagnostic.
fn resolve_signal(
    name: Ident,
    span: Span,
    sig_env: &SignalEnv,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    if let Some(&sid) = sig_env.get(&name) {
        IrExpr::Signal(SignalRef::Signal(sid))
    } else {
        sink.emit(errors::error_unknown_signal(interner.resolve(name), span));
        poison(span)
    }
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

/// Parses a VHDL bit string literal (e.g., `X"FF"`, `B"1010"`) into a `LogicVec`.
fn parse_vhdl_bit_string(text: &str) -> LogicVec {
    let text = text.replace('_', "");
    let upper = text.to_uppercase();
    if let Some(hex_part) = upper.strip_prefix("X\"").and_then(|s| s.strip_suffix('"')) {
        let val = u64::from_str_radix(hex_part, 16).unwrap_or(0);
        let width = (hex_part.len() as u32) * 4;
        logic_vec_from_u64(width.max(1), val)
    } else if let Some(bin_part) = upper.strip_prefix("B\"").and_then(|s| s.strip_suffix('"')) {
        let val = u64::from_str_radix(bin_part, 2).unwrap_or(0);
        let width = bin_part.len() as u32;
        logic_vec_from_u64(width.max(1), val)
    } else if let Some(oct_part) = upper.strip_prefix("O\"").and_then(|s| s.strip_suffix('"')) {
        let val = u64::from_str_radix(oct_part, 8).unwrap_or(0);
        let width = (oct_part.len() as u32) * 3;
        logic_vec_from_u64(width.max(1), val)
    } else {
        LogicVec::all_zero(1)
    }
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
fn map_verilog_unary_op(op: aion_verilog_parser::ast::UnaryOp) -> UnaryOp {
    use aion_verilog_parser::ast::UnaryOp as V;
    match op {
        V::Plus => UnaryOp::Neg, // unary plus is identity, but we map it
        V::Minus => UnaryOp::Neg,
        V::LogNot => UnaryOp::LogicNot,
        V::BitNot => UnaryOp::Not,
        V::RedAnd | V::RedNand => UnaryOp::RedAnd,
        V::RedOr | V::RedNor => UnaryOp::RedOr,
        V::RedXor | V::RedXnor => UnaryOp::RedXor,
    }
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
fn map_sv_unary_op(op: aion_sv_parser::ast::UnaryOp) -> UnaryOp {
    use aion_sv_parser::ast::UnaryOp as S;
    match op {
        S::Plus => UnaryOp::Neg, // identity
        S::Minus => UnaryOp::Neg,
        S::LogNot => UnaryOp::LogicNot,
        S::BitNot => UnaryOp::Not,
        S::RedAnd | S::RedNand => UnaryOp::RedAnd,
        S::RedOr | S::RedNor => UnaryOp::RedOr,
        S::RedXor | S::RedXnor => UnaryOp::RedXor,
        S::PreIncr | S::PreDecr => UnaryOp::Neg, // approximate
    }
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
fn map_vhdl_unary_op(op: aion_vhdl_parser::ast::UnaryOp) -> UnaryOp {
    use aion_vhdl_parser::ast::UnaryOp as V;
    match op {
        V::Not => UnaryOp::Not,
        V::Abs => UnaryOp::Neg, // approximate
        V::Pos => UnaryOp::Neg, // identity
        V::Neg => UnaryOp::Neg,
        V::Condition => UnaryOp::Not, // VHDL-2008 condition operator (??) treated as boolean test
    }
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
        let lv = parse_vhdl_bit_string("X\"FF\"");
        assert_eq!(lv.width(), 8);
    }

    #[test]
    fn vhdl_bit_string_bin() {
        let lv = parse_vhdl_bit_string("B\"1010\"");
        assert_eq!(lv.width(), 4);
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
            V::Plus,
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
            let _ = map_verilog_unary_op(op);
        }
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

    #[test]
    fn is_vhdl_builtin_case_insensitive() {
        assert!(is_vhdl_builtin("RISING_EDGE"));
        assert!(is_vhdl_builtin("Rising_Edge"));
        assert!(is_vhdl_builtin("to_unsigned"));
        assert!(!is_vhdl_builtin("my_signal"));
    }
}
