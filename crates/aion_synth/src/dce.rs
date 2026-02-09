//! Dead code elimination (DCE) optimization pass.
//!
//! Removes cells whose outputs are unused — not connected to any live cell's
//! input or to any output port signal. Works backwards from port signals.

use crate::netlist::Netlist;
use crate::optimize::OptPass;
use aion_diagnostics::DiagnosticSink;
use aion_ir::{CellId, PortDirection, SignalId, SignalRef};
use std::collections::HashSet;

/// Dead code elimination pass.
pub(crate) struct DcePass;

impl OptPass for DcePass {
    fn run(&self, netlist: &mut Netlist, _sink: &DiagnosticSink) -> bool {
        let mut changed = false;

        // Step 1: Mark all output port signals as "live roots"
        let mut live_signals: HashSet<SignalId> = HashSet::new();
        for port in &netlist.ports {
            if port.direction == PortDirection::Output || port.direction == PortDirection::InOut {
                live_signals.insert(port.signal);
            }
        }

        // Also mark input port signals as live (they are external)
        for port in &netlist.ports {
            if port.direction == PortDirection::Input {
                live_signals.insert(port.signal);
            }
        }

        // Step 2: Build driver map (signal → cell that drives it)
        let driver_map = netlist.driver_map();

        // Step 3: Build fanout map (signal → cells that read it)
        let fanout_map = netlist.fanout_map();

        // Step 4: Walk backwards from live signals to find all live cells
        let mut live_cells: HashSet<CellId> = HashSet::new();
        let mut worklist: Vec<SignalId> = live_signals.iter().copied().collect();

        while let Some(sig) = worklist.pop() {
            if let Some(&cell_id) = driver_map.get(&sig) {
                if live_cells.insert(cell_id) {
                    // This cell is newly live — mark its input signals as live
                    let cell = netlist.cells.get(cell_id);
                    for conn in &cell.connections {
                        if conn.direction == PortDirection::Input
                            || conn.direction == PortDirection::InOut
                        {
                            for input_sig in extract_signal_ids(&conn.signal) {
                                if live_signals.insert(input_sig) {
                                    worklist.push(input_sig);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Also keep cells that drive signals with fanout (even if not port-connected)
        // This handles cells in combinational logic chains
        for (sig, readers) in &fanout_map {
            for &reader_id in readers {
                if live_cells.contains(&reader_id) {
                    // The reader is live, so the driver must also be live
                    if let Some(&driver_id) = driver_map.get(sig) {
                        if live_cells.insert(driver_id) {
                            // Propagate liveness to driver's inputs
                            let cell = netlist.cells.get(driver_id);
                            for conn in &cell.connections {
                                if conn.direction == PortDirection::Input {
                                    for input_sig in extract_signal_ids(&conn.signal) {
                                        if live_signals.insert(input_sig) {
                                            worklist.push(input_sig);
                                        }
                                    }
                                }
                            }
                            // Re-process the worklist
                            while let Some(s) = worklist.pop() {
                                if let Some(&cid) = driver_map.get(&s) {
                                    if live_cells.insert(cid) {
                                        let c = netlist.cells.get(cid);
                                        for conn in &c.connections {
                                            if conn.direction == PortDirection::Input {
                                                for input_sig in extract_signal_ids(&conn.signal) {
                                                    if live_signals.insert(input_sig) {
                                                        worklist.push(input_sig);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Step 5: Remove dead cells
        let all_cell_ids: Vec<CellId> = netlist
            .cells
            .iter()
            .filter(|(id, _)| !netlist.is_dead(*id))
            .map(|(id, _)| id)
            .collect();

        for cell_id in all_cell_ids {
            if !live_cells.contains(&cell_id) {
                netlist.remove_cell(cell_id);
                changed = true;
            }
        }

        changed
    }
}

/// Extracts all signal IDs from a signal reference.
fn extract_signal_ids(sr: &SignalRef) -> Vec<SignalId> {
    match sr {
        SignalRef::Signal(id) => vec![*id],
        SignalRef::Slice { signal, .. } => vec![*signal],
        SignalRef::Concat(refs) => refs.iter().flat_map(extract_signal_ids).collect(),
        SignalRef::Const(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use aion_common::{Interner, LogicVec};
    use aion_ir::{
        Arena, CellKind, Module, Port, PortDirection, Signal, SignalId, SignalKind, SignalRef,
        Type, TypeDb,
    };
    use aion_source::Span;

    fn make_netlist_with_port(interner: &Interner) -> Netlist<'_> {
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);
        let mod_name = interner.get_or_intern("test");
        let in_name = interner.get_or_intern("in");
        let out_name = interner.get_or_intern("out");

        let mut signals = Arena::new();
        let in_id = signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: in_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let out_id = signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: out_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let module = Module {
            id: aion_ir::ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![
                Port {
                    id: aion_ir::PortId::from_raw(0),
                    name: in_name,
                    direction: PortDirection::Input,
                    ty: bit_ty,
                    signal: in_id,
                    span: Span::DUMMY,
                },
                Port {
                    id: aion_ir::PortId::from_raw(1),
                    name: out_name,
                    direction: PortDirection::Output,
                    ty: bit_ty,
                    signal: out_id,
                    span: Span::DUMMY,
                },
            ],
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
    fn dce_removes_dead_cell() {
        let interner = Interner::new();
        let mut netlist = make_netlist_with_port(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let dead_out = netlist.add_signal("dead", bit_ty, SignalKind::Wire);

        // This cell's output (dead_out) isn't connected to any port
        netlist.add_cell(
            "dead",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.output_conn("Y", SignalRef::Signal(dead_out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = DcePass.run(&mut netlist, &sink);
        assert!(changed);
        assert_eq!(netlist.live_cell_count(), 0);
    }

    #[test]
    fn dce_keeps_live_cell() {
        let interner = Interner::new();
        let mut netlist = make_netlist_with_port(&interner);
        let out_id = SignalId::from_raw(1); // output port signal

        // This cell drives the output port
        netlist.add_cell(
            "live",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.output_conn("Y", SignalRef::Signal(out_id)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = DcePass.run(&mut netlist, &sink);
        assert!(!changed);
        assert_eq!(netlist.live_cell_count(), 1);
    }

    #[test]
    fn dce_transitive_liveness() {
        let interner = Interner::new();
        let mut netlist = make_netlist_with_port(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let intermediate = netlist.add_signal("mid", bit_ty, SignalKind::Wire);
        let out_id = SignalId::from_raw(1);

        // Cell 1: in → mid
        netlist.add_cell(
            "c1",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.output_conn("Y", SignalRef::Signal(intermediate)),
            ],
        );

        // Cell 2: mid → out (port)
        netlist.add_cell(
            "c2",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(intermediate)),
                netlist.output_conn("Y", SignalRef::Signal(out_id)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = DcePass.run(&mut netlist, &sink);
        assert!(!changed);
        assert_eq!(netlist.live_cell_count(), 2);
    }

    #[test]
    fn dce_mixed_live_and_dead() {
        let interner = Interner::new();
        let mut netlist = make_netlist_with_port(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out_id = SignalId::from_raw(1);
        let dead_out = netlist.add_signal("dead", bit_ty, SignalKind::Wire);

        // Live cell: drives output port
        netlist.add_cell(
            "live",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.output_conn("Y", SignalRef::Signal(out_id)),
            ],
        );

        // Dead cell: output not connected to anything
        netlist.add_cell(
            "dead",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.input_conn("B", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.output_conn("Y", SignalRef::Signal(dead_out)),
            ],
        );

        let sink = DiagnosticSink::new();
        let changed = DcePass.run(&mut netlist, &sink);
        assert!(changed);
        assert_eq!(netlist.live_cell_count(), 1);
    }

    #[test]
    fn dce_no_ports_removes_all() {
        let interner = Interner::new();
        let types = TypeDb::new();
        let mod_name = interner.get_or_intern("test");
        let module = Module {
            id: aion_ir::ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![],
            clock_domains: vec![],
            content_hash: aion_common::ContentHash::from_bytes(b"empty"),
        };
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let s = netlist.add_signal("x", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "dead",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(s))],
        );

        let sink = DiagnosticSink::new();
        let changed = DcePass.run(&mut netlist, &sink);
        assert!(changed);
        assert_eq!(netlist.live_cell_count(), 0);
    }

    #[test]
    fn dce_empty_netlist() {
        let interner = Interner::new();
        let types = TypeDb::new();
        let mod_name = interner.get_or_intern("test");
        let module = Module {
            id: aion_ir::ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![],
            clock_domains: vec![],
            content_hash: aion_common::ContentHash::from_bytes(b"empty"),
        };
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        let changed = DcePass.run(&mut netlist, &sink);
        assert!(!changed);
    }

    #[test]
    fn dce_already_dead_cells_ignored() {
        let interner = Interner::new();
        let mut netlist = make_netlist_with_port(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let dead_out = netlist.add_signal("dead", bit_ty, SignalKind::Wire);
        let cell_id = netlist.add_cell(
            "dead",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.output_conn("Y", SignalRef::Signal(dead_out)),
            ],
        );
        // Pre-mark as dead
        netlist.remove_cell(cell_id);

        let sink = DiagnosticSink::new();
        let changed = DcePass.run(&mut netlist, &sink);
        assert!(!changed); // Already dead, nothing new to remove
    }
}
