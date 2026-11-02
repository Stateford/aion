//! Expression trees for behavioral IR.
//!
//! [`Expr`] represents language-independent expressions used inside
//! processes and assignments. All expressions are typed via [`TypeId`].

use crate::ids::TypeId;
use crate::signal::SignalRef;
use aion_common::{Ident, LogicVec};
use aion_source::Span;
use serde::{Deserialize, Serialize};

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnaryOp {
    /// Bitwise NOT (`~` / `not`).
    Not,
    /// Arithmetic negation (`-`).
    Neg,
    /// Reduction AND (`&`).
    RedAnd,
    /// Reduction OR (`|`).
    RedOr,
    /// Reduction XOR (`^`).
    RedXor,
    /// Logical NOT (`!`).
    LogicNot,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinaryOp {
    /// Addition (`+`).
    Add,
    /// Subtraction (`-`).
    Sub,
    /// Multiplication (`*`).
    Mul,
    /// Division (`/`).
    Div,
    /// Modulo (`%` / `mod`).
    Mod,
    /// Exponentiation (`**`).
    Pow,
    /// Bitwise AND (`&` / `and`).
    And,
    /// Bitwise OR (`|` / `or`).
    Or,
    /// Bitwise XOR (`^` / `xor`).
    Xor,
    /// Left shift (`<<` / `sll`).
    Shl,
    /// Right shift (`>>` / `srl`).
    Shr,
    /// Equality (`==` / `=`).
    Eq,
    /// Inequality (`!=` / `/=`).
    Ne,
    /// Less than (`<`).
    Lt,
    /// Less than or equal (`<=`).
    Le,
    /// Greater than (`>`).
    Gt,
    /// Greater than or equal (`>=`).
    Ge,
    /// Logical AND (`&&` / `and`).
    LogicAnd,
    /// Logical OR (`||` / `or`).
    LogicOr,
}

/// An expression in the behavioral IR.
///
/// Expressions are language-independent and fully typed after elaboration.
/// They appear inside process bodies, assignments, and case arms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expr {
    /// A reference to a signal (or part of a signal).
    Signal(SignalRef),
    /// A literal constant value.
    Literal(LogicVec),
    /// A reference to a named compile-time constant (a Verilog/SV
    /// `parameter` or `localparam`, or a VHDL generic), already resolved to
    /// its value during elaboration.
    ///
    /// It evaluates exactly like [`Expr::Literal`] with the same `value`. The
    /// name is kept so that consumers such as lint rules can tell a named
    /// constant from a magic number or a constant condition.
    NamedConst {
        /// The constant's name.
        name: Ident,
        /// The constant's value, at the parameter's declared width (32 bits
        /// for an untyped integer parameter).
        value: LogicVec,
    },
    /// A unary operation.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand expression.
        operand: Box<Expr>,
        /// The result type.
        ty: TypeId,
        /// Source location.
        span: Span,
    },
    /// A binary operation.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left-hand side.
        lhs: Box<Expr>,
        /// The right-hand side.
        rhs: Box<Expr>,
        /// The result type.
        ty: TypeId,
        /// Source location.
        span: Span,
    },
    /// A ternary/conditional expression (`cond ? a : b`).
    Ternary {
        /// The condition.
        condition: Box<Expr>,
        /// The value when true.
        true_val: Box<Expr>,
        /// The value when false.
        false_val: Box<Expr>,
        /// The result type.
        ty: TypeId,
        /// Source location.
        span: Span,
    },
    /// A function call expression.
    FuncCall {
        /// The function name.
        name: Ident,
        /// The argument expressions.
        args: Vec<Expr>,
        /// The return type.
        ty: TypeId,
        /// Source location.
        span: Span,
    },
    /// A concatenation of expressions.
    Concat(Vec<Expr>),
    /// A repeat expression (`{count{expr}}`).
    Repeat {
        /// The expression to repeat.
        expr: Box<Expr>,
        /// The number of repetitions.
        count: u32,
        /// Source location.
        span: Span,
    },
    /// A bit index expression (`expr[index]`).
    Index {
        /// The expression being indexed.
        expr: Box<Expr>,
        /// The index expression.
        index: Box<Expr>,
        /// Source location.
        span: Span,
    },
    /// A bit slice expression (`expr[high:low]`).
    Slice {
        /// The expression being sliced.
        expr: Box<Expr>,
        /// The high bit bound.
        high: Box<Expr>,
        /// The low bit bound.
        low: Box<Expr>,
        /// Source location.
        span: Span,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SignalId;

    #[test]
    fn literal_expr() {
        let e = Expr::Literal(LogicVec::all_zero(8));
        if let Expr::Literal(v) = &e {
            assert_eq!(v.width(), 8);
        } else {
            panic!("expected Literal");
        }
    }

    /// Asserts that `e` is a `NamedConst` named `name` with value `value`.
    fn assert_named(e: &Expr, name: Ident, value: &LogicVec) {
        match e {
            Expr::NamedConst { name: n, value: v } => {
                assert_eq!(*n, name);
                assert_eq!(v, value);
            }
            other => panic!("expected NamedConst, got {other:?}"),
        }
    }

    #[test]
    fn named_const_bincode_roundtrip() {
        let value = LogicVec::from_u64(0xA5, 32);
        let e = Expr::NamedConst {
            name: Ident::from_raw(3),
            value: value.clone(),
        };
        let config = bincode::config::standard();
        let bytes = bincode::serde::encode_to_vec(&e, config).unwrap();
        let (back, _): (Expr, usize) = bincode::serde::decode_from_slice(&bytes, config).unwrap();
        assert_named(&back, Ident::from_raw(3), &value);
    }

    #[test]
    fn named_const_json_roundtrip_is_distinct_from_literal() {
        let value = LogicVec::from_u64(1, 2);
        let e = Expr::NamedConst {
            name: Ident::from_raw(9),
            value: value.clone(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: Expr = serde_json::from_str(&json).unwrap();
        assert_named(&back, Ident::from_raw(9), &value);
        // A literal of the same value does not deserialize as a NamedConst.
        let lit_json = serde_json::to_string(&Expr::Literal(value)).unwrap();
        assert_ne!(json, lit_json);
    }

    #[test]
    fn signal_expr() {
        let e = Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)));
        assert!(matches!(e, Expr::Signal(SignalRef::Signal(_))));
    }

    #[test]
    fn unary_expr() {
        let operand = Box::new(Expr::Literal(LogicVec::all_one(4)));
        let e = Expr::Unary {
            op: UnaryOp::Not,
            operand,
            ty: TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        if let Expr::Unary { op, .. } = &e {
            assert_eq!(*op, UnaryOp::Not);
        } else {
            panic!("expected Unary");
        }
    }

    #[test]
    fn binary_expr() {
        let lhs = Box::new(Expr::Literal(LogicVec::all_zero(8)));
        let rhs = Box::new(Expr::Literal(LogicVec::all_one(8)));
        let e = Expr::Binary {
            op: BinaryOp::Add,
            lhs,
            rhs,
            ty: TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        if let Expr::Binary { op, .. } = &e {
            assert_eq!(*op, BinaryOp::Add);
        } else {
            panic!("expected Binary");
        }
    }

    #[test]
    fn ternary_expr() {
        let e = Expr::Ternary {
            condition: Box::new(Expr::Literal(LogicVec::all_one(1))),
            true_val: Box::new(Expr::Literal(LogicVec::all_zero(8))),
            false_val: Box::new(Expr::Literal(LogicVec::all_one(8))),
            ty: TypeId::from_raw(0),
            span: Span::DUMMY,
        };
        assert!(matches!(e, Expr::Ternary { .. }));
    }

    #[test]
    fn concat_expr() {
        let e = Expr::Concat(vec![
            Expr::Literal(LogicVec::all_zero(4)),
            Expr::Literal(LogicVec::all_one(4)),
        ]);
        if let Expr::Concat(parts) = &e {
            assert_eq!(parts.len(), 2);
        } else {
            panic!("expected Concat");
        }
    }

    #[test]
    fn all_unary_ops() {
        let ops = [
            UnaryOp::Not,
            UnaryOp::Neg,
            UnaryOp::RedAnd,
            UnaryOp::RedOr,
            UnaryOp::RedXor,
            UnaryOp::LogicNot,
        ];
        // All distinct
        for (i, a) in ops.iter().enumerate() {
            for (j, b) in ops.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }

    #[test]
    fn all_binary_ops() {
        let ops = [
            BinaryOp::Add,
            BinaryOp::Sub,
            BinaryOp::Mul,
            BinaryOp::Div,
            BinaryOp::Mod,
            BinaryOp::Pow,
            BinaryOp::And,
            BinaryOp::Or,
            BinaryOp::Xor,
            BinaryOp::Shl,
            BinaryOp::Shr,
            BinaryOp::Eq,
            BinaryOp::Ne,
            BinaryOp::Lt,
            BinaryOp::Le,
            BinaryOp::Gt,
            BinaryOp::Ge,
            BinaryOp::LogicAnd,
            BinaryOp::LogicOr,
        ];
        // Verify all distinct
        for (i, a) in ops.iter().enumerate() {
            for (j, b) in ops.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }
}
