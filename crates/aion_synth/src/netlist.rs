//! Internal mutable netlist for synthesis.
//!
//! The [`Netlist`] is the working representation for a single module during
//! synthesis. It wraps arenas of signals and cells with helper methods for
//! building cell networks, querying connectivity, and removing dead cells.

use aion_common::{Ident, Interner};
use aion_ir::{
    Arena, Assignment, Cell, CellId, CellKind, Connection, Module, Port, PortDirection, Signal,
    SignalId, SignalKind, SignalRef, TypeDb, TypeId,
};
use aion_source::Span;
use std::collections::HashMap;

/// A mutable netlist representing a single module during synthesis.
///
/// Contains signals and cells that can be added, removed, and queried.
/// Built from an IR [`Module`] and progressively transformed through
/// lowering, optimization, and technology mapping.
pub(crate) struct Netlist<'a> {
    /// All signals in the netlist.
    pub signals: Arena<SignalId, Signal>,
    /// All cells in the netlist.
    pub cells: Arena<CellId, Cell>,
    /// Type database shared with the design.
    pub types: TypeDb,
    /// String interner (borrowed from caller).
    pub interner: &'a Interner,
    /// Counter for generating temporary signal names.
    next_tmp: u32,
    /// Counter for generating cell names.
    next_cell: u32,
    /// Ports from the original module (preserved for output).
    pub ports: Vec<Port>,
    /// Original assignments (consumed during lowering).
    pub assignments: Vec<Assignment>,
    /// Set of cell IDs that have been removed (dead).
    dead_cells: std::collections::HashSet<CellId>,
}

impl<'a> Netlist<'a> {
    /// Creates a netlist from an IR module, importing its signals and cells.
    pub fn from_module(module: &Module, types: &TypeDb, interner: &'a Interner) -> Self {
        let mut signals = Arena::new();
        for (_id, sig) in module.signals.iter() {
            signals.alloc(sig.clone());
        }
        let mut cells = Arena::new();
        for (_id, cell) in module.cells.iter() {
            cells.alloc(cell.clone());
        }

        Self {
            signals,
            cells,
            types: types.clone(),
            interner,
            next_tmp: 0,
            next_cell: module.cells.len() as u32,
            ports: module.ports.clone(),
            assignments: module.assignments.clone(),
            dead_cells: std::collections::HashSet::new(),
        }
    }

    /// Adds a new temporary signal to the netlist and returns its ID.
    pub fn add_signal(&mut self, name: &str, ty: TypeId, kind: SignalKind) -> SignalId {
        let full_name = format!("_synth_{name}_{}", self.next_tmp);
        self.next_tmp += 1;
        let ident = self.interner.get_or_intern(&full_name);
        self.signals.alloc(Signal {
            id: SignalId::from_raw(self.signals.len() as u32),
            name: ident,
            ty,
            kind,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        })
    }

    /// Adds a new cell to the netlist and returns its ID.
    pub fn add_cell(&mut self, name: &str, kind: CellKind, connections: Vec<Connection>) -> CellId {
        let full_name = format!("_synth_{name}_{}", self.next_cell);
        self.next_cell += 1;
        let ident = self.interner.get_or_intern(&full_name);
        let id = CellId::from_raw(self.cells.len() as u32);
        self.cells.alloc(Cell {
            id,
            name: ident,
            kind,
            connections,
            span: Span::DUMMY,
        })
    }

    /// Marks a cell as dead (for dead code elimination).
    pub fn remove_cell(&mut self, id: CellId) {
        self.dead_cells.insert(id);
    }

    /// Returns true if a cell has been removed.
    pub fn is_dead(&self, id: CellId) -> bool {
        self.dead_cells.contains(&id)
    }

    /// Builds a fanout map: signal → list of cells that read it as input.
    pub fn fanout_map(&self) -> HashMap<SignalId, Vec<CellId>> {
        let mut map: HashMap<SignalId, Vec<CellId>> = HashMap::new();
        for (cell_id, cell) in self.cells.iter() {
            if self.is_dead(cell_id) {
                continue;
            }
            for conn in &cell.connections {
                if conn.direction == PortDirection::Input || conn.direction == PortDirection::InOut
                {
                    for sig_id in signal_ref_signals(&conn.signal) {
                        map.entry(sig_id).or_default().push(cell_id);
                    }
                }
            }
        }
        map
    }

