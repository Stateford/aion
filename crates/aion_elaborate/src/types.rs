//! AST type indications to IR [`TypeId`] resolution.
//!
//! Converts language-specific type representations (Verilog ranges, SystemVerilog
//! variable types, VHDL type indications) into the unified [`Type`] system via
//! [`TypeDb::intern`].

use aion_common::Interner;
use aion_diagnostics::DiagnosticSink;
use aion_ir::types::{Type, TypeDb};
use aion_ir::TypeId;
use aion_source::SourceDb;

use crate::const_eval::{self, ConstEnv};
use crate::errors;

/// Resolves a Verilog type from an optional range and signed flag to a [`TypeId`].
///
/// No range produces [`Type::Bit`]. A range `[N:M]` produces
/// [`Type::BitVec`] with `width = |N - M| + 1`.
pub fn resolve_verilog_type(
    range: Option<&aion_verilog_parser::ast::Range>,
    signed: bool,
    types: &mut TypeDb,
    env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> TypeId {
    match range {
        None => types.intern(Type::Bit),
        Some(r) => {
            if let Some((msb, lsb)) =
                const_eval::eval_verilog_range(r, source_db, interner, env, sink)
            {
                let width = (msb - lsb).unsigned_abs() as u32 + 1;
                types.intern(Type::BitVec { width, signed })
            } else {
                types.intern(Type::Error)
            }
        }
    }
}

/// Resolves a Verilog net type (`wire`, `reg`, `integer`, `real`) to a [`TypeId`].
#[allow(clippy::too_many_arguments)]
pub fn resolve_verilog_net_type(
    net_type: Option<&aion_verilog_parser::ast::NetType>,
    range: Option<&aion_verilog_parser::ast::Range>,
    signed: bool,
    types: &mut TypeDb,
    env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> TypeId {
    use aion_verilog_parser::ast::NetType;
    match net_type {
        Some(NetType::Integer) => types.intern(Type::Integer),
        Some(NetType::Real) => types.intern(Type::Real),
        _ => resolve_verilog_type(range, signed, types, env, source_db, interner, sink),
    }
}

/// Resolves a SystemVerilog port type to a [`TypeId`].
///
/// Handles `logic`/`bit` with optional ranges, and built-in integer types
/// like `int` (32-bit signed), `byte` (8-bit signed), etc.
#[allow(clippy::too_many_arguments)]
pub fn resolve_sv_type(
    port_type: &aion_sv_parser::ast::SvPortType,
    range: Option<&aion_sv_parser::ast::Range>,
    signed: bool,
    types: &mut TypeDb,
    env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> TypeId {
    use aion_sv_parser::ast::SvPortType;
    match port_type {
        SvPortType::Var(vt) => {
            resolve_sv_var_type(vt, range, signed, types, env, source_db, interner, sink)
        }
        SvPortType::Net(_) => {
            resolve_sv_range(range, signed, types, env, source_db, interner, sink)
        }
        SvPortType::Implicit => {
            resolve_sv_range(range, signed, types, env, source_db, interner, sink)
        }
        SvPortType::InterfacePort { .. } => types.intern(Type::Error),
    }
}

/// Resolves a SystemVerilog variable type to a [`TypeId`].
#[allow(clippy::too_many_arguments)]
pub fn resolve_sv_var_type(
    var_type: &aion_sv_parser::ast::VarType,
    range: Option<&aion_sv_parser::ast::Range>,
    signed: bool,
    types: &mut TypeDb,
    env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> TypeId {
    use aion_sv_parser::ast::VarType;
    match var_type {
        VarType::Logic | VarType::Bit | VarType::Reg => {
            resolve_sv_range(range, signed, types, env, source_db, interner, sink)
        }
        VarType::Byte => types.intern(Type::BitVec {
            width: 8,
            signed: true,
        }),
        VarType::Shortint => types.intern(Type::BitVec {
            width: 16,
            signed: true,
        }),
        VarType::Int => types.intern(Type::BitVec {
            width: 32,
            signed: true,
        }),
        VarType::Longint => types.intern(Type::BitVec {
            width: 64,
            signed: true,
        }),
        VarType::Integer => types.intern(Type::Integer),
        VarType::Real => types.intern(Type::Real),
    }
}

/// Resolves a SystemVerilog range to a [`TypeId`], defaulting to [`Type::Bit`].
pub(crate) fn resolve_sv_range(
    range: Option<&aion_sv_parser::ast::Range>,
    signed: bool,
    types: &mut TypeDb,
    env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> TypeId {
    match range {
        None => types.intern(Type::Bit),
        Some(r) => {
            if let Some((msb, lsb)) = const_eval::eval_sv_range(r, source_db, interner, env, sink) {
                let width = (msb - lsb).unsigned_abs() as u32 + 1;
                types.intern(Type::BitVec { width, signed })
            } else {
                types.intern(Type::Error)
            }
        }
    }
}

/// Resolves a VHDL type indication to a [`TypeId`].
///
/// Recognizes common IEEE types: `std_logic` maps to [`Type::Bit`],
/// `std_logic_vector(N downto M)` maps to [`Type::BitVec`],
/// `integer` maps to [`Type::Integer`], `boolean` maps to [`Type::Bool`].
pub fn resolve_vhdl_type(
    ty: &aion_vhdl_parser::ast::TypeIndication,
    types: &mut TypeDb,
    env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> TypeId {
    let type_name = resolve_type_mark_name(ty, interner);

    match type_name.as_str() {
        "std_logic" | "std_ulogic" => types.intern(Type::Bit),
        "bit" => types.intern(Type::Bit),
        "std_logic_vector" | "std_ulogic_vector" | "signed" | "unsigned" => {
            let is_signed = type_name == "signed";
            if let Some(constraint) = &ty.constraint {
                match constraint {
                    aion_vhdl_parser::ast::Constraint::Index(ranges, _) => {
                        if let Some(aion_vhdl_parser::ast::DiscreteRange::Range(rc)) =
                            ranges.first()
                        {
                            let left = const_eval::eval_vhdl_expr(
                                &rc.left, source_db, interner, env, sink,
                            );
                            let right = const_eval::eval_vhdl_expr(
                                &rc.right, source_db, interner, env, sink,
                            );
                            if let (Some(l), Some(r)) = (left, right) {
                                if let (Some(lv), Some(rv)) =
                                    (const_eval::const_to_i64(&l), const_eval::const_to_i64(&r))
                                {
                                    let width = (lv - rv).unsigned_abs() as u32 + 1;
                                    return types.intern(Type::BitVec {
                                        width,
                                        signed: is_signed,
                                    });
                                }
                            }
                        }
                        types.intern(Type::Error)
                    }
                    aion_vhdl_parser::ast::Constraint::Range(rc) => {
                        let left =
                            const_eval::eval_vhdl_expr(&rc.left, source_db, interner, env, sink);
                        let right =
                            const_eval::eval_vhdl_expr(&rc.right, source_db, interner, env, sink);
                        if let (Some(l), Some(r)) = (left, right) {
                            if let (Some(lv), Some(rv)) =
                                (const_eval::const_to_i64(&l), const_eval::const_to_i64(&r))
                            {
                                let width = (lv - rv).unsigned_abs() as u32 + 1;
                                return types.intern(Type::BitVec {
                                    width,
                                    signed: is_signed,
                                });
                            }
                        }
                        types.intern(Type::Error)
                    }
                }
            } else {
                // No constraint — unconstrained vector, use Error for now
                types.intern(Type::Error)
            }
        }
        "integer" | "natural" | "positive" => types.intern(Type::Integer),
        "real" => types.intern(Type::Real),
        "boolean" => types.intern(Type::Bool),
        "string" => types.intern(Type::Str),
        _ => {
            // Unknown type — emit diagnostic
            sink.emit(errors::error_unsupported(
                &format!("unknown VHDL type `{type_name}`"),
                ty.span,
            ));
            types.intern(Type::Error)
        }
    }
}

/// Extracts the final type name from a VHDL type mark's selected name.
fn resolve_type_mark_name(
    ty: &aion_vhdl_parser::ast::TypeIndication,
    interner: &Interner,
) -> String {
    let parts = &ty.type_mark.parts;
    if let Some(last) = parts.last() {
        interner.resolve(*last).to_lowercase()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_ir::types::TypeDb;
    use aion_source::SourceDb;

    fn setup() -> (SourceDb, Interner, DiagnosticSink, TypeDb, ConstEnv) {
        (
            SourceDb::new(),
            Interner::new(),
            DiagnosticSink::new(),
            TypeDb::new(),
            ConstEnv::new(),
        )
    }

    #[test]
    fn verilog_no_range_is_bit() {
        let (sdb, interner, sink, mut types, env) = setup();
        let tid = resolve_verilog_type(None, false, &mut types, &env, &sdb, &interner, &sink);
        assert_eq!(*types.get(tid), Type::Bit);
    }

    #[test]
    fn verilog_range_produces_bitvec() {
        let (mut sdb, interner, sink, mut types, env) = setup();
        let fid = sdb.add_source("test.v", "7 0".to_string());
        let range = aion_verilog_parser::ast::Range {
            msb: aion_verilog_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 0, 1),
            },
            lsb: aion_verilog_parser::ast::Expr::Literal {
                span: aion_source::Span::new(fid, 2, 3),
            },
            span: aion_source::Span::DUMMY,
        };
        let tid = resolve_verilog_type(
            Some(&range),
            false,
            &mut types,
            &env,
            &sdb,
            &interner,
            &sink,
        );
        assert_eq!(
            *types.get(tid),
            Type::BitVec {
                width: 8,
                signed: false
            }
        );
    }

    #[test]
    fn sv_logic_no_range_is_bit() {
        let (sdb, interner, sink, mut types, env) = setup();
        let pt = aion_sv_parser::ast::SvPortType::Var(aion_sv_parser::ast::VarType::Logic);
        let tid = resolve_sv_type(&pt, None, false, &mut types, &env, &sdb, &interner, &sink);
        assert_eq!(*types.get(tid), Type::Bit);
    }

    #[test]
    fn sv_int_type() {
        let (sdb, interner, sink, mut types, env) = setup();
        let pt = aion_sv_parser::ast::SvPortType::Var(aion_sv_parser::ast::VarType::Int);
        let tid = resolve_sv_type(&pt, None, false, &mut types, &env, &sdb, &interner, &sink);
        assert_eq!(
            *types.get(tid),
            Type::BitVec {
                width: 32,
                signed: true
            }
        );
    }

    #[test]
    fn sv_byte_type() {
        let (sdb, interner, sink, mut types, env) = setup();
        let pt = aion_sv_parser::ast::SvPortType::Var(aion_sv_parser::ast::VarType::Byte);
        let tid = resolve_sv_type(&pt, None, false, &mut types, &env, &sdb, &interner, &sink);
        assert_eq!(
            *types.get(tid),
            Type::BitVec {
                width: 8,
                signed: true
            }
        );
    }

    #[test]
    fn vhdl_std_logic_is_bit() {
        let (sdb, interner, sink, mut types, env) = setup();
        let std_logic = interner.get_or_intern("std_logic");
        let ti = aion_vhdl_parser::ast::TypeIndication {
            type_mark: aion_vhdl_parser::ast::SelectedName {
                parts: vec![std_logic],
                span: aion_source::Span::DUMMY,
            },
            constraint: None,
            span: aion_source::Span::DUMMY,
        };
        let tid = resolve_vhdl_type(&ti, &mut types, &env, &sdb, &interner, &sink);
        assert_eq!(*types.get(tid), Type::Bit);
    }

    #[test]
    fn vhdl_integer_type() {
        let (sdb, interner, sink, mut types, env) = setup();
        let integer = interner.get_or_intern("integer");
        let ti = aion_vhdl_parser::ast::TypeIndication {
            type_mark: aion_vhdl_parser::ast::SelectedName {
                parts: vec![integer],
                span: aion_source::Span::DUMMY,
            },
            constraint: None,
            span: aion_source::Span::DUMMY,
        };
        let tid = resolve_vhdl_type(&ti, &mut types, &env, &sdb, &interner, &sink);
        assert_eq!(*types.get(tid), Type::Integer);
    }

    #[test]
    fn vhdl_std_logic_vector_with_range() {
        let (mut sdb, interner, sink, mut types, env) = setup();
        let fid = sdb.add_source("test.vhd", "7 0".to_string());
        let slv = interner.get_or_intern("std_logic_vector");
        let ti = aion_vhdl_parser::ast::TypeIndication {
            type_mark: aion_vhdl_parser::ast::SelectedName {
                parts: vec![slv],
                span: aion_source::Span::DUMMY,
            },
            constraint: Some(aion_vhdl_parser::ast::Constraint::Index(
                vec![aion_vhdl_parser::ast::DiscreteRange::Range(
                    aion_vhdl_parser::ast::RangeConstraint {
                        left: Box::new(aion_vhdl_parser::ast::Expr::IntLiteral {
                            span: aion_source::Span::new(fid, 0, 1),
                        }),
                        direction: aion_vhdl_parser::ast::RangeDirection::Downto,
                        right: Box::new(aion_vhdl_parser::ast::Expr::IntLiteral {
                            span: aion_source::Span::new(fid, 2, 3),
                        }),
                        span: aion_source::Span::DUMMY,
                    },
                )],
                aion_source::Span::DUMMY,
            )),
            span: aion_source::Span::DUMMY,
        };
        let tid = resolve_vhdl_type(&ti, &mut types, &env, &sdb, &interner, &sink);
        assert_eq!(
            *types.get(tid),
            Type::BitVec {
                width: 8,
                signed: false
            }
        );
    }

    #[test]
    fn vhdl_unknown_type_emits_diagnostic() {
        let (sdb, interner, sink, mut types, env) = setup();
        let custom = interner.get_or_intern("my_custom_type");
        let ti = aion_vhdl_parser::ast::TypeIndication {
            type_mark: aion_vhdl_parser::ast::SelectedName {
                parts: vec![custom],
                span: aion_source::Span::DUMMY,
            },
            constraint: None,
            span: aion_source::Span::DUMMY,
        };
        let tid = resolve_vhdl_type(&ti, &mut types, &env, &sdb, &interner, &sink);
        assert_eq!(*types.get(tid), Type::Error);
        assert!(sink.has_errors());
    }
}
