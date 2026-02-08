//! Constant expression evaluation for elaboration.
//!
//! This module evaluates compile-time constant expressions from VHDL, Verilog,
//! and SystemVerilog ASTs into [`ConstValue`] results. It supports arithmetic
//! operations, literal parsing (including sized Verilog literals like `4'b1010`),
//! identifier lookup in a parameter environment, and built-in functions such as
//! `$clog2`.

use std::collections::HashMap;

use aion_common::{Ident, Interner};
use aion_diagnostics::DiagnosticSink;
use aion_ir::ConstValue;
use aion_source::SourceDb;
use aion_sv_parser::ast as sv_ast;
use aion_verilog_parser::ast as v_ast;
use aion_vhdl_parser::ast as vhdl_ast;

use crate::errors;

/// A mapping from interned identifiers to their constant values.
///
/// Used during elaboration to track parameter bindings and genvar values
/// so that constant expressions referencing parameters can be evaluated.
pub type ConstEnv = HashMap<Ident, ConstValue>;

/// Coerces a [`ConstValue`] to an `i64`, if the value can be represented as one.
///
/// - `Int(n)` returns `Some(n)` directly.
/// - `Real(f)` returns `Some(f as i64)` (truncation toward zero).
/// - `Bool(b)` returns `Some(1)` for `true`, `Some(0)` for `false`.
/// - `Logic` and `String` return `None` because they lack a natural integer mapping.
pub fn const_to_i64(val: &ConstValue) -> Option<i64> {
    match val {
        ConstValue::Int(n) => Some(*n),
        ConstValue::Real(f) => Some(*f as i64),
        ConstValue::Bool(b) => {
            if *b {
                Some(1)
            } else {
                Some(0)
            }
        }
        ConstValue::Logic(_) | ConstValue::String(_) => None,
    }
}

/// Parses a Verilog/SystemVerilog numeric literal from its source text.
///
/// Handles plain decimal (`42`), sized binary (`4'b1010`), sized hex (`8'hFF`),
/// sized octal (`8'o17`), sized decimal (`32'd100`), unsized based literals
/// (`'b1`, `'hFF`), and underscore separators (`1_000`).
pub(crate) fn parse_verilog_literal(text: &str) -> Option<i64> {
    let text = text.replace('_', "");

    if let Some(tick_pos) = text.find('\'') {
        let after_tick = &text[tick_pos + 1..];
        if after_tick.is_empty() {
            return None;
        }

        // Skip optional 's'/'S' for signed base literals
        let after_sign = if after_tick.starts_with('s') || after_tick.starts_with('S') {
            &after_tick[1..]
        } else {
            after_tick
        };

        if after_sign.is_empty() {
            return None;
        }

        let base_char = after_sign.as_bytes()[0];
        let digits = &after_sign[1..];

        let radix = match base_char {
            b'b' | b'B' => 2,
            b'o' | b'O' => 8,
            b'd' | b'D' => 10,
            b'h' | b'H' => 16,
            _ => return None,
        };

        // Replace x/z/? with 0 for constant evaluation purposes
        let clean: String = digits
            .chars()
            .filter_map(|c| match c {
                'x' | 'X' | 'z' | 'Z' | '?' => Some('0'),
                '_' => None,
                other => Some(other),
            })
            .collect();

        return i64::from_str_radix(&clean, radix).ok();
    }

    text.parse::<i64>().ok()
}

/// Computes the ceiling of log-base-2 for a non-negative integer.
///
/// Follows the SystemVerilog `$clog2` semantics:
/// - `clog2(0) = 0`
/// - `clog2(1) = 0`
/// - `clog2(2) = 1`
/// - `clog2(3) = 2`
/// - `clog2(4) = 2`
fn clog2(n: i64) -> i64 {
    if n <= 1 {
        return 0;
    }
    let mut result = 0i64;
    let mut val = n - 1;
    while val > 0 {
        result += 1;
        val >>= 1;
    }
    result
}

