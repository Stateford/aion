//! Constant propagation optimization pass.
//!
//! Walks cells topologically: if all inputs to a cell are constants,
//! evaluates the output and replaces the cell with a [`CellKind::Const`].

use crate::netlist::Netlist;
use crate::optimize::OptPass;
use aion_common::LogicVec;
use aion_diagnostics::DiagnosticSink;
use aion_ir::{CellId, CellKind, PortDirection, SignalId, SignalRef};
use std::collections::HashMap;
use std::ops::{BitAnd, BitOr, BitXor, Not};

/// Constant propagation optimization pass.
pub(crate) struct ConstPropPass;

impl OptPass for ConstPropPass {
    fn run(&self, netlist: &mut Netlist, _sink: &DiagnosticSink) -> bool {
        let mut changed = false;

        // Build a map of signal → constant value
        let mut const_signals: HashMap<SignalId, LogicVec> = HashMap::new();

        // First, find all Const cells and record their output signals
        for (cell_id, cell) in netlist.cells.iter() {
            if netlist.is_dead(cell_id) {
                continue;
            }
            if let CellKind::Const { ref value } = cell.kind {
                for conn in &cell.connections {
                    if conn.direction == PortDirection::Output {
                        if let SignalRef::Signal(sig_id) = conn.signal {
                            const_signals.insert(sig_id, value.clone());
                        }
                    }
                }
            }
        }

        // Now try to evaluate cells whose inputs are all constant
        let cell_ids: Vec<CellId> = netlist
            .cells
            .iter()
            .filter(|(id, _)| !netlist.is_dead(*id))
            .map(|(id, _)| id)
            .collect();

        for cell_id in cell_ids {
            if netlist.is_dead(cell_id) {
                continue;
            }
            let cell = netlist.cells.get(cell_id);

            // Skip cells that are already Const
            if matches!(&cell.kind, CellKind::Const { .. }) {
                continue;
            }

            // Collect input values
            let inputs: Vec<Option<LogicVec>> = cell
                .connections
                .iter()
                .filter(|c| c.direction == PortDirection::Input)
                .map(|c| get_const_value(&c.signal, &const_signals))
                .collect();

            // All inputs must be constant
            if inputs.iter().any(|v| v.is_none()) {
                continue;
            }

            let input_values: Vec<LogicVec> = inputs.into_iter().map(|v| v.unwrap()).collect();

            // Try to evaluate
            if let Some(result) = evaluate_cell(&cell.kind, &input_values) {
                // Record the output signal as constant
                for conn in &netlist.cells.get(cell_id).connections {
                    if conn.direction == PortDirection::Output {
                        if let SignalRef::Signal(sig_id) = conn.signal {
                            const_signals.insert(sig_id, result.clone());
                        }
                    }
                }

                // Replace cell with Const
                let output_conn: Vec<_> = netlist
                    .cells
                    .get(cell_id)
                    .connections
                    .iter()
                    .filter(|c| c.direction == PortDirection::Output)
                    .cloned()
                    .collect();

                let cell_mut = netlist.cells.get_mut(cell_id);
                cell_mut.kind = CellKind::Const { value: result };
                cell_mut.connections = output_conn;

                changed = true;
            }
        }

        changed
    }
}

/// Gets the constant value for a signal ref, if available.
fn get_const_value(
    sr: &SignalRef,
    const_signals: &HashMap<SignalId, LogicVec>,
) -> Option<LogicVec> {
    match sr {
        SignalRef::Signal(id) => const_signals.get(id).cloned(),
        SignalRef::Const(lv) => Some(lv.clone()),
        _ => None,
    }
}

