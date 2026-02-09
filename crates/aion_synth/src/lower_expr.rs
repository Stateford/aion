//! Expression lowering: walks [`Expr`] trees and emits cells + intermediate signals.
//!
//! Each expression variant is decomposed into one or more cells in the netlist,
//! with temporary signals connecting them. The return value is a [`SignalRef`]
//! pointing to the output of the generated cell network.

use crate::netlist::Netlist;
use aion_common::LogicVec;
use aion_ir::{BinaryOp, CellKind, Expr, SignalKind, SignalRef, Type, UnaryOp};

/// Lowers an expression into the netlist, returning a reference to the output signal.
///
/// Recursively walks the expression tree, creating cells for each operation
/// and wiring them together with temporary signals.
pub(crate) fn lower_expr(expr: &Expr, netlist: &mut Netlist) -> SignalRef {
    match expr {
        Expr::Literal(lv) => {
            let width = lv.width();
            let ty = if width == 1 {
                netlist.types.intern(Type::Bit)
            } else {
                netlist.types.intern(Type::BitVec {
                    width,
                    signed: false,
                })
            };
            let out = netlist.add_signal("const", ty, SignalKind::Wire);
            netlist.add_cell(
                "const",
                CellKind::Const { value: lv.clone() },
                vec![netlist.output_conn("Y", SignalRef::Signal(out))],
            );
            SignalRef::Signal(out)
        }

        Expr::Signal(sr) => sr.clone(),

        Expr::Unary {
            op, operand, ty, ..
        } => {
            let input = lower_expr(operand, netlist);
            let width = netlist.types.bit_width(*ty).unwrap_or(1);
            let out_ty = if width == 1 {
                netlist.types.intern(Type::Bit)
            } else {
                netlist.types.intern(Type::BitVec {
                    width,
                    signed: false,
                })
            };
            let out = netlist.add_signal("unary", out_ty, SignalKind::Wire);

            let (cell_kind, input_name) = match op {
                UnaryOp::Not => (CellKind::Not { width }, "A"),
                UnaryOp::Neg => {
                    // Negate: subtract from zero
                    let zero_lv = LogicVec::all_zero(width);
                    let zero_ref = lower_expr(&Expr::Literal(zero_lv), netlist);
                    netlist.add_cell(
                        "neg",
                        CellKind::Sub { width },
                        vec![
                            netlist.input_conn("A", zero_ref),
                            netlist.input_conn("B", input),
                            netlist.output_conn("Y", SignalRef::Signal(out)),
                        ],
                    );
                    return SignalRef::Signal(out);
                }
                UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor => {
                    // Reduction ops: fold bits with the corresponding gate
                    // For now, produce a single-output reduction cell
                    // We represent these as a 1-bit output gate with the full input
                    let gate_kind = match op {
                        UnaryOp::RedAnd => CellKind::And { width: 1 },
                        UnaryOp::RedOr => CellKind::Or { width: 1 },
                        UnaryOp::RedXor => CellKind::Xor { width: 1 },
                        _ => unreachable!(),
                    };
                    netlist.add_cell(
                        "reduce",
                        gate_kind,
                        vec![
                            netlist.input_conn("A", input),
                            netlist.output_conn("Y", SignalRef::Signal(out)),
                        ],
                    );
                    return SignalRef::Signal(out);
                }
                UnaryOp::LogicNot => {
                    // Logical not: reduce to single bit, then invert
                    let one_bit_ty = netlist.types.intern(Type::Bit);
                    let reduced = netlist.add_signal("lnot_red", one_bit_ty, SignalKind::Wire);
                    netlist.add_cell(
                        "lnot_reduce",
                        CellKind::Or { width: 1 },
                        vec![
                            netlist.input_conn("A", input),
                            netlist.output_conn("Y", SignalRef::Signal(reduced)),
                        ],
                    );
                    netlist.add_cell(
                        "lnot",
                        CellKind::Not { width: 1 },
                        vec![
                            netlist.input_conn("A", SignalRef::Signal(reduced)),
                            netlist.output_conn("Y", SignalRef::Signal(out)),
                        ],
                    );
                    return SignalRef::Signal(out);
                }
            };

            netlist.add_cell(
                "unary",
                cell_kind,
                vec![
                    netlist.input_conn(input_name, input),
                    netlist.output_conn("Y", SignalRef::Signal(out)),
                ],
            );
            SignalRef::Signal(out)
        }

        Expr::Binary {
            op, lhs, rhs, ty, ..
        } => {
            let left = lower_expr(lhs, netlist);
            let right = lower_expr(rhs, netlist);
            let width = netlist.types.bit_width(*ty).unwrap_or(1);
            let out_ty = if width == 1 {
                netlist.types.intern(Type::Bit)
            } else {
                netlist.types.intern(Type::BitVec {
                    width,
                    signed: false,
                })
            };
            let out = netlist.add_signal("binop", out_ty, SignalKind::Wire);

            let cell_kind = match op {
                BinaryOp::Add => CellKind::Add { width },
                BinaryOp::Sub => CellKind::Sub { width },
                BinaryOp::Mul => CellKind::Mul { width },
                BinaryOp::And => CellKind::And { width },
                BinaryOp::Or => CellKind::Or { width },
                BinaryOp::Xor => CellKind::Xor { width },
                BinaryOp::Shl => CellKind::Shl { width },
                BinaryOp::Shr => CellKind::Shr { width },
                BinaryOp::Eq | BinaryOp::Ne => CellKind::Eq { width },
                BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => CellKind::Lt { width },
                BinaryOp::LogicAnd | BinaryOp::LogicOr => {
                    // Logic and/or: reduce both to 1-bit then combine
                    let one_bit_ty = netlist.types.intern(Type::Bit);
                    let la = netlist.add_signal("logic_a", one_bit_ty, SignalKind::Wire);
                    let lb = netlist.add_signal("logic_b", one_bit_ty, SignalKind::Wire);
                    netlist.add_cell(
                        "logic_red_a",
                        CellKind::Or { width: 1 },
                        vec![
                            netlist.input_conn("A", left),
                            netlist.output_conn("Y", SignalRef::Signal(la)),
                        ],
                    );
                    netlist.add_cell(
                        "logic_red_b",
                        CellKind::Or { width: 1 },
                        vec![
                            netlist.input_conn("A", right),
                            netlist.output_conn("Y", SignalRef::Signal(lb)),
                        ],
                    );
                    let gate = if *op == BinaryOp::LogicAnd {
                        CellKind::And { width: 1 }
                    } else {
                        CellKind::Or { width: 1 }
                    };
                    netlist.add_cell(
                        "logic_comb",
                        gate,
                        vec![
                            netlist.input_conn("A", SignalRef::Signal(la)),
                            netlist.input_conn("B", SignalRef::Signal(lb)),
                            netlist.output_conn("Y", SignalRef::Signal(out)),
                        ],
                    );
                    return SignalRef::Signal(out);
                }
                BinaryOp::Div | BinaryOp::Mod | BinaryOp::Pow => {
                    // These are not directly synthesizable — emit a blackbox
                    let port_a = netlist.intern("A");
                    let port_b = netlist.intern("B");
                    netlist.add_cell(
                        "unsupported",
                        CellKind::BlackBox {
                            port_names: vec![port_a, port_b],
                        },
                        vec![
                            netlist.input_conn("A", left),
                            netlist.input_conn("B", right),
                            netlist.output_conn("Y", SignalRef::Signal(out)),
                        ],
                    );
                    return SignalRef::Signal(out);
                }
            };

            netlist.add_cell(
                "binop",
                cell_kind,
                vec![
                    netlist.input_conn("A", left),
                    netlist.input_conn("B", right),
                    netlist.output_conn("Y", SignalRef::Signal(out)),
                ],
            );

            // For Ne, Gt, Ge, Le: add inverter or swap operands
            match op {
                BinaryOp::Ne => {
                    let inv_out_ty = netlist.types.intern(Type::Bit);
                    let inv_out = netlist.add_signal("ne_inv", inv_out_ty, SignalKind::Wire);
                    netlist.add_cell(
                        "ne_inv",
                        CellKind::Not { width: 1 },
                        vec![
                            netlist.input_conn("A", SignalRef::Signal(out)),
                            netlist.output_conn("Y", SignalRef::Signal(inv_out)),
                        ],
                    );
                    SignalRef::Signal(inv_out)
                }
                _ => SignalRef::Signal(out),
            }
        }

        Expr::Ternary {
            condition,
            true_val,
            false_val,
            ty,
            ..
        } => {
            let cond = lower_expr(condition, netlist);
            let t_val = lower_expr(true_val, netlist);
            let f_val = lower_expr(false_val, netlist);
            let width = netlist.types.bit_width(*ty).unwrap_or(1);
            let out_ty = if width == 1 {
                netlist.types.intern(Type::Bit)
            } else {
                netlist.types.intern(Type::BitVec {
                    width,
                    signed: false,
                })
            };
            let out = netlist.add_signal("mux", out_ty, SignalKind::Wire);
            netlist.add_cell(
                "mux",
                CellKind::Mux {
                    width,
                    select_width: 1,
                },
                vec![
                    netlist.input_conn("S", cond),
                    netlist.input_conn("A", f_val),
                    netlist.input_conn("B", t_val),
                    netlist.output_conn("Y", SignalRef::Signal(out)),
                ],
            );
            SignalRef::Signal(out)
        }

        Expr::Concat(exprs) => {
            let inputs: Vec<SignalRef> = exprs.iter().map(|e| lower_expr(e, netlist)).collect();
            // Calculate total width
            let total_width: u32 = exprs
                .iter()
                .map(|e| expr_width(e, netlist).unwrap_or(1))
                .sum();
            let out_ty = netlist.types.intern(Type::BitVec {
                width: total_width,
                signed: false,
            });
            let out = netlist.add_signal("concat", out_ty, SignalKind::Wire);
            let mut conns: Vec<_> = inputs
                .into_iter()
                .enumerate()
                .map(|(i, sr)| netlist.input_conn(&format!("I{i}"), sr))
                .collect();
            conns.push(netlist.output_conn("Y", SignalRef::Signal(out)));
            netlist.add_cell("concat", CellKind::Concat, conns);
            SignalRef::Signal(out)
        }

        Expr::Repeat { expr, count, .. } => {
            let input = lower_expr(expr, netlist);
            let inner_width = expr_width(expr, netlist).unwrap_or(1);
            let total_width = inner_width * count;
            let out_ty = netlist.types.intern(Type::BitVec {
                width: total_width,
                signed: false,
            });
            let out = netlist.add_signal("repeat", out_ty, SignalKind::Wire);
            netlist.add_cell(
                "repeat",
                CellKind::Repeat { count: *count },
                vec![
                    netlist.input_conn("A", input),
                    netlist.output_conn("Y", SignalRef::Signal(out)),
                ],
            );
            SignalRef::Signal(out)
        }

        Expr::Index { expr, index, .. } => {
            let input = lower_expr(expr, netlist);
            let idx = lower_expr(index, netlist);
            let out_ty = netlist.types.intern(Type::Bit);
            let out = netlist.add_signal("index", out_ty, SignalKind::Wire);
            // Dynamic index — use a mux-based selector
            netlist.add_cell(
                "index",
                CellKind::Slice {
                    offset: 0,
                    width: 1,
                },
                vec![
                    netlist.input_conn("A", input),
                    netlist.input_conn("S", idx),
                    netlist.output_conn("Y", SignalRef::Signal(out)),
                ],
            );
            SignalRef::Signal(out)
        }

        Expr::Slice {
            expr, high, low, ..
        } => {
            let input = lower_expr(expr, netlist);
            // Try to evaluate high/low as constants
            let high_val = const_eval_expr(high);
            let low_val = const_eval_expr(low);
            match (high_val, low_val) {
                (Some(h), Some(l)) => {
                    let width = (h - l + 1) as u32;
                    let out_ty = if width == 1 {
                        netlist.types.intern(Type::Bit)
                    } else {
                        netlist.types.intern(Type::BitVec {
                            width,
                            signed: false,
                        })
                    };
                    let out = netlist.add_signal("slice", out_ty, SignalKind::Wire);
                    netlist.add_cell(
                        "slice",
                        CellKind::Slice {
                            offset: l as u32,
                            width,
                        },
                        vec![
                            netlist.input_conn("A", input),
                            netlist.output_conn("Y", SignalRef::Signal(out)),
                        ],
                    );
                    SignalRef::Signal(out)
                }
                _ => {
                    // Dynamic slice — lower high and low as signals
                    let h_sig = lower_expr(high, netlist);
                    let l_sig = lower_expr(low, netlist);
                    let out_ty = netlist.types.intern(Type::BitVec {
                        width: 1,
                        signed: false,
                    });
                    let out = netlist.add_signal("dyn_slice", out_ty, SignalKind::Wire);
                    netlist.add_cell(
                        "dyn_slice",
                        CellKind::Slice {
                            offset: 0,
                            width: 1,
                        },
                        vec![
                            netlist.input_conn("A", input),
                            netlist.input_conn("H", h_sig),
                            netlist.input_conn("L", l_sig),
                            netlist.output_conn("Y", SignalRef::Signal(out)),
                        ],
                    );
                    SignalRef::Signal(out)
                }
            }
        }

        Expr::FuncCall { .. } => {
            // Function calls are not directly synthesizable — emit const 0
            let out_ty = netlist.types.intern(Type::Bit);
            let out = netlist.add_signal("func_stub", out_ty, SignalKind::Wire);
            netlist.add_cell(
                "func_stub",
                CellKind::Const {
                    value: LogicVec::all_zero(1),
                },
                vec![netlist.output_conn("Y", SignalRef::Signal(out))],
            );
            SignalRef::Signal(out)
        }
    }
}