/// Applies a binary arithmetic operation on two `i64` operands.
///
/// Returns `None` for division/modulo by zero, negative exponents, or
/// unsupported operator strings.
fn apply_binop_i64(op: &str, lhs: i64, rhs: i64) -> Option<i64> {
    match op {
        "+" => Some(lhs.wrapping_add(rhs)),
        "-" => Some(lhs.wrapping_sub(rhs)),
        "*" => Some(lhs.wrapping_mul(rhs)),
        "/" => {
            if rhs == 0 {
                None
            } else {
                Some(lhs / rhs)
            }
        }
        "%" => {
            if rhs == 0 {
                None
            } else {
                Some(lhs % rhs)
            }
        }
        "**" => {
            if rhs < 0 {
                Some(0)
            } else {
                Some(lhs.wrapping_pow(rhs as u32))
            }
        }
        _ => None,
    }
}

/// Maps a Verilog `BinaryOp` to the operator string used by [`apply_binop_i64`].
fn verilog_binop_str(op: &v_ast::BinaryOp) -> Option<&'static str> {
    match op {
        v_ast::BinaryOp::Add => Some("+"),
        v_ast::BinaryOp::Sub => Some("-"),
        v_ast::BinaryOp::Mul => Some("*"),
        v_ast::BinaryOp::Div => Some("/"),
        v_ast::BinaryOp::Mod => Some("%"),
        v_ast::BinaryOp::Pow => Some("**"),
        _ => None,
    }
}

/// Maps a SystemVerilog `BinaryOp` to the operator string used by [`apply_binop_i64`].
fn sv_binop_str(op: &sv_ast::BinaryOp) -> Option<&'static str> {
    match op {
        sv_ast::BinaryOp::Add => Some("+"),
        sv_ast::BinaryOp::Sub => Some("-"),
        sv_ast::BinaryOp::Mul => Some("*"),
        sv_ast::BinaryOp::Div => Some("/"),
        sv_ast::BinaryOp::Mod => Some("%"),
        sv_ast::BinaryOp::Pow => Some("**"),
        _ => None,
    }
}

/// Maps a VHDL `BinaryOp` to the operator string used by [`apply_binop_i64`].
fn vhdl_binop_str(op: &vhdl_ast::BinaryOp) -> Option<&'static str> {
    match op {
        vhdl_ast::BinaryOp::Add => Some("+"),
        vhdl_ast::BinaryOp::Sub => Some("-"),
        vhdl_ast::BinaryOp::Mul => Some("*"),
        vhdl_ast::BinaryOp::Div => Some("/"),
        vhdl_ast::BinaryOp::Mod => Some("%"),
        vhdl_ast::BinaryOp::Pow => Some("**"),
        _ => None,
    }
}

