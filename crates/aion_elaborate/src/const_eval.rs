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

/// Applies unary plus (the identity) to an evaluated numeric constant.
///
/// A `Real` operand is returned unchanged so `+3.5` stays `3.5`; going through
/// [`const_to_i64`] would truncate it. Every other value is coerced to an
/// integer exactly as the unary-minus arms do, and `Logic`/`String` operands
/// yield `None` because they have no numeric meaning.
fn unary_plus_value(val: ConstValue) -> Option<ConstValue> {
    match val {
        ConstValue::Real(f) => Some(ConstValue::Real(f)),
        other => Some(ConstValue::Int(const_to_i64(&other)?)),
    }
}

/// Parses a Verilog/SystemVerilog numeric literal from its source text.
///
/// Handles plain decimal (`42`), sized binary (`4'b1010`), sized hex (`8'hFF`),
/// sized octal (`8'o17`), sized decimal (`32'd100`), unsized based literals
/// (`'b1`, `'hFF`), and underscore separators (`1_000`).
pub(crate) fn parse_verilog_literal(text: &str) -> Option<i64> {
    parse_verilog_literal_with_width(text).map(|(_, val)| val)
}

/// Parses a Verilog/SystemVerilog numeric literal, returning `(explicit_width, value)`.
///
/// For sized literals like `24'h000000`, returns `(Some(24), 0)`.
/// For unsized literals like `42` or `'hFF`, returns `(None, value)`.
/// Whitespace inside the token (`8 'h FF`, allowed by IEEE 1364-2005 §3.5.1)
/// is ignored like underscores.
pub(crate) fn parse_verilog_literal_with_width(text: &str) -> Option<(Option<u32>, i64)> {
    let text: String = text
        .chars()
        .filter(|c| *c != '_' && !c.is_whitespace())
        .collect();

    if let Some(tick_pos) = text.find('\'') {
        // Extract the explicit width prefix (e.g., "24" from "24'h000000")
        let width_str = &text[..tick_pos];
        let explicit_width = if width_str.is_empty() {
            None
        } else {
            width_str.parse::<u32>().ok()
        };

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

        return i64::from_str_radix(&clean, radix)
            .ok()
            .map(|val| (explicit_width, val));
    }

    text.parse::<i64>().ok().map(|val| (None, val))
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

/// Applies a binary arithmetic operation like [`apply_binop_i64`], but
/// returns `None` when the result overflows an `i64` instead of wrapping.
///
/// Used for VHDL, where an out-of-range result is an error (IEEE 1076-2008
/// §9.2) and where `time` values are large femtosecond counts, so `1 hr * 3`
/// overflowing must not silently become a negative time.
fn checked_binop_i64(op: &str, lhs: i64, rhs: i64) -> Option<i64> {
    match op {
        "+" => lhs.checked_add(rhs),
        "-" => lhs.checked_sub(rhs),
        "*" => lhs.checked_mul(rhs),
        "/" => lhs.checked_div(rhs),
        "%" => lhs.checked_rem(rhs),
        "**" => {
            if rhs < 0 {
                Some(0)
            } else {
                lhs.checked_pow(u32::try_from(rhs).ok()?)
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

/// Maps a VHDL `BinaryOp` to the operator string used by [`checked_binop_i64`].
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
        v_ast::Expr::Unary {
            op: v_ast::UnaryOp::Plus,
            operand,
            ..
        } => {
            // Unary plus is the identity.
            unary_plus_value(eval_verilog_expr(operand, source_db, interner, env, sink)?)
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
        sv_ast::Expr::Unary {
            op: sv_ast::UnaryOp::Plus,
            operand,
            ..
        } => {
            // Unary plus is the identity.
            unary_plus_value(eval_sv_expr(operand, source_db, interner, env, sink)?)
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
/// binary arithmetic, unary negation, and parenthesized expressions. Physical
/// literals of type `time` (`20 ns`, see [`eval_vhdl_time_literal`]) evaluate
/// to their value in femtoseconds, so time arithmetic such as `T / 2` works
/// on plain integers. Emits an E209 diagnostic and returns `None` for
/// expressions that cannot be evaluated at compile time.
pub fn eval_vhdl_expr(
    expr: &vhdl_ast::Expr,
    source_db: &SourceDb,
    interner: &Interner,
    env: &ConstEnv,
    sink: &DiagnosticSink,
) -> Option<ConstValue> {
    // Physical literals of type `time` (`20 ns`, `ns`) are femtosecond counts.
    match eval_vhdl_time_literal(expr, source_db, interner, env) {
        Some(VhdlTimeLiteral::Fs(fs)) => return Some(ConstValue::Int(fs)),
        Some(VhdlTimeLiteral::Unrepresentable) => {
            sink.emit(errors::error_time_literal_unrepresentable(
                source_db.snippet(expr.span()),
                expr.span(),
            ));
            return None;
        }
        None => {}
    }
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
            match op_str.and_then(|s| checked_binop_i64(s, l, r)) {
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
            match n.checked_neg() {
                Some(neg) => Some(ConstValue::Int(neg)),
                None => {
                    sink.emit(errors::error_param_not_const(
                        "arithmetic overflow",
                        expr.span(),
                    ));
                    None
                }
            }
        }
        vhdl_ast::Expr::Unary {
            op: vhdl_ast::UnaryOp::Pos,
            operand,
            ..
        } => {
            // Unary plus is the identity.
            unary_plus_value(eval_vhdl_expr(operand, source_db, interner, env, sink)?)
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

/// The value of a SystemVerilog time literal (`10ns`, `1.5ps`, `1step`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeLiteralValue {
    /// A number with a time unit, converted to femtoseconds.
    Fs {
        /// The duration in femtoseconds, rounded half-up to a whole number.
        fs: u64,
        /// `false` if rounding to a whole femtosecond changed the value.
        exact: bool,
    },
    /// `1step`: one unit of the time precision, which depends on the
    /// timescale in effect.
    Step,
}

/// Parses the text of a SystemVerilog time literal into femtoseconds.
///
/// Accepts an unsigned integer or fixed-point number (underscores allowed)
/// immediately followed by `s`, `ms`, `us`, `ns`, `ps`, or `fs`, and the
/// special literal `1step`. The conversion is exact integer arithmetic: a
/// time literal is an absolute duration, independent of the time unit.
/// Returns `None` for malformed text or a value that does not fit in a `u64`
/// number of femtoseconds.
pub fn parse_time_literal(text: &str) -> Option<TimeLiteralValue> {
    let clean: String = text.chars().filter(|&c| c != '_').collect();
    if clean == "1step" {
        return Some(TimeLiteralValue::Step);
    }
    let unit_at = clean.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = clean.split_at(unit_at);
    let fs_per_unit: u128 = match unit {
        "s" => 1_000_000_000_000_000,
        "ms" => 1_000_000_000_000,
        "us" => 1_000_000_000,
        "ns" => 1_000_000,
        "ps" => 1_000,
        "fs" => 1,
        _ => return None,
    };
    let (int_part, frac_part) = number.split_once('.').unwrap_or((number, ""));
    let (fs, exact) = scale_decimal(int_part, frac_part, fs_per_unit)?;
    Some(TimeLiteralValue::Fs {
        fs: u64::try_from(fs).ok()?,
        exact,
    })
}

/// Returns the number of femtoseconds in one unit of the predefined VHDL
/// physical type `time` (IEEE 1076-2008 §16.3): `fs`, `ps`, `ns`, `us`, `ms`,
/// `sec`, `min`, or `hr`, matched case-insensitively.
///
/// Returns `None` for any other name. `fs` is the primary unit and the
/// resolution limit Aion uses, so every unit is a whole number of
/// femtoseconds and the largest (`hr`, 3.6e18 fs) still fits in an `i64`.
pub fn vhdl_time_unit_fs(unit: &str) -> Option<i64> {
    const UNITS: [(&str, i64); 8] = [
        ("fs", 1),
        ("ps", 1_000),
        ("ns", 1_000_000),
        ("us", 1_000_000_000),
        ("ms", 1_000_000_000_000),
        ("sec", 1_000_000_000_000_000),
        ("min", 60_000_000_000_000_000),
        ("hr", 3_600_000_000_000_000_000),
    ];
    UNITS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(unit))
        .map(|&(_, fs)| fs)
}

/// A VHDL physical literal of type `time`, recognized in an expression tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VhdlTimeLiteral {
    /// A literal that evaluated to this many femtoseconds.
    Fs(i64),
    /// A literal of the physical form whose value cannot be represented:
    /// it overflows a signed 64-bit femtosecond count, or its abstract
    /// literal is in a form Aion does not evaluate (a based literal such as
    /// `16#A# ns`).
    Unrepresentable,
}

/// Recognizes a VHDL physical literal of type `time` and evaluates it to
/// femtoseconds.
///
/// The parser represents `20 ns` as `Binary(Mul, 20, Name(ns))`; a bare
/// unit name such as `ns` is itself a physical literal meaning one unit.
/// Both forms are recognized, with an integer or real abstract literal
/// (`1.5 us`, `1e3 ps`, `1_000 fs`). A unit name that is also a constant in
/// `env` is not treated as a time unit, so a generic shadowing it keeps its
/// meaning. A real value is rounded half-up to a whole femtosecond, the
/// resolution limit.
///
/// Returns `None` if `expr` is not a physical time literal, so the caller can
/// evaluate it some other way. No diagnostics are emitted.
pub fn eval_vhdl_time_literal(
    expr: &vhdl_ast::Expr,
    source_db: &SourceDb,
    interner: &Interner,
    env: &ConstEnv,
) -> Option<VhdlTimeLiteral> {
    let unit_fs = |name: &vhdl_ast::Name| -> Option<i64> {
        if !name.parts.is_empty() || env.contains_key(&name.primary) {
            return None;
        }
        vhdl_time_unit_fs(interner.resolve(name.primary))
    };
    match expr {
        vhdl_ast::Expr::Name(name) => unit_fs(name).map(VhdlTimeLiteral::Fs),
        vhdl_ast::Expr::Binary {
            left,
            op: vhdl_ast::BinaryOp::Mul,
            right,
            ..
        } => {
            let vhdl_ast::Expr::Name(unit) = right.as_ref() else {
                return None;
            };
            let (vhdl_ast::Expr::IntLiteral { span } | vhdl_ast::Expr::RealLiteral { span }) =
                left.as_ref()
            else {
                return None;
            };
            let per_unit = unit_fs(unit)?;
            Some(
                abstract_literal_times(source_db.snippet(*span), per_unit)
                    .map_or(VhdlTimeLiteral::Unrepresentable, VhdlTimeLiteral::Fs),
            )
        }
        _ => None,
    }
}

/// Multiplies the non-negative decimal number `int_part.frac_part` (digits
/// only, `frac_part` possibly empty) by `factor`, rounding half-up to an
/// integer. Returns the result and whether it is exact.
///
/// The arithmetic is exact integer arithmetic, so `0.1` times `10` is exactly
/// `1`. Fractional digits beyond what can be scaled within a `u128` are
/// dropped (each is worth less than `factor / 10^19`); dropping a non-zero
/// digit makes the result inexact. Returns `None` for malformed digits or a
/// result that does not fit in a `u128`. Shared by the SystemVerilog time
/// literal parser and VHDL physical literals.
fn scale_decimal(int_part: &str, frac_part: &str, factor: u128) -> Option<(u128, bool)> {
    if int_part.is_empty()
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // The most fractional digits whose scaled value cannot overflow.
    let max_kept = (0..=38u32)
        .rev()
        .find(|&k| 10u128.pow(k).checked_mul(factor).is_some())
        .unwrap_or(0) as usize;
    let kept = frac_part.len().min(max_kept);
    let cut_nonzero = frac_part[kept..].bytes().any(|b| b != b'0');
    let int_val: u128 = int_part.parse().ok()?;
    let frac_val: u128 = if kept == 0 {
        0
    } else {
        frac_part[..kept].parse().ok()?
    };
    let scale = 10u128.pow(kept as u32);
    let whole = int_val.checked_mul(factor)?;
    let frac_scaled = frac_val * factor;
    let rem = frac_scaled % scale;
    let round_up = u128::from(rem != 0 && rem >= scale - rem);
    let value = whole.checked_add(frac_scaled / scale + round_up)?;
    Some((value, rem == 0 && !cut_nonzero))
}

/// Multiplies a VHDL decimal abstract literal (`20`, `1.5`, `2.5e-3`,
/// `1_000`) by a positive integer `factor`, rounding the exact product
/// half-up to an integer.
///
/// The exponent is applied by moving the decimal point, then
/// [`scale_decimal`] does the exact scaling. Returns `None` for malformed
/// text, a based literal (`16#FF#`), or a result that does not fit in an
/// `i64`.
fn abstract_literal_times(text: &str, factor: i64) -> Option<i64> {
    let clean = text.replace('_', "").to_ascii_lowercase();
    let (mantissa, exponent) = match clean.split_once('e') {
        Some((m, e)) => (m, e.strip_prefix('+').unwrap_or(e).parse::<i64>().ok()?),
        None => (clean.as_str(), 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{int_part}{frac_part}");
    if int_part.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let factor = u128::try_from(factor).ok()?;
    if digits.bytes().all(|b| b == b'0') {
        return Some(0);
    }
    // Bound the shift: a non-zero value times 10^61 overflows, and one below
    // 10^-60 times any `i64` factor rounds to zero.
    let point = i64::try_from(int_part.len()).ok()? + exponent;
    if exponent > 60 {
        return None;
    }
    if point < -60 {
        return Some(0);
    }
    let (int_digits, frac_digits) = if point <= 0 {
        (
            "0".to_string(),
            format!("{}{digits}", "0".repeat(point.unsigned_abs() as usize)),
        )
    } else if point as usize >= digits.len() {
        (
            format!("{digits}{}", "0".repeat(point as usize - digits.len())),
            String::new(),
        )
    } else {
        let (i, f) = digits.split_at(point as usize);
        (i.to_string(), f.to_string())
    };
    let (value, _) = scale_decimal(&int_digits, &frac_digits, factor)?;
    i64::try_from(value).ok()
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

    // ---- parse_verilog_literal_with_width ----

    #[test]
    fn parse_literal_with_width_sized_hex() {
        assert_eq!(
            parse_verilog_literal_with_width("8'hFF"),
            Some((Some(8), 255))
        );
    }

    #[test]
    fn parse_literal_with_width_sized_zero() {
        // This is the key bug fix: 24'h000000 must preserve width=24
        assert_eq!(
            parse_verilog_literal_with_width("24'h000000"),
            Some((Some(24), 0))
        );
    }

    #[test]
    fn parse_literal_with_width_sized_one() {
        assert_eq!(
            parse_verilog_literal_with_width("24'h000001"),
            Some((Some(24), 1))
        );
    }

    #[test]
    fn parse_literal_with_width_sized_binary() {
        assert_eq!(
            parse_verilog_literal_with_width("4'b1010"),
            Some((Some(4), 10))
        );
    }

    #[test]
    fn parse_literal_with_width_unsized_based() {
        assert_eq!(parse_verilog_literal_with_width("'hFF"), Some((None, 255)));
    }

    #[test]
    fn parse_literal_with_width_plain_decimal() {
        assert_eq!(parse_verilog_literal_with_width("42"), Some((None, 42)));
    }

    #[test]
    fn parse_literal_with_width_one_bit() {
        assert_eq!(parse_verilog_literal_with_width("1'b0"), Some((Some(1), 0)));
        assert_eq!(parse_verilog_literal_with_width("1'b1"), Some((Some(1), 1)));
    }

    #[test]
    fn parse_literal_with_width_eight_bit_zero() {
        assert_eq!(
            parse_verilog_literal_with_width("8'h00"),
            Some((Some(8), 0))
        );
    }

    #[test]
    fn parse_literal_with_width_ignores_lrm_whitespace() {
        assert_eq!(
            parse_verilog_literal_with_width("8 'hFF"),
            Some((Some(8), 255))
        );
        assert_eq!(
            parse_verilog_literal_with_width("8'h FF"),
            Some((Some(8), 255))
        );
        assert_eq!(
            parse_verilog_literal_with_width("4 'sb\n1010"),
            Some((Some(4), 10))
        );
    }

    #[test]
    fn parse_literal_with_width_rejects_invalid_digits() {
        assert_eq!(parse_verilog_literal_with_width("4'b102"), None);
        assert_eq!(parse_verilog_literal_with_width("4'b"), None);
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

    // ---- parse_time_literal ----

    #[test]
    fn time_literal_units() {
        let fs = |t: &str| match parse_time_literal(t) {
            Some(TimeLiteralValue::Fs { fs, exact: true }) => fs,
            other => panic!("{t}: {other:?}"),
        };
        assert_eq!(fs("10ns"), 10_000_000);
        assert_eq!(fs("1.5ps"), 1_500);
        assert_eq!(fs("100fs"), 100);
        assert_eq!(fs("1s"), 1_000_000_000_000_000);
        assert_eq!(fs("2ms"), 2_000_000_000_000);
        assert_eq!(fs("3us"), 3_000_000_000);
        assert_eq!(fs("1_000ps"), 1_000_000);
        assert_eq!(fs("0.25ns"), 250_000);
        assert_eq!(parse_time_literal("1step"), Some(TimeLiteralValue::Step));
    }

    #[test]
    fn time_literal_rounding_and_errors() {
        assert_eq!(
            parse_time_literal("1.5fs"),
            Some(TimeLiteralValue::Fs {
                fs: 2,
                exact: false
            })
        );
        assert_eq!(
            parse_time_literal("1.2fs"),
            Some(TimeLiteralValue::Fs {
                fs: 1,
                exact: false
            })
        );
        // Far too many fractional digits still parse and round.
        assert_eq!(
            parse_time_literal("1.0000000000000000000001ns"),
            Some(TimeLiteralValue::Fs {
                fs: 1_000_000,
                exact: false
            })
        );
        // Out of range for u64 femtoseconds (~18,446 s).
        assert_eq!(parse_time_literal("20000s"), None);
        // Malformed.
        assert_eq!(parse_time_literal("10"), None);
        assert_eq!(parse_time_literal("10xs"), None);
        assert_eq!(parse_time_literal("2step"), None);
        assert_eq!(parse_time_literal(".5ns"), None);
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

    // ---- operator precedence through the real parsers (issue #31) ----

    /// Parses `text` with the Verilog parser and evaluates it as a constant.
    fn eval_verilog_text(text: &str) -> Option<i64> {
        let mut db = SourceDb::new();
        let file = db.add_source("t.v", text.to_string());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let tokens = aion_verilog_parser::lexer::lex(text, file, &sink);
        let mut parser =
            aion_verilog_parser::parser::VerilogParser::new(tokens, text, file, &interner, &sink);
        let expr = parser.parse_expr();
        assert!(!sink.has_errors(), "parse errors: {:?}", sink.diagnostics());
        let val = eval_verilog_expr(&expr, &db, &interner, &ConstEnv::new(), &sink)?;
        const_to_i64(&val)
    }

    /// Parses `text` with the SystemVerilog parser and evaluates it as a constant.
    fn eval_sv_text(text: &str) -> Option<i64> {
        let mut db = SourceDb::new();
        let file = db.add_source("t.sv", text.to_string());
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let tokens = aion_sv_parser::lexer::lex(text, file, &sink);
        let mut parser =
            aion_sv_parser::parser::SvParser::new(tokens, text, file, &interner, &sink);
        let expr = parser.parse_expr();
        assert!(!sink.has_errors(), "parse errors: {:?}", sink.diagnostics());
        let val = eval_sv_expr(&expr, &db, &interner, &ConstEnv::new(), &sink)?;
        const_to_i64(&val)
    }

    #[test]
    fn power_is_left_associative_in_parameters() {
        // IEEE 1364-2005 §5.1.2 / IEEE 1800-2017 Table 11-2: `**` is
        // left-associative, so 2 ** 3 ** 2 == (2 ** 3) ** 2 == 64 (not 512).
        assert_eq!(eval_verilog_text("2 ** 3 ** 2"), Some(64));
        assert_eq!(eval_sv_text("2 ** 3 ** 2"), Some(64));
        // Parentheses still force right grouping.
        assert_eq!(eval_verilog_text("2 ** (3 ** 2)"), Some(512));
        assert_eq!(eval_sv_text("2 ** (3 ** 2)"), Some(512));
    }

    #[test]
    fn unary_minus_binds_tighter_than_power_in_parameters() {
        // Unary operators bind tighter than `**`: -2 ** 2 == (-2) ** 2 == 4.
        assert_eq!(eval_verilog_text("-2 ** 2"), Some(4));
        assert_eq!(eval_sv_text("-2 ** 2"), Some(4));
        assert_eq!(eval_verilog_text("-(2 ** 2)"), Some(-4));
        // `**` binds tighter than `*`: 3 * 2 ** 2 == 12.
        assert_eq!(eval_verilog_text("3 * 2 ** 2"), Some(12));
        assert_eq!(eval_sv_text("2 ** 2 * 3"), Some(12));
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

    // ---- Unary plus / minus ----

    /// Wraps `inner` in the Verilog unary operators `ops`, outermost first.
    fn v_unary(ops: &[v_ast::UnaryOp], inner: v_ast::Expr) -> v_ast::Expr {
        ops.iter().rev().fold(inner, |e, op| v_ast::Expr::Unary {
            op: *op,
            operand: Box::new(e),
            span: Span::DUMMY,
        })
    }

    /// Wraps `inner` in the SystemVerilog unary operators `ops`, outermost first.
    fn sv_unary(ops: &[sv_ast::UnaryOp], inner: sv_ast::Expr) -> sv_ast::Expr {
        ops.iter().rev().fold(inner, |e, op| sv_ast::Expr::Unary {
            op: *op,
            operand: Box::new(e),
            span: Span::DUMMY,
        })
    }

    /// Wraps `inner` in the VHDL unary operators `ops`, outermost first.
    fn vhdl_unary(ops: &[vhdl_ast::UnaryOp], inner: vhdl_ast::Expr) -> vhdl_ast::Expr {
        ops.iter().rev().fold(inner, |e, op| vhdl_ast::Expr::Unary {
            op: *op,
            operand: Box::new(e),
            span: Span::DUMMY,
        })
    }

    #[test]
    fn eval_verilog_unary_plus_minus() {
        use v_ast::UnaryOp::{Minus, Plus};
        let (db, span) = make_source("3");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let lit = || v_ast::Expr::Literal { span };
        for (ops, want) in [
            (&[Plus][..], 3),
            (&[Minus][..], -3),
            (&[Plus, Minus][..], -3),
            (&[Minus, Plus][..], -3),
        ] {
            let expr = v_unary(ops, lit());
            assert_eq!(
                eval_verilog_expr(&expr, &db, &interner, &env, &sink),
                Some(ConstValue::Int(want)),
                "{ops:?}"
            );
        }
        assert!(!sink.has_errors());
    }

    #[test]
    fn eval_verilog_unary_plus_of_non_constant_returns_none() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let expr = v_unary(
            &[v_ast::UnaryOp::Plus],
            v_ast::Expr::Concat {
                elements: Vec::new(),
                span: Span::DUMMY,
            },
        );
        assert!(eval_verilog_expr(&expr, &db, &interner, &env, &sink).is_none());
        assert!(sink.has_errors());
    }

    #[test]
    fn eval_sv_unary_plus_minus() {
        use sv_ast::UnaryOp::{Minus, Plus};
        let (db, span) = make_source("3");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let lit = || sv_ast::Expr::Literal { span };
        for (ops, want) in [
            (&[Plus][..], 3),
            (&[Minus][..], -3),
            (&[Plus, Minus][..], -3),
            (&[Minus, Plus][..], -3),
        ] {
            let expr = sv_unary(ops, lit());
            assert_eq!(
                eval_sv_expr(&expr, &db, &interner, &env, &sink),
                Some(ConstValue::Int(want)),
                "{ops:?}"
            );
        }
        assert!(!sink.has_errors());
    }

    #[test]
    fn eval_sv_unary_plus_of_non_constant_returns_none() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let expr = sv_unary(
            &[sv_ast::UnaryOp::Plus],
            sv_ast::Expr::Concat {
                elements: Vec::new(),
                span: Span::DUMMY,
            },
        );
        assert!(eval_sv_expr(&expr, &db, &interner, &env, &sink).is_none());
        assert!(sink.has_errors());
    }

    #[test]
    fn eval_vhdl_unary_plus_minus() {
        use vhdl_ast::UnaryOp::{Neg, Pos};
        let (db, span) = make_source("3");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let lit = || vhdl_ast::Expr::IntLiteral { span };
        for (ops, want) in [
            (&[Pos][..], 3),
            (&[Neg][..], -3),
            (&[Pos, Neg][..], -3),
            (&[Neg, Pos][..], -3),
        ] {
            let expr = vhdl_unary(ops, lit());
            assert_eq!(
                eval_vhdl_expr(&expr, &db, &interner, &env, &sink),
                Some(ConstValue::Int(want)),
                "{ops:?}"
            );
        }
        assert!(!sink.has_errors());
    }

    #[test]
    fn vhdl_time_units_are_femtosecond_multiples() {
        assert_eq!(vhdl_time_unit_fs("fs"), Some(1));
        assert_eq!(vhdl_time_unit_fs("NS"), Some(1_000_000));
        assert_eq!(vhdl_time_unit_fs("sec"), Some(1_000_000_000_000_000));
        assert_eq!(vhdl_time_unit_fs("hr"), Some(3_600_000_000_000_000_000));
        // `s` is the SystemVerilog spelling, not a VHDL unit.
        assert_eq!(vhdl_time_unit_fs("s"), None);
        assert_eq!(vhdl_time_unit_fs("clk"), None);
    }

    #[test]
    fn abstract_literal_scaling_is_exact_and_rounds_half_up() {
        assert_eq!(abstract_literal_times("20", 1_000_000), Some(20_000_000));
        assert_eq!(abstract_literal_times("1_000", 1), Some(1_000));
        assert_eq!(abstract_literal_times("1.5", 1_000), Some(1_500));
        assert_eq!(abstract_literal_times("0.1", 10), Some(1));
        assert_eq!(abstract_literal_times("2.5E-3", 1_000_000), Some(2_500));
        assert_eq!(abstract_literal_times("1e+3", 1_000), Some(1_000_000));
        assert_eq!(abstract_literal_times("0.0", 1_000), Some(0));
        // Sub-femtosecond values round to the nearest femtosecond.
        assert_eq!(abstract_literal_times("1.5", 1), Some(2));
        assert_eq!(abstract_literal_times("1.4", 1), Some(1));
        assert_eq!(abstract_literal_times("1e-60", 1), Some(0));
    }

    #[test]
    fn abstract_literal_scaling_rejects_bad_or_huge_values() {
        // 3 hours fit in i64 femtoseconds; 3000 hours do not.
        assert!(abstract_literal_times("2", 3_600_000_000_000_000_000).is_some());
        assert_eq!(
            abstract_literal_times("3000", 3_600_000_000_000_000_000),
            None
        );
        assert_eq!(abstract_literal_times("1e40", 1), None);
        assert_eq!(abstract_literal_times("16#FF#", 1), None);
        assert_eq!(abstract_literal_times(".5", 1), None);
        assert_eq!(abstract_literal_times("1e", 1), None);
    }

    /// Builds `lit unit` as the VHDL parser does: `Binary(Mul, lit, Name(unit))`.
    fn vhdl_physical(lit: vhdl_ast::Expr, unit: Ident) -> vhdl_ast::Expr {
        let span = lit.span();
        vhdl_ast::Expr::Binary {
            left: Box::new(lit),
            op: vhdl_ast::BinaryOp::Mul,
            right: Box::new(vhdl_name(unit)),
            span,
        }
    }

    fn vhdl_name(name: Ident) -> vhdl_ast::Expr {
        vhdl_ast::Expr::Name(vhdl_ast::Name {
            primary: name,
            parts: Vec::new(),
            span: Span::DUMMY,
        })
    }

    #[test]
    fn eval_vhdl_physical_time_literals() {
        let (db, spans) = make_multi_source(&["20", "1.5"]);
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let ns = interner.get_or_intern("ns");
        let us = interner.get_or_intern("us");
        let int_ns = vhdl_physical(vhdl_ast::Expr::IntLiteral { span: spans[0] }, ns);
        let real_us = vhdl_physical(vhdl_ast::Expr::RealLiteral { span: spans[1] }, us);
        assert_eq!(
            eval_vhdl_expr(&int_ns, &db, &interner, &env, &sink),
            Some(ConstValue::Int(20_000_000))
        );
        assert_eq!(
            eval_vhdl_expr(&real_us, &db, &interner, &env, &sink),
            Some(ConstValue::Int(1_500_000_000))
        );
        // A bare unit name is one unit.
        assert_eq!(
            eval_vhdl_expr(&vhdl_name(ns), &db, &interner, &env, &sink),
            Some(ConstValue::Int(1_000_000))
        );
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    }

    #[test]
    fn eval_vhdl_time_arithmetic_on_constants() {
        let (db, spans) = make_multi_source(&["20", "2"]);
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let ns = interner.get_or_intern("ns");
        let period = interner.get_or_intern("period");
        let mut env = ConstEnv::new();
        env.insert(period, ConstValue::Int(20_000_000));
        // `period / 2`
        let half = vhdl_ast::Expr::Binary {
            left: Box::new(vhdl_name(period)),
            op: vhdl_ast::BinaryOp::Div,
            right: Box::new(vhdl_ast::Expr::IntLiteral { span: spans[1] }),
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_vhdl_expr(&half, &db, &interner, &env, &sink),
            Some(ConstValue::Int(10_000_000))
        );
        // `20 ns * 2`
        let double = vhdl_ast::Expr::Binary {
            left: Box::new(vhdl_physical(
                vhdl_ast::Expr::IntLiteral { span: spans[0] },
                ns,
            )),
            op: vhdl_ast::BinaryOp::Mul,
            right: Box::new(vhdl_ast::Expr::IntLiteral { span: spans[1] }),
            span: Span::DUMMY,
        };
        assert_eq!(
            eval_vhdl_expr(&double, &db, &interner, &env, &sink),
            Some(ConstValue::Int(40_000_000))
        );
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
    }

    #[test]
    fn eval_vhdl_time_literal_edge_cases() {
        let (db, spans) = make_multi_source(&["3000", "16#A#"]);
        let interner = Interner::new();
        let ns = interner.get_or_intern("ns");
        let hr = interner.get_or_intern("hr");
        // A constant named like a unit keeps its meaning.
        let mut env = ConstEnv::new();
        env.insert(ns, ConstValue::Int(7));
        assert_eq!(
            eval_vhdl_time_literal(&vhdl_name(ns), &db, &interner, &env),
            None
        );
        let sink = DiagnosticSink::new();
        assert_eq!(
            eval_vhdl_expr(&vhdl_name(ns), &db, &interner, &env, &sink),
            Some(ConstValue::Int(7))
        );
        // Not a unit: no time literal.
        let clk = interner.get_or_intern("clk");
        let env = ConstEnv::new();
        let not_time = vhdl_physical(vhdl_ast::Expr::IntLiteral { span: spans[0] }, clk);
        assert_eq!(
            eval_vhdl_time_literal(&not_time, &db, &interner, &env),
            None
        );
        // Out of range (3000 hr) and based literals are errors, not wrong values.
        for lit in [
            vhdl_physical(vhdl_ast::Expr::IntLiteral { span: spans[0] }, hr),
            vhdl_physical(vhdl_ast::Expr::IntLiteral { span: spans[1] }, ns),
        ] {
            assert_eq!(
                eval_vhdl_time_literal(&lit, &db, &interner, &env),
                Some(VhdlTimeLiteral::Unrepresentable)
            );
            let sink = DiagnosticSink::new();
            assert_eq!(eval_vhdl_expr(&lit, &db, &interner, &env, &sink), None);
            assert!(sink.has_errors());
        }
    }

    #[test]
    fn eval_vhdl_unary_plus_of_non_constant_returns_none() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let env = ConstEnv::new();
        let sink = DiagnosticSink::new();
        let expr = vhdl_unary(
            &[vhdl_ast::UnaryOp::Pos],
            vhdl_ast::Expr::Others { span: Span::DUMMY },
        );
        assert!(eval_vhdl_expr(&expr, &db, &interner, &env, &sink).is_none());
        assert!(sink.has_errors());
    }

    #[test]
    fn unary_plus_value_preserves_real_and_coerces_others() {
        assert_eq!(
            unary_plus_value(ConstValue::Real(3.5)),
            Some(ConstValue::Real(3.5))
        );
        assert_eq!(
            unary_plus_value(ConstValue::Int(-7)),
            Some(ConstValue::Int(-7))
        );
        assert_eq!(
            unary_plus_value(ConstValue::Bool(true)),
            Some(ConstValue::Int(1))
        );
        assert_eq!(unary_plus_value(ConstValue::String("s".into())), None);
    }

    #[test]
    fn eval_unary_plus_preserves_real_binding_all_languages() {
        let (db, _) = make_source("");
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let r = interner.get_or_intern("R");
        let mut env = ConstEnv::new();
        env.insert(r, ConstValue::Real(3.5));
        let want = Some(ConstValue::Real(3.5));

        let v = v_unary(
            &[v_ast::UnaryOp::Plus],
            v_ast::Expr::Identifier {
                name: r,
                span: Span::DUMMY,
            },
        );
        assert_eq!(eval_verilog_expr(&v, &db, &interner, &env, &sink), want);

        let s = sv_unary(
            &[sv_ast::UnaryOp::Plus],
            sv_ast::Expr::Identifier {
                name: r,
                span: Span::DUMMY,
            },
        );
        assert_eq!(eval_sv_expr(&s, &db, &interner, &env, &sink), want);

        let h = vhdl_unary(
            &[vhdl_ast::UnaryOp::Pos],
            vhdl_ast::Expr::Name(vhdl_ast::Name {
                primary: r,
                parts: Vec::new(),
                span: Span::DUMMY,
            }),
        );
        assert_eq!(eval_vhdl_expr(&h, &db, &interner, &env, &sink), want);
        assert!(!sink.has_errors());
    }
}