    /// Builds a driver map: signal → cell that drives it (output connection).
    pub fn driver_map(&self) -> HashMap<SignalId, CellId> {
        let mut map = HashMap::new();
        for (cell_id, cell) in self.cells.iter() {
            if self.is_dead(cell_id) {
                continue;
            }
            for conn in &cell.connections {
                if conn.direction == PortDirection::Output {
                    for sig_id in signal_ref_signals(&conn.signal) {
                        map.insert(sig_id, cell_id);
                    }
                }
            }
        }
        map
    }

    /// Returns the number of live (non-dead) cells.
    #[cfg(test)]
    pub fn live_cell_count(&self) -> usize {
        self.cells.len() - self.dead_cells.len()
    }

    /// Creates an intern helper for port names used in connections.
    pub fn intern(&self, name: &str) -> Ident {
        self.interner.get_or_intern(name)
    }

    /// Creates an input connection.
    pub fn input_conn(&self, port_name: &str, signal: SignalRef) -> Connection {
        Connection {
            port_name: self.intern(port_name),
            direction: PortDirection::Input,
            signal,
        }
    }

    /// Creates an output connection.
    pub fn output_conn(&self, port_name: &str, signal: SignalRef) -> Connection {
        Connection {
            port_name: self.intern(port_name),
            direction: PortDirection::Output,
            signal,
        }
    }

    /// Gets the bit width of a signal from the type database.
    pub fn signal_width(&self, sig_id: SignalId) -> u32 {
        let sig = self.signals.get(sig_id);
        self.types.bit_width(sig.ty).unwrap_or(1)
    }
}