/// Evaluates a cell with constant inputs, returning the output value if possible.
fn evaluate_cell(kind: &CellKind, inputs: &[LogicVec]) -> Option<LogicVec> {
    match kind {
        CellKind::Not { width } => {
            let a = inputs.first()?;
            let mut result = LogicVec::new(*width);
            for i in 0..*width {
                let bit = a.get(i);
                result.set(i, Not::not(bit));
            }
            Some(result)
        }

        CellKind::And { width } => {
            if inputs.len() < 2 {
                return None;
            }
            let a = &inputs[0];
            let b = &inputs[1];
            let mut result = LogicVec::new(*width);
            for i in 0..*width {
                result.set(i, BitAnd::bitand(a.get(i), b.get(i)));
            }
            Some(result)
        }

        CellKind::Or { width } => {
            if inputs.len() < 2 {
                return None;
            }
            let a = &inputs[0];
            let b = &inputs[1];
            let mut result = LogicVec::new(*width);
            for i in 0..*width {
                result.set(i, BitOr::bitor(a.get(i), b.get(i)));
            }
            Some(result)
        }

        CellKind::Xor { width } => {
            if inputs.len() < 2 {
                return None;
            }
            let a = &inputs[0];
            let b = &inputs[1];
            let mut result = LogicVec::new(*width);
            for i in 0..*width {
                result.set(i, BitXor::bitxor(a.get(i), b.get(i)));
            }
            Some(result)
        }

        CellKind::Add { width } => {
            if inputs.len() < 2 {
                return None;
            }
            let a_val = inputs[0].to_u64()?;
            let b_val = inputs[1].to_u64()?;
            let result = a_val.wrapping_add(b_val);
            Some(LogicVec::from_u64(result, *width))
        }

        CellKind::Sub { width } => {
            if inputs.len() < 2 {
                return None;
            }
            let a_val = inputs[0].to_u64()?;
            let b_val = inputs[1].to_u64()?;
            let result = a_val.wrapping_sub(b_val);
            Some(LogicVec::from_u64(result, *width))
        }

        CellKind::Mul { width } => {
            if inputs.len() < 2 {
                return None;
            }
            let a_val = inputs[0].to_u64()?;
            let b_val = inputs[1].to_u64()?;
            let result = a_val.wrapping_mul(b_val);
            Some(LogicVec::from_u64(result, *width))
        }

        CellKind::Eq { .. } => {
            if inputs.len() < 2 {
                return None;
            }
            let a_val = inputs[0].to_u64()?;
            let b_val = inputs[1].to_u64()?;
            Some(LogicVec::from_bool(a_val == b_val))
        }

        CellKind::Lt { .. } => {
            if inputs.len() < 2 {
                return None;
            }
            let a_val = inputs[0].to_u64()?;
            let b_val = inputs[1].to_u64()?;
            Some(LogicVec::from_bool(a_val < b_val))
        }

        CellKind::Shl { width } => {
            if inputs.len() < 2 {
                return None;
            }
            let a_val = inputs[0].to_u64()?;
            let b_val = inputs[1].to_u64()?;
            let result = if b_val >= 64 { 0 } else { a_val << b_val };
            Some(LogicVec::from_u64(result, *width))
        }

        CellKind::Shr { width } => {
            if inputs.len() < 2 {
                return None;
            }
            let a_val = inputs[0].to_u64()?;
            let b_val = inputs[1].to_u64()?;
            let result = if b_val >= 64 { 0 } else { a_val >> b_val };
            Some(LogicVec::from_u64(result, *width))
        }

        CellKind::Mux { .. } => {
            // Inputs: S, A (false), B (true)
            if inputs.len() < 3 {
                return None;
            }
            let sel = inputs[0].to_u64()?;
            if sel != 0 {
                Some(inputs[2].clone()) // B (true)
            } else {
                Some(inputs[1].clone()) // A (false)
            }
        }

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use aion_common::{Interner, LogicVec};
    use aion_ir::{Arena, CellKind, Module, Signal, SignalId, SignalKind, SignalRef, Type, TypeDb};
    use aion_source::Span;

    fn make_netlist(interner: &Interner) -> Netlist<'_> {
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);
        let mod_name = interner.get_or_intern("test");
        let a_name = interner.get_or_intern("a");
        let mut signals = Arena::new();
        signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: a_name,
            ty: bit_ty,
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

    #[test]
    fn const_prop_folds_and() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);

        // Create two const cells
        let a = netlist.add_signal("ca", bit_ty, SignalKind::Wire);
        let b = netlist.add_signal("cb", bit_ty, SignalKind::Wire);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);

        netlist.add_cell(
            "c1",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(a))],
        );
        netlist.add_cell(
            "c2",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(b))],
        );
        netlist.add_cell(
            "and",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = ConstPropPass.run(&mut netlist, &sink);
        assert!(changed);

        // The AND cell should now be a Const cell
        let and_cell = netlist.cells.iter().find(|(_, c)| {
            c.connections
                .iter()
                .any(|conn| conn.signal == SignalRef::Signal(out))
        });
        assert!(and_cell.is_some());
        let (_, cell) = and_cell.unwrap();
        assert!(matches!(&cell.kind, CellKind::Const { value } if value.to_u64() == Some(1)));
    }

    #[test]
    fn const_prop_leaves_non_const() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let a = SignalId::from_raw(0); // non-const signal
        let b = netlist.add_signal("cb", bit_ty, SignalKind::Wire);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);

        netlist.add_cell(
            "c1",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(b))],
        );
        netlist.add_cell(
            "and",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = ConstPropPass.run(&mut netlist, &sink);
        assert!(!changed);

        // AND cell should remain
        let has_and = netlist
            .cells
            .iter()
            .any(|(_, c)| matches!(&c.kind, CellKind::And { .. }));
        assert!(has_and);
    }

    #[test]
    fn const_prop_folds_add() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = netlist.types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let a = netlist.add_signal("ca", ty, SignalKind::Wire);
        let b = netlist.add_signal("cb", ty, SignalKind::Wire);
        let out = netlist.add_signal("out", ty, SignalKind::Wire);

        netlist.add_cell(
            "c1",
            CellKind::Const {
                value: LogicVec::from_u64(10, 8),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(a))],
        );
        netlist.add_cell(
            "c2",
            CellKind::Const {
                value: LogicVec::from_u64(20, 8),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(b))],
        );
        netlist.add_cell(
            "add",
            CellKind::Add { width: 8 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = ConstPropPass.run(&mut netlist, &sink);
        assert!(changed);
    }

    #[test]
    fn const_prop_folds_not() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let a = netlist.add_signal("ca", bit_ty, SignalKind::Wire);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);

        netlist.add_cell(
            "c1",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(a))],
        );
        netlist.add_cell(
            "not",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = ConstPropPass.run(&mut netlist, &sink);
        assert!(changed);
    }

    #[test]
    fn const_prop_folds_eq() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = netlist.types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let bit_ty = netlist.types.intern(Type::Bit);
        let a = netlist.add_signal("ca", ty, SignalKind::Wire);
        let b = netlist.add_signal("cb", ty, SignalKind::Wire);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);

        netlist.add_cell(
            "c1",
            CellKind::Const {
                value: LogicVec::from_u64(5, 8),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(a))],
        );
        netlist.add_cell(
            "c2",
            CellKind::Const {
                value: LogicVec::from_u64(5, 8),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(b))],
        );
        netlist.add_cell(
            "eq",
            CellKind::Eq { width: 8 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = ConstPropPass.run(&mut netlist, &sink);
        assert!(changed);
    }

    #[test]
    fn const_prop_folds_mux_select_true() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let sel = netlist.add_signal("sel", bit_ty, SignalKind::Wire);
        let a = netlist.add_signal("a", bit_ty, SignalKind::Wire);
        let b = netlist.add_signal("b", bit_ty, SignalKind::Wire);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);

        netlist.add_cell(
            "sel_c",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(sel))],
        );
        netlist.add_cell(
            "a_c",
            CellKind::Const {
                value: LogicVec::from_bool(false),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(a))],
        );
        netlist.add_cell(
            "b_c",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(b))],
        );
        netlist.add_cell(
            "mux",
            CellKind::Mux {
                width: 1,
                select_width: 1,
            },
            vec![
                netlist.input_conn("S", SignalRef::Signal(sel)),
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = ConstPropPass.run(&mut netlist, &sink);
        assert!(changed);
    }

    #[test]
    fn const_prop_chain() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let a = netlist.add_signal("a", bit_ty, SignalKind::Wire);
        let b = netlist.add_signal("b", bit_ty, SignalKind::Wire);
        // NOT(true) = false, then NOT(false) = true
        // But single pass only propagates one level

        netlist.add_cell(
            "c1",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(a))],
        );
        netlist.add_cell(
            "not1",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.output_conn("Y", SignalRef::Signal(b)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = ConstPropPass.run(&mut netlist, &sink);
        assert!(changed);
    }

    #[test]
    fn evaluate_cell_or() {
        let result = evaluate_cell(
            &CellKind::Or { width: 1 },
            &[LogicVec::from_bool(false), LogicVec::from_bool(true)],
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().to_u64(), Some(1));
    }

    #[test]
    fn evaluate_cell_xor() {
        let result = evaluate_cell(
            &CellKind::Xor { width: 1 },
            &[LogicVec::from_bool(true), LogicVec::from_bool(true)],
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().to_u64(), Some(0));
    }

    #[test]
    fn evaluate_cell_unknown_returns_none() {
        let result = evaluate_cell(&CellKind::Concat, &[LogicVec::from_bool(true)]);
        assert!(result.is_none());
    }
}