/// Evaluates a Verilog-2005 expression to a compile-time constant.
///
/// Handles numeric literals, identifier lookup in the parameter environment,
/// binary arithmetic (+, -, *, /, %, **), unary negation, `$clog2`, and
/// parenthesized expressions. Emits an E209 diagnostic and returns `None`
/// for expressions that cannot be evaluated at compile time.
pub fn eval_verilog_expr(
    expr: &v_ast::Expr,
    source_db: &SourceDb,
    interner: &Interner,
    env: &ConstEnv,
    sink: &DiagnosticSink,
) -> Option<ConstValue> {
    match expr {
        v_ast::Expr::Literal { span } => {
            let text = source_db.snippet(*span);
            parse_verilog_literal(text).map(ConstValue::Int)
        }
        v_ast::Expr::Identifier { name, span } => match env.get(name) {
            Some(val) => Some(val.clone()),
            None => {
                let name_str = interner.resolve(*name);
                sink.emit(errors::error_param_not_const(
                    &format!("unknown identifier `{name_str}`"),
                    *span,
                ));
                None
            }
        },
        v_ast::Expr::Binary {
            left,
            op,
            right,
            span,
        } => {
            let lhs = eval_verilog_expr(left, source_db, interner, env, sink)?;
            let rhs = eval_verilog_expr(right, source_db, interner, env, sink)?;
            let l = const_to_i64(&lhs)?;
            let r = const_to_i64(&rhs)?;
            let op_str = verilog_binop_str(op);
            match op_str.and_then(|s| apply_binop_i64(s, l, r)) {
                Some(result) => Some(ConstValue::Int(result)),
                None => {
                    sink.emit(errors::error_param_not_const(
                        "arithmetic overflow or unsupported operator",
                        *span,
                    ));
                    None
                }
            }
        }
        v_ast::Expr::Unary {
            op: v_ast::UnaryOp::Minus,
            operand,
            ..
        } => {
            let val = eval_verilog_expr(operand, source_db, interner, env, sink)?;
            let n = const_to_i64(&val)?;
            Some(ConstValue::Int(-n))
        }
        v_ast::Expr::SystemCall {
            name, args, span, ..
        } => {
            let func_name = interner.resolve(*name);
            if func_name == "$clog2" {
                if args.len() != 1 {
                    sink.emit(errors::error_param_not_const(
                        "$clog2 requires exactly one argument",
                        *span,
                    ));
                    return None;
                }
                let arg_val = eval_verilog_expr(&args[0], source_db, interner, env, sink)?;
                let n = const_to_i64(&arg_val)?;
                Some(ConstValue::Int(clog2(n)))
            } else {
                sink.emit(errors::error_param_not_const(
                    &format!("unsupported system function `{func_name}`"),
                    *span,
                ));
                None
            }
        }
        v_ast::Expr::Paren { inner, .. } => {
            eval_verilog_expr(inner, source_db, interner, env, sink)
        }
        other => {
            sink.emit(errors::error_param_not_const(
                "non-constant expression",
                other.span(),
            ));
            None
        }
    }
}

/// Evaluates a SystemVerilog expression to a compile-time constant.
///
/// Handles the same constructs as [`eval_verilog_expr`] plus `ScopedIdent`
/// (package-qualified names like `pkg::PARAM`) which are looked up in the
/// parameter environment by their unqualified name.
pub fn eval_sv_expr(
    expr: &sv_ast::Expr,
    source_db: &SourceDb,
    interner: &Interner,
    env: &ConstEnv,
    sink: &DiagnosticSink,
) -> Option<ConstValue> {
    match expr {
        sv_ast::Expr::Literal { span } => {
            let text = source_db.snippet(*span);
            parse_verilog_literal(text).map(ConstValue::Int)
        }
        sv_ast::Expr::Identifier { name, span } => match env.get(name) {
            Some(val) => Some(val.clone()),
            None => {
                let name_str = interner.resolve(*name);
                sink.emit(errors::error_param_not_const(
                    &format!("unknown identifier `{name_str}`"),
                    *span,
                ));
                None
            }
        },
        sv_ast::Expr::ScopedIdent { name, span, .. } => match env.get(name) {
            Some(val) => Some(val.clone()),
            None => {
                let name_str = interner.resolve(*name);
                sink.emit(errors::error_param_not_const(
                    &format!("unknown scoped identifier `{name_str}`"),
                    *span,
                ));
                None
            }
        },
        sv_ast::Expr::Binary {
            left,
            op,
            right,
            span,
        } => {
            let lhs = eval_sv_expr(left, source_db, interner, env, sink)?;
            let rhs = eval_sv_expr(right, source_db, interner, env, sink)?;
            let l = const_to_i64(&lhs)?;
            let r = const_to_i64(&rhs)?;
            let op_str = sv_binop_str(op);
            match op_str.and_then(|s| apply_binop_i64(s, l, r)) {
                Some(result) => Some(ConstValue::Int(result)),
                None => {
                    sink.emit(errors::error_param_not_const(
                        "arithmetic overflow or unsupported operator",
                        *span,
                    ));
                    None
                }
            }
        }
        sv_ast::Expr::Unary {
            op: sv_ast::UnaryOp::Minus,
            operand,
            ..
        } => {
            let val = eval_sv_expr(operand, source_db, interner, env, sink)?;
            let n = const_to_i64(&val)?;
            Some(ConstValue::Int(-n))
        }
        sv_ast::Expr::SystemCall {
            name, args, span, ..
        } => {
            let func_name = interner.resolve(*name);
            if func_name == "$clog2" {
                if args.len() != 1 {
                    sink.emit(errors::error_param_not_const(
                        "$clog2 requires exactly one argument",
                        *span,
                    ));
                    return None;
                }
                let arg_val = eval_sv_expr(&args[0], source_db, interner, env, sink)?;
                let n = const_to_i64(&arg_val)?;
                Some(ConstValue::Int(clog2(n)))
            } else {
                sink.emit(errors::error_param_not_const(
                    &format!("unsupported system function `{func_name}`"),
                    *span,
                ));
                None
            }
        }
        sv_ast::Expr::Paren { inner, .. } => eval_sv_expr(inner, source_db, interner, env, sink),
        other => {
            sink.emit(errors::error_param_not_const(
                "non-constant expression",
                other.span(),
            ));
            None
        }
    }
}