/// Extracts all `SignalId`s referenced by a `SignalRef`.
fn signal_ref_signals(sr: &SignalRef) -> Vec<SignalId> {
    match sr {
        SignalRef::Signal(id) => vec![*id],
        SignalRef::Slice { signal, .. } => vec![*signal],
        SignalRef::Concat(refs) => refs.iter().flat_map(signal_ref_signals).collect(),
        SignalRef::Const(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::LogicVec;
    use aion_ir::{Arena, Type};

    fn make_test_module(interner: &Interner, types: &mut TypeDb) -> Module {
        let bit_ty = types.intern(Type::Bit);
        let mut signals = Arena::new();
        let clk_name = interner.get_or_intern("clk");
        let clk_id = signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: clk_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let out_name = interner.get_or_intern("out");
        signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: out_name,
            ty: bit_ty,
            kind: SignalKind::Reg,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let mod_name = interner.get_or_intern("test_mod");
        Module {
            id: aion_ir::ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![Port {
                id: aion_ir::PortId::from_raw(0),
                name: clk_name,
                direction: PortDirection::Input,
                ty: bit_ty,
                signal: clk_id,
                span: Span::DUMMY,
            }],
            signals,
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![],
            clock_domains: vec![],
            content_hash: aion_common::ContentHash::from_bytes(b"test"),
        }
    }

    #[test]
    fn from_module_imports_signals() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let netlist = Netlist::from_module(&module, &types, &interner);
        assert_eq!(netlist.signals.len(), 2);
        assert_eq!(netlist.cells.len(), 0);
    }

    #[test]
    fn add_signal_creates_unique() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let s1 = netlist.add_signal("tmp", bit_ty, SignalKind::Wire);
        let s2 = netlist.add_signal("tmp", bit_ty, SignalKind::Wire);
        assert_ne!(s1, s2);
        assert_eq!(netlist.signals.len(), 4);
    }

    #[test]
    fn add_cell_and_query() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let c = netlist.add_cell("and", CellKind::And { width: 1 }, vec![]);
        assert_eq!(netlist.cells.len(), 1);
        assert!(!netlist.is_dead(c));
    }

    #[test]
    fn remove_cell_marks_dead() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let c = netlist.add_cell("x", CellKind::Not { width: 1 }, vec![]);
        assert_eq!(netlist.live_cell_count(), 1);
        netlist.remove_cell(c);
        assert!(netlist.is_dead(c));
        assert_eq!(netlist.live_cell_count(), 0);
    }

    #[test]
    fn fanout_map_tracks_inputs() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sig_a = SignalId::from_raw(0);
        let sig_b = SignalId::from_raw(1);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        let c = netlist.add_cell(
            "and",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(sig_a)),
                netlist.input_conn("B", SignalRef::Signal(sig_b)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );
        let fanout = netlist.fanout_map();
        assert!(fanout.get(&sig_a).unwrap().contains(&c));
        assert!(fanout.get(&sig_b).unwrap().contains(&c));
        assert!(!fanout.contains_key(&out));
    }

    #[test]
    fn driver_map_tracks_outputs() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sig_a = SignalId::from_raw(0);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        let c = netlist.add_cell(
            "not",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(sig_a)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );
        let drivers = netlist.driver_map();
        assert_eq!(drivers.get(&out), Some(&c));
    }

    #[test]
    fn dead_cells_excluded_from_maps() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sig_a = SignalId::from_raw(0);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        let c = netlist.add_cell(
            "not",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(sig_a)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );
        netlist.remove_cell(c);
        let fanout = netlist.fanout_map();
        assert!(!fanout.contains_key(&sig_a));
        let drivers = netlist.driver_map();
        assert!(!drivers.contains_key(&out));
    }

    #[test]
    fn signal_width_queries() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let vec8_ty = netlist.types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let s = netlist.add_signal("bus", vec8_ty, SignalKind::Wire);
        assert_eq!(netlist.signal_width(s), 8);
        assert_eq!(netlist.signal_width(SignalId::from_raw(0)), 1);
    }

    #[test]
    fn input_output_conn_helpers() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let netlist = Netlist::from_module(&module, &types, &interner);
        let conn_in = netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0)));
        assert_eq!(conn_in.direction, PortDirection::Input);
        let conn_out = netlist.output_conn("Y", SignalRef::Signal(SignalId::from_raw(1)));
        assert_eq!(conn_out.direction, PortDirection::Output);
    }

    #[test]
    fn from_module_preserves_ports() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let netlist = Netlist::from_module(&module, &types, &interner);
        assert_eq!(netlist.ports.len(), 1);
    }

    #[test]
    fn fanout_map_with_slice_ref() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sig_a = SignalId::from_raw(0);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        let c = netlist.add_cell(
            "slice",
            CellKind::Slice {
                offset: 0,
                width: 1,
            },
            vec![
                netlist.input_conn(
                    "A",
                    SignalRef::Slice {
                        signal: sig_a,
                        high: 0,
                        low: 0,
                    },
                ),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );
        let fanout = netlist.fanout_map();
        assert!(fanout.get(&sig_a).unwrap().contains(&c));
    }

    #[test]
    fn empty_module_creates_empty_netlist() {
        let interner = Interner::new();
        let types = TypeDb::new();
        let mod_name = interner.get_or_intern("empty");
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
        let netlist = Netlist::from_module(&module, &types, &interner);
        assert_eq!(netlist.signals.len(), 0);
        assert_eq!(netlist.cells.len(), 0);
        assert_eq!(netlist.live_cell_count(), 0);
    }

    #[test]
    fn fanout_excludes_const_refs() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "const_user",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Const(LogicVec::from_bool(true))),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );
        let fanout = netlist.fanout_map();
        // No signal should show up (the input is a const, not a signal)
        assert_eq!(fanout.len(), 0);
    }

    #[test]
    fn multiple_cells_same_input() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let module = make_test_module(&interner, &mut types);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sig_a = SignalId::from_raw(0);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out1 = netlist.add_signal("o1", bit_ty, SignalKind::Wire);
        let out2 = netlist.add_signal("o2", bit_ty, SignalKind::Wire);
        let c1 = netlist.add_cell(
            "not1",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(sig_a)),
                netlist.output_conn("Y", SignalRef::Signal(out1)),
            ],
        );
        let c2 = netlist.add_cell(
            "not2",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(sig_a)),
                netlist.output_conn("Y", SignalRef::Signal(out2)),
            ],
        );
        let fanout = netlist.fanout_map();
        let readers = fanout.get(&sig_a).unwrap();
        assert!(readers.contains(&c1));
        assert!(readers.contains(&c2));
        assert_eq!(readers.len(), 2);
    }
}
