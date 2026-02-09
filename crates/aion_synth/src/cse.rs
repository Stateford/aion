//! Common subexpression elimination (CSE) optimization pass.
//!
//! Identifies cells with identical kinds and identical input signals,
//! then merges their outputs so that only one copy is retained.

use crate::netlist::Netlist;
use crate::optimize::OptPass;
use aion_diagnostics::DiagnosticSink;
use aion_ir::{CellId, CellKind, PortDirection, SignalId, SignalRef};
use std::collections::HashMap;

/// Common subexpression elimination pass.
pub(crate) struct CsePass;

impl OptPass for CsePass {
    fn run(&self, netlist: &mut Netlist, _sink: &DiagnosticSink) -> bool {
        let mut changed = false;

        // Build a map of (cell_kind_key, sorted_input_signals) → first cell_id
        let mut seen: HashMap<CseKey, CellId> = HashMap::new();

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

            // Skip cells that can't be deduplicated
            if !is_pure_cell(&cell.kind) {
                continue;
            }

            let key = make_cse_key(&cell.kind, &cell.connections);

            if let Some(&existing_id) = seen.get(&key) {
                // Found a duplicate — redirect the duplicate's output to the existing cell's output
                // Get the existing cell's output signal
                let existing_output = get_output_signal(netlist, existing_id);
                let dup_output = get_output_signal(netlist, cell_id);

                if let (Some(existing_sig), Some(dup_sig)) = (existing_output, dup_output) {
                    if existing_sig != dup_sig {
                        // Redirect: replace all uses of dup_sig with existing_sig
                        redirect_signal(netlist, dup_sig, existing_sig);
                        netlist.remove_cell(cell_id);
                        changed = true;
                    }
                }
            } else {
                seen.insert(key, cell_id);
            }
        }

        changed
    }
}

/// A key for identifying identical cells.
#[derive(Hash, PartialEq, Eq, Clone, Debug)]
struct CseKey {
    kind_tag: String,
    inputs: Vec<SignalId>,
}

/// Creates a CSE key from a cell's kind and connections.
fn make_cse_key(kind: &CellKind, connections: &[aion_ir::Connection]) -> CseKey {
    let kind_tag = format!("{kind:?}");
    let mut inputs: Vec<SignalId> = connections
        .iter()
        .filter(|c| c.direction == PortDirection::Input)
        .filter_map(|c| match &c.signal {
            SignalRef::Signal(id) => Some(*id),
            _ => None,
        })
        .collect();
    // Sort for commutative operations (AND, OR, XOR, ADD, MUL, EQ)
    if is_commutative(kind) {
        inputs.sort_by_key(|id| id.as_raw());
    }
    CseKey { kind_tag, inputs }
}

/// Returns true if the cell kind is a pure function (no side effects, deterministic).
fn is_pure_cell(kind: &CellKind) -> bool {
    matches!(
        kind,
        CellKind::And { .. }
            | CellKind::Or { .. }
            | CellKind::Xor { .. }
            | CellKind::Not { .. }
            | CellKind::Add { .. }
            | CellKind::Sub { .. }
            | CellKind::Mul { .. }
            | CellKind::Shl { .. }
            | CellKind::Shr { .. }
            | CellKind::Eq { .. }
            | CellKind::Lt { .. }
            | CellKind::Mux { .. }
            | CellKind::Const { .. }
            | CellKind::Concat
            | CellKind::Slice { .. }
    )
}

/// Returns true if the operation is commutative (input order doesn't matter).
fn is_commutative(kind: &CellKind) -> bool {
    matches!(
        kind,
        CellKind::And { .. }
            | CellKind::Or { .. }
            | CellKind::Xor { .. }
            | CellKind::Add { .. }
            | CellKind::Mul { .. }
            | CellKind::Eq { .. }
    )
}