/// Evaluates a VHDL expression to a compile-time constant.
///
/// Handles integer literals, simple name lookups in the parameter environment,
/// binary arithmetic, unary negation, and parenthesized expressions. Emits an
/// E209 diagnostic and returns `None` for expressions that cannot be evaluated
/// at compile time.
pub fn eval_vhdl_expr(
    expr: &vhdl_ast::Expr,
    source_db: &SourceDb,
    interner: &Interner,
    env: &ConstEnv,
    sink: &DiagnosticSink,
) -> Option<ConstValue> {
    match expr {
        vhdl_ast::Expr::IntLiteral { span } => {
            let text = source_db.snippet(*span).replace('_', "");
            text.parse::<i64>().ok().map(ConstValue::Int)
        }
        vhdl_ast::Expr::Name(name) => {
            if !name.parts.is_empty() {
                sink.emit(errors::error_param_not_const(
                    "qualified names are not supported in constant expressions",
                    name.span,
                ));
                return None;
            }
            match env.get(&name.primary) {
                Some(val) => Some(val.clone()),
                None => {
                    let name_str = interner.resolve(name.primary);
                    sink.emit(errors::error_param_not_const(
                        &format!("unknown identifier `{name_str}`"),
                        name.span,
                    ));
                    None
                }
            }
        }
        vhdl_ast::Expr::Binary {
            left,
            op,
            right,
            span,
        } => {
            let lhs = eval_vhdl_expr(left, source_db, interner, env, sink)?;
            let rhs = eval_vhdl_expr(right, source_db, interner, env, sink)?;
            let l = const_to_i64(&lhs)?;
            let r = const_to_i64(&rhs)?;
            let op_str = vhdl_binop_str(op);
            match op_str.and_then(|s| apply_binop_i64(s, l, r)) {
                Some(result) => Some(ConstValue::Int(result)),
                None => {
                    sink.emit(errors::error_param_not_const(
                        "arithmetic overflow or unsupported operator",
                        *span,
                    ));
                    None
                }
            }
        }
        vhdl_ast::Expr::Unary {
            op: vhdl_ast::UnaryOp::Neg,
            operand,
            ..
        } => {
            let val = eval_vhdl_expr(operand, source_db, interner, env, sink)?;
            let n = const_to_i64(&val)?;
            Some(ConstValue::Int(-n))
        }
        vhdl_ast::Expr::Paren { inner, .. } => {
            eval_vhdl_expr(inner, source_db, interner, env, sink)
        }
        other => {
            sink.emit(errors::error_param_not_const(
                "non-constant expression",
                other.span(),
            ));
            None
        }
    }
}

