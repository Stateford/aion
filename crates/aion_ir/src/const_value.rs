//! Constant values for parameters, initial values, and compile-time expressions.
//!
//! [`ConstValue`] represents a resolved compile-time value after elaboration.

use aion_common::LogicVec;
use serde::{Deserialize, Serialize};

/// A resolved compile-time constant value.
///
/// Used for parameter values, initial/reset values, and constant expressions
/// that have been fully evaluated during elaboration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConstValue {
    /// An integer constant.
    Int(i64),
    /// A floating-point constant.
    Real(f64),
    /// A logic vector constant (bit pattern).
    Logic(LogicVec),
    /// A string constant.
    String(String),
    /// A boolean constant.
    Bool(bool),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_value_variants() {
        let int = ConstValue::Int(42);
        let real = ConstValue::Real(1.5);
        let logic = ConstValue::Logic(LogicVec::all_zero(8));
        let string = ConstValue::String("hello".to_string());
        let boolean = ConstValue::Bool(true);

        assert_eq!(int, ConstValue::Int(42));
        assert_eq!(real, ConstValue::Real(1.5));
        assert_ne!(logic, ConstValue::Logic(LogicVec::all_one(8)));
        assert_eq!(string, ConstValue::String("hello".to_string()));
        assert_eq!(boolean, ConstValue::Bool(true));
    }

    #[test]
    fn const_value_serde_roundtrip() {
        let vals = vec![
            ConstValue::Int(-100),
            ConstValue::Real(9.81),
            ConstValue::String("test".to_string()),
            ConstValue::Bool(false),
        ];
        for val in vals {
            let json = serde_json::to_string(&val).unwrap();
            let restored: ConstValue = serde_json::from_str(&json).unwrap();
            assert_eq!(val, restored);
        }
    }
}