/// Gets the output signal of a cell.
fn get_output_signal(netlist: &Netlist, cell_id: CellId) -> Option<SignalId> {
    let cell = netlist.cells.get(cell_id);
    for conn in &cell.connections {
        if conn.direction == PortDirection::Output {
            if let SignalRef::Signal(id) = conn.signal {
                return Some(id);
            }
        }
    }
    None
}

/// Redirects all uses of `old_sig` to `new_sig` in all cell input connections.
fn redirect_signal(netlist: &mut Netlist, old_sig: SignalId, new_sig: SignalId) {
    let cell_ids: Vec<CellId> = netlist
        .cells
        .iter()
        .filter(|(id, _)| !netlist.is_dead(*id))
        .map(|(id, _)| id)
        .collect();

    for cell_id in cell_ids {
        let cell = netlist.cells.get_mut(cell_id);
        for conn in &mut cell.connections {
            if conn.direction == PortDirection::Input || conn.direction == PortDirection::InOut {
                replace_signal_in_ref(&mut conn.signal, old_sig, new_sig);
            }
        }
    }
}

/// Replaces occurrences of `old` with `new` in a signal reference.
fn replace_signal_in_ref(sr: &mut SignalRef, old: SignalId, new: SignalId) {
    match sr {
        SignalRef::Signal(id) => {
            if *id == old {
                *id = new;
            }
        }
        SignalRef::Slice { signal, .. } => {
            if *signal == old {
                *signal = new;
            }
        }
        SignalRef::Concat(refs) => {
            for r in refs {
                replace_signal_in_ref(r, old, new);
            }
        }
        SignalRef::Const(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use aion_common::Interner;
    use aion_ir::{Arena, CellKind, Module, Signal, SignalId, SignalKind, SignalRef, Type, TypeDb};
    use aion_source::Span;

    fn make_netlist(interner: &Interner) -> Netlist<'_> {
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);
        let mod_name = interner.get_or_intern("test");
        let a_name = interner.get_or_intern("a");
        let b_name = interner.get_or_intern("b");
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
        signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: b_name,
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
    fn cse_merges_identical_cells() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out1 = netlist.add_signal("o1", bit_ty, SignalKind::Wire);
        let out2 = netlist.add_signal("o2", bit_ty, SignalKind::Wire);

        let a = SignalId::from_raw(0);
        let b = SignalId::from_raw(1);

        netlist.add_cell(
            "and1",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out1)),
            ],
        );
        netlist.add_cell(
            "and2",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out2)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = CsePass.run(&mut netlist, &sink);
        assert!(changed);
        assert_eq!(netlist.live_cell_count(), 1);
    }

    #[test]
    fn cse_preserves_different_cells() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out1 = netlist.add_signal("o1", bit_ty, SignalKind::Wire);
        let out2 = netlist.add_signal("o2", bit_ty, SignalKind::Wire);

        let a = SignalId::from_raw(0);
        let b = SignalId::from_raw(1);

        netlist.add_cell(
            "and",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out1)),
            ],
        );
        netlist.add_cell(
            "or",
            CellKind::Or { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out2)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = CsePass.run(&mut netlist, &sink);
        assert!(!changed);
        assert_eq!(netlist.live_cell_count(), 2);
    }

    #[test]
    fn cse_commutative_reorder() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out1 = netlist.add_signal("o1", bit_ty, SignalKind::Wire);
        let out2 = netlist.add_signal("o2", bit_ty, SignalKind::Wire);

        let a = SignalId::from_raw(0);
        let b = SignalId::from_raw(1);

        // AND(a, b) and AND(b, a) should be recognized as identical
        netlist.add_cell(
            "and1",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out1)),
            ],
        );
        netlist.add_cell(
            "and2",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(b)),
                netlist.input_conn("B", SignalRef::Signal(a)),
                netlist.output_conn("Y", SignalRef::Signal(out2)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = CsePass.run(&mut netlist, &sink);
        assert!(changed);
        assert_eq!(netlist.live_cell_count(), 1);
    }

    #[test]
    fn cse_non_commutative_different_order() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out1 = netlist.add_signal("o1", bit_ty, SignalKind::Wire);
        let out2 = netlist.add_signal("o2", bit_ty, SignalKind::Wire);

        let a = SignalId::from_raw(0);
        let b = SignalId::from_raw(1);

        // Sub(a, b) and Sub(b, a) are different
        netlist.add_cell(
            "sub1",
            CellKind::Sub { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out1)),
            ],
        );
        netlist.add_cell(
            "sub2",
            CellKind::Sub { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(b)),
                netlist.input_conn("B", SignalRef::Signal(a)),
                netlist.output_conn("Y", SignalRef::Signal(out2)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = CsePass.run(&mut netlist, &sink);
        assert!(!changed);
        assert_eq!(netlist.live_cell_count(), 2);
    }

    #[test]
    fn cse_redirects_downstream_users() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out1 = netlist.add_signal("o1", bit_ty, SignalKind::Wire);
        let out2 = netlist.add_signal("o2", bit_ty, SignalKind::Wire);
        let final_out = netlist.add_signal("final", bit_ty, SignalKind::Wire);

        let a = SignalId::from_raw(0);
        let b = SignalId::from_raw(1);

        netlist.add_cell(
            "and1",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out1)),
            ],
        );
        netlist.add_cell(
            "and2",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out2)),
            ],
        );
        // This cell uses out2 as input — after CSE, should use out1
        netlist.add_cell(
            "not",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(out2)),
                netlist.output_conn("Y", SignalRef::Signal(final_out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = CsePass.run(&mut netlist, &sink);
        assert!(changed);

        // The NOT cell's input should now reference out1 instead of out2
        let not_cell = netlist
            .cells
            .iter()
            .find(|(id, c)| !netlist.is_dead(*id) && matches!(&c.kind, CellKind::Not { .. }));
        assert!(not_cell.is_some());
        let (_, not_cell) = not_cell.unwrap();
        let input_sig = not_cell
            .connections
            .iter()
            .find(|c| c.direction == PortDirection::Input);
        assert!(input_sig.is_some());
        assert_eq!(input_sig.unwrap().signal, SignalRef::Signal(out1));
    }

    #[test]
    fn cse_skips_dff() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out1 = netlist.add_signal("o1", bit_ty, SignalKind::Wire);
        let out2 = netlist.add_signal("o2", bit_ty, SignalKind::Wire);

        let a = SignalId::from_raw(0);

        // DFFs are not pure — should not be merged
        netlist.add_cell(
            "dff1",
            CellKind::Dff {
                width: 1,
                has_reset: false,
                has_enable: false,
            },
            vec![
                netlist.input_conn("D", SignalRef::Signal(a)),
                netlist.output_conn("Q", SignalRef::Signal(out1)),
            ],
        );
        netlist.add_cell(
            "dff2",
            CellKind::Dff {
                width: 1,
                has_reset: false,
                has_enable: false,
            },
            vec![
                netlist.input_conn("D", SignalRef::Signal(a)),
                netlist.output_conn("Q", SignalRef::Signal(out2)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = CsePass.run(&mut netlist, &sink);
        assert!(!changed);
        assert_eq!(netlist.live_cell_count(), 2);
    }

    #[test]
    fn is_pure_cell_classification() {
        assert!(is_pure_cell(&CellKind::And { width: 1 }));
        assert!(is_pure_cell(&CellKind::Or { width: 1 }));
        assert!(is_pure_cell(&CellKind::Not { width: 1 }));
        assert!(is_pure_cell(&CellKind::Add { width: 8 }));
        assert!(is_pure_cell(&CellKind::Mux {
            width: 1,
            select_width: 1,
        }));
        assert!(!is_pure_cell(&CellKind::Dff {
            width: 1,
            has_reset: false,
            has_enable: false,
        }));
        assert!(!is_pure_cell(&CellKind::Latch { width: 1 }));
    }
}