/// Tries to evaluate an expression as a constant integer.
fn const_eval_expr(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Literal(lv) => lv.to_u64().map(|v| v as i64),
        _ => None,
    }
}

/// Estimates the bit width of an expression from the netlist context.
fn expr_width(expr: &Expr, netlist: &Netlist) -> Option<u32> {
    match expr {
        Expr::Literal(lv) => Some(lv.width()),
        Expr::Signal(SignalRef::Signal(id)) => Some(netlist.signal_width(*id)),
        Expr::Signal(SignalRef::Slice { high, low, .. }) => Some(high - low + 1),
        Expr::Signal(SignalRef::Const(lv)) => Some(lv.width()),
        Expr::Binary { ty, .. } | Expr::Unary { ty, .. } | Expr::Ternary { ty, .. } => {
            netlist.types.bit_width(*ty)
        }
        Expr::Concat(exprs) => {
            let mut total = 0u32;
            for e in exprs {
                total += expr_width(e, netlist)?;
            }
            Some(total)
        }
        Expr::Repeat { expr, count, .. } => expr_width(expr, netlist).map(|w| w * count),
        Expr::Index { .. } => Some(1),
        Expr::Slice { high, low, .. } => {
            let h = const_eval_expr(high)?;
            let l = const_eval_expr(low)?;
            Some((h - l + 1) as u32)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use aion_common::Interner;
    use aion_ir::{Arena, Module, Signal, SignalId, SignalKind, Type, TypeDb, TypeId};
    use aion_source::Span;

    fn make_netlist(interner: &Interner) -> Netlist<'_> {
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);
        let vec8_ty = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let mod_name = interner.get_or_intern("test");
        let mut signals = Arena::new();
        let a_name = interner.get_or_intern("a");
        signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: a_name,
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let b_name = interner.get_or_intern("b");
        signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: b_name,
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let bus_name = interner.get_or_intern("bus");
        signals.alloc(Signal {
            id: SignalId::from_raw(2),
            name: bus_name,
            ty: vec8_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let module = Module {
            id: aion_ir::ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals,
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![],
            clock_domains: vec![],
            content_hash: aion_common::ContentHash::from_bytes(b"test"),
        };
        Netlist::from_module(&module, &types, interner)
    }

    fn bit_ty(netlist: &mut Netlist) -> TypeId {
        netlist.types.intern(Type::Bit)
    }

    fn vec8_ty(netlist: &mut Netlist) -> TypeId {
        netlist.types.intern(Type::BitVec {
            width: 8,
            signed: false,
        })
    }

    #[test]
    fn lower_literal() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let expr = Expr::Literal(LogicVec::from_u64(42, 8));
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        // Should create a Const cell
        let has_const = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Const { .. }));
        assert!(has_const);
    }

    #[test]
    fn lower_signal_passthrough() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let sig_ref = SignalRef::Signal(SignalId::from_raw(0));
        let expr = Expr::Signal(sig_ref.clone());
        let out = lower_expr(&expr, &mut netlist);
        assert_eq!(out, sig_ref);
        assert_eq!(netlist.cells.len(), 0);
    }

    #[test]
    fn lower_not() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_not = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Not { .. }));
        assert!(has_not);
    }

    #[test]
    fn lower_neg() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = vec8_ty(&mut netlist);
        let expr = Expr::Unary {
            op: UnaryOp::Neg,
            operand: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(2)))),
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        // Should have a Sub cell (0 - operand)
        let has_sub = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Sub { .. }));
        assert!(has_sub);
    }

    #[test]
    fn lower_binary_add() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = vec8_ty(&mut netlist);
        let expr = Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(2)))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(1, 8))),
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_add = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Add { .. }));
        assert!(has_add);
    }

    #[test]
    fn lower_binary_and() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::Binary {
            op: BinaryOp::And,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            rhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_and = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::And { .. }));
        assert!(has_and);
    }

    #[test]
    fn lower_binary_eq() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            rhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_eq = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Eq { .. }));
        assert!(has_eq);
    }

    #[test]
    fn lower_binary_ne_has_inverter() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::Binary {
            op: BinaryOp::Ne,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            rhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
            ty,
            span: Span::DUMMY,
        };
        let _out = lower_expr(&expr, &mut netlist);
        // Should have both Eq and Not cells
        let has_eq = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Eq { .. }));
        let has_not = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Not { .. }));
        assert!(has_eq);
        assert!(has_not);
    }

    #[test]
    fn lower_ternary_mux() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::Ternary {
            condition: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            true_val: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
            false_val: Box::new(Expr::Literal(LogicVec::from_bool(false))),
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_mux = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Mux { .. }));
        assert!(has_mux);
    }

    #[test]
    fn lower_concat() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let expr = Expr::Concat(vec![
            Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
            Expr::Signal(SignalRef::Signal(SignalId::from_raw(1))),
        ]);
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_concat = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Concat));
        assert!(has_concat);
    }

    #[test]
    fn lower_repeat() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let expr = Expr::Repeat {
            expr: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            count: 4,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_repeat = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Repeat { count: 4 }));
        assert!(has_repeat);
    }

    #[test]
    fn lower_slice_static() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let expr = Expr::Slice {
            expr: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(2)))),
            high: Box::new(Expr::Literal(LogicVec::from_u64(3, 32))),
            low: Box::new(Expr::Literal(LogicVec::from_u64(0, 32))),
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_slice = netlist.cells.iter().any(|(_, c)| {
            matches!(
                &c.kind,
                CellKind::Slice {
                    offset: 0,
                    width: 4
                }
            )
        });
        assert!(has_slice);
    }

    #[test]
    fn lower_index() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let expr = Expr::Index {
            expr: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(2)))),
            index: Box::new(Expr::Literal(LogicVec::from_u64(0, 32))),
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
    }

    #[test]
    fn lower_logic_and() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::Binary {
            op: BinaryOp::LogicAnd,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            rhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
            ty,
            span: Span::DUMMY,
        };
        let _out = lower_expr(&expr, &mut netlist);
        // Should have reduction OR cells + AND cell
        let and_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| matches!(&c.kind, CellKind::And { .. }))
            .count();
        assert!(and_count >= 1);
    }

    #[test]
    fn lower_reduction_xor() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::Unary {
            op: UnaryOp::RedXor,
            operand: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(2)))),
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        let has_xor = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Xor { width: 1 }));
        assert!(has_xor);
    }

    #[test]
    fn lower_logic_not() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::Unary {
            op: UnaryOp::LogicNot,
            operand: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            ty,
            span: Span::DUMMY,
        };
        let _out = lower_expr(&expr, &mut netlist);
        // Should have OR (reduce) + NOT cells
        let has_or = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Or { .. }));
        let has_not = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Not { .. }));
        assert!(has_or);
        assert!(has_not);
    }

    #[test]
    fn lower_mul() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = vec8_ty(&mut netlist);
        let expr = Expr::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(2)))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(2, 8))),
            ty,
            span: Span::DUMMY,
        };
        let _out = lower_expr(&expr, &mut netlist);
        let has_mul = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Mul { .. }));
        assert!(has_mul);
    }

    #[test]
    fn lower_nested_expr() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        // (a & b) | a
        let expr = Expr::Binary {
            op: BinaryOp::Or,
            lhs: Box::new(Expr::Binary {
                op: BinaryOp::And,
                lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
                rhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
                ty,
                span: Span::DUMMY,
            }),
            rhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
        // Should have both And and Or cells
        let has_and = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::And { .. }));
        let has_or = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Or { .. }));
        assert!(has_and);
        assert!(has_or);
    }

    #[test]
    fn lower_func_call_emits_stub() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = bit_ty(&mut netlist);
        let expr = Expr::FuncCall {
            name: netlist.intern("clog2"),
            args: vec![Expr::Literal(LogicVec::from_u64(8, 32))],
            ty,
            span: Span::DUMMY,
        };
        let out = lower_expr(&expr, &mut netlist);
        assert!(matches!(out, SignalRef::Signal(_)));
    }

    #[test]
    fn lower_shift_ops() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = vec8_ty(&mut netlist);
        let expr = Expr::Binary {
            op: BinaryOp::Shl,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(2)))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(1, 8))),
            ty,
            span: Span::DUMMY,
        };
        let _out = lower_expr(&expr, &mut netlist);
        let has_shl = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::Shl { .. }));
        assert!(has_shl);
    }

    #[test]
    fn lower_div_emits_blackbox() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = vec8_ty(&mut netlist);
        let expr = Expr::Binary {
            op: BinaryOp::Div,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(2)))),
            rhs: Box::new(Expr::Literal(LogicVec::from_u64(2, 8))),
            ty,
            span: Span::DUMMY,
        };
        let _out = lower_expr(&expr, &mut netlist);
        let has_bbox = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::BlackBox { .. }));
        assert!(has_bbox);
    }
}