/// Evaluates a Verilog range to an `(msb, lsb)` pair of integer values.
///
/// Both the MSB and LSB expressions are evaluated as constants using the
/// given parameter environment.
pub fn eval_verilog_range(
    range: &v_ast::Range,
    source_db: &SourceDb,
    interner: &Interner,
    env: &ConstEnv,
    sink: &DiagnosticSink,
) -> Option<(i64, i64)> {
    let msb_val = eval_verilog_expr(&range.msb, source_db, interner, env, sink)?;
    let lsb_val = eval_verilog_expr(&range.lsb, source_db, interner, env, sink)?;
    let msb = const_to_i64(&msb_val)?;
    let lsb = const_to_i64(&lsb_val)?;
    Some((msb, lsb))
}

/// Evaluates a SystemVerilog range to an `(msb, lsb)` pair of integer values.
///
/// Both the MSB and LSB expressions are evaluated as constants using the
/// given parameter environment.
pub fn eval_sv_range(
    range: &sv_ast::Range,
    source_db: &SourceDb,
    interner: &Interner,
    env: &ConstEnv,
    sink: &DiagnosticSink,
) -> Option<(i64, i64)> {
    let msb_val = eval_sv_expr(&range.msb, source_db, interner, env, sink)?;
    let lsb_val = eval_sv_expr(&range.lsb, source_db, interner, env, sink)?;
    let msb = const_to_i64(&msb_val)?;
    let lsb = const_to_i64(&lsb_val)?;
    Some((msb, lsb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::LogicVec;
    use aion_source::Span;

    /// Creates a `SourceDb` with a single file and returns it along with a
    /// `Span` covering the entire content.
    fn make_source(text: &str) -> (SourceDb, Span) {
        let mut db = SourceDb::new();
        let file_id = db.add_source("test.v", text.to_string());
        let span = Span::new(file_id, 0, text.len() as u32);
        (db, span)
    }

    /// Creates a `SourceDb` with multiple contiguous snippets, returning
    /// individual spans for each.
    fn make_multi_source(texts: &[&str]) -> (SourceDb, Vec<Span>) {
        let combined: String = texts.join("");
        let mut db = SourceDb::new();
        let file_id = db.add_source("test.v", combined);
        let mut spans = Vec::new();
        let mut offset = 0u32;
        for text in texts {
            let len = text.len() as u32;
            spans.push(Span::new(file_id, offset, offset + len));
            offset += len;
        }
        (db, spans)
    }

    // ---- const_to_i64 ----

    #[test]
    fn const_to_i64_int() {
        assert_eq!(const_to_i64(&ConstValue::Int(42)), Some(42));
        assert_eq!(const_to_i64(&ConstValue::Int(-7)), Some(-7));
    }

    #[test]
    fn const_to_i64_real_truncates() {
        assert_eq!(const_to_i64(&ConstValue::Real(3.9)), Some(3));
        assert_eq!(const_to_i64(&ConstValue::Real(-2.1)), Some(-2));
    }

    #[test]
    fn const_to_i64_logic_returns_none() {
        assert_eq!(
            const_to_i64(&ConstValue::Logic(LogicVec::all_zero(8))),
            None
        );
    }

    #[test]
    fn const_to_i64_bool() {
        assert_eq!(const_to_i64(&ConstValue::Bool(true)), Some(1));
        assert_eq!(const_to_i64(&ConstValue::Bool(false)), Some(0));
    }

    #[test]
    fn const_to_i64_string_returns_none() {
        assert_eq!(const_to_i64(&ConstValue::String("hello".to_string())), None);
    }

    // ---- parse_verilog_literal ----

    #[test]
    fn parse_literal_decimal() {
        assert_eq!(parse_verilog_literal("42"), Some(42));
        assert_eq!(parse_verilog_literal("0"), Some(0));
    }

    #[test]
    fn parse_literal_sized_binary() {
        assert_eq!(parse_verilog_literal("4'b1010"), Some(10));
    }

    #[test]
    fn parse_literal_sized_hex() {
        assert_eq!(parse_verilog_literal("8'hFF"), Some(255));
    }

    #[test]
    fn parse_literal_sized_octal() {
        assert_eq!(parse_verilog_literal("8'o17"), Some(15));
    }

    #[test]
    fn parse_literal_sized_decimal() {
        assert_eq!(parse_verilog_literal("32'd100"), Some(100));
    }

    #[test]
    fn parse_literal_unsized_based() {
        assert_eq!(parse_verilog_literal("'b1"), Some(1));
        assert_eq!(parse_verilog_literal("'hFF"), Some(255));
    }

    #[test]
    fn parse_literal_underscore_separator() {
        assert_eq!(parse_verilog_literal("1_000"), Some(1000));
        assert_eq!(parse_verilog_literal("8'hF_F"), Some(255));
    }

    // ---- clog2 ----

    #[test]
    fn clog2_values() {
        assert_eq!(clog2(0), 0);
        assert_eq!(clog2(1), 0);
        assert_eq!(clog2(2), 1);
        assert_eq!(clog2(3), 2);
        assert_eq!(clog2(4), 2);
        assert_eq!(clog2(5), 3);
        assert_eq!(clog2(8), 3);
        assert_eq!(clog2(256), 8);
    }

    // ---- eval_verilog_expr ----

    #[test]
    fn eval_verilog_decimal_literal() {
        let (db, span) = make_source("42");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = v_ast::Expr::Literal { span };
        assert_eq!(
            eval_verilog_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(42))
        );
    }

    #[test]
    fn eval_verilog_sized_binary_literal() {
        let (db, span) = make_source("4'b1010");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = v_ast::Expr::Literal { span };
        assert_eq!(
            eval_verilog_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(10))
        );
    }

    #[test]
    fn eval_verilog_sized_hex_literal() {
        let (db, span) = make_source("8'hFF");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = v_ast::Expr::Literal { span };
        assert_eq!(
            eval_verilog_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(255))
        );
    }

    #[test]
    fn eval_verilog_identifier_in_env() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let width = interner.get_or_intern("WIDTH");
        let mut env = ConstEnv::new();
        env.insert(width, ConstValue::Int(8));

        let expr = v_ast::Expr::Identifier {
            name: width,
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_verilog_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(8))
        );
    }

    #[test]
    fn eval_verilog_binary_add() {
        let texts = ["10", "20"];
        let (db, spans) = make_multi_source(&texts);
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = v_ast::Expr::Binary {
            left: Box::new(v_ast::Expr::Literal { span: spans[0] }),
            op: v_ast::BinaryOp::Add,
            right: Box::new(v_ast::Expr::Literal { span: spans[1] }),
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_verilog_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(30))
        );
    }

    #[test]
    fn eval_verilog_clog2() {
        let (db, span) = make_source("256");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let clog2_name = interner.get_or_intern("$clog2");

        let expr = v_ast::Expr::SystemCall {
            name: clog2_name,
            args: vec![v_ast::Expr::Literal { span }],
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_verilog_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(8))
        );
    }

    #[test]
    fn eval_verilog_unknown_identifier_emits_diagnostic() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let unknown = interner.get_or_intern("MISSING");

        let expr = v_ast::Expr::Identifier {
            name: unknown,
            span: Span::DUMMY,
        };
        let result = eval_verilog_expr(&expr, &db, &interner, &env, &sink);
        assert!(result.is_none());
        assert!(sink.has_errors());
        assert_eq!(sink.error_count(), 1);
    }

    // ---- eval_sv_expr ----

    #[test]
    fn eval_sv_scoped_ident_lookup() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let pkg = interner.get_or_intern("pkg");
        let width = interner.get_or_intern("WIDTH");
        let mut env = ConstEnv::new();
        env.insert(width, ConstValue::Int(16));

        let expr = sv_ast::Expr::ScopedIdent {
            scope: pkg,
            name: width,
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_sv_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(16))
        );
    }

    #[test]
    fn eval_sv_literal() {
        let (db, span) = make_source("8'hAB");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = sv_ast::Expr::Literal { span };
        assert_eq!(
            eval_sv_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(0xAB))
        );
    }

    #[test]
    fn eval_sv_binary_add() {
        let texts = ["5", "3"];
        let (db, spans) = make_multi_source(&texts);
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = sv_ast::Expr::Binary {
            left: Box::new(sv_ast::Expr::Literal { span: spans[0] }),
            op: sv_ast::BinaryOp::Add,
            right: Box::new(sv_ast::Expr::Literal { span: spans[1] }),
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_sv_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(8))
        );
    }

    // ---- eval_vhdl_expr ----

    #[test]
    fn eval_vhdl_integer_literal() {
        let (db, span) = make_source("42");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = vhdl_ast::Expr::IntLiteral { span };
        assert_eq!(
            eval_vhdl_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(42))
        );
    }

    #[test]
    fn eval_vhdl_name_lookup() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let width = interner.get_or_intern("WIDTH");
        let mut env = ConstEnv::new();
        env.insert(width, ConstValue::Int(32));

        let expr = vhdl_ast::Expr::Name(vhdl_ast::Name {
            primary: width,
            parts: Vec::new(),
            span: Span::DUMMY,
        });
        assert_eq!(
            eval_vhdl_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(32))
        );
    }

    #[test]
    fn eval_vhdl_binary_add() {
        let texts = ["10", "20"];
        let (db, spans) = make_multi_source(&texts);
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = vhdl_ast::Expr::Binary {
            left: Box::new(vhdl_ast::Expr::IntLiteral { span: spans[0] }),
            op: vhdl_ast::BinaryOp::Add,
            right: Box::new(vhdl_ast::Expr::IntLiteral { span: spans[1] }),
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_vhdl_expr(&expr, &db, &interner, &env, &sink),
            Some(ConstValue::Int(30))
        );
    }

    // ---- eval_verilog_range ----

    #[test]
    fn eval_verilog_range_evaluates() {
        let texts = ["7", "0"];
        let (db, spans) = make_multi_source(&texts);
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let range = v_ast::Range {
            msb: v_ast::Expr::Literal { span: spans[0] },
            lsb: v_ast::Expr::Literal { span: spans[1] },
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_verilog_range(&range, &db, &interner, &env, &sink),
            Some((7, 0))
        );
    }

    // ---- eval_sv_range ----

    #[test]
    fn eval_sv_range_evaluates() {
        let texts = ["15", "0"];
        let (db, spans) = make_multi_source(&texts);
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let range = sv_ast::Range {
            msb: sv_ast::Expr::Literal { span: spans[0] },
            lsb: sv_ast::Expr::Literal { span: spans[1] },
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_sv_range(&range, &db, &interner, &env, &sink),
            Some((15, 0))
        );
    }

    // ---- Error cases ----

    #[test]
    fn non_constant_verilog_expr_returns_none() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = v_ast::Expr::Concat {
            elements: Vec::new(),
            span: Span::DUMMY,
        };
        let result = eval_verilog_expr(&expr, &db, &interner, &env, &sink);
        assert!(result.is_none());
        assert!(sink.has_errors());
    }

    #[test]
    fn non_constant_sv_expr_returns_none() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = sv_ast::Expr::Concat {
            elements: Vec::new(),
            span: Span::DUMMY,
        };
        let result = eval_sv_expr(&expr, &db, &interner, &env, &sink);
        assert!(result.is_none());
        assert!(sink.has_errors());
    }

    #[test]
    fn non_constant_vhdl_expr_returns_none() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();

        let expr = vhdl_ast::Expr::Others { span: Span::DUMMY };
        let result = eval_vhdl_expr(&expr, &db, &interner, &env, &sink);
        assert!(result.is_none());
        assert!(sink.has_errors());
    }
}
