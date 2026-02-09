//! Technology mapping: maps generic cells to device-specific primitives.
//!
//! Uses the [`TechMapper`] from [`aion_arch`] to convert generic gate cells
//! (AND, OR, MUX, etc.) into LUTs, and to infer BRAMs and DSPs from
//! memory and multiplier patterns.

use crate::netlist::Netlist;
use aion_arch::{ArithmeticPattern, ArithmeticPatternKind, MapResult, MemoryCell, TechMapper};
use aion_common::LogicVec;
use aion_diagnostics::DiagnosticSink;
use aion_ir::{CellId, CellKind, PortDirection, SignalKind, SignalRef, Type};

/// Runs technology mapping on the netlist using the given mapper.
///
/// Iterates all cells and replaces generic primitives with device-specific
/// cells (LUTs, BRAMs, DSPs) based on the architecture's tech mapper.
pub(crate) fn tech_map(netlist: &mut Netlist, mapper: &dyn TechMapper, sink: &DiagnosticSink) {
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
        let kind = cell.kind.clone();

        match &kind {
            // Memory inference
            CellKind::Memory {
                depth,
                width,
                read_ports,
                write_ports,
            } => {
                let mem = MemoryCell {
                    depth: *depth,
                    width: *width,
                    read_ports: *read_ports,
                    write_ports: *write_ports,
                    has_registered_output: false,
                    clock_signal: None,
                };
                if mapper.infer_bram(&mem) {
                    let cell_mut = netlist.cells.get_mut(cell_id);
                    cell_mut.kind = CellKind::Bram(aion_ir::BramConfig {
                        depth: *depth,
                        width: *width,
                    });
                }
                // If BRAM inference fails, keep as generic Memory
                // (will be decomposed to LUT-RAM in a future pass)
            }

            // Multiplier → DSP inference
            CellKind::Mul { width } => {
                let pattern = ArithmeticPattern {
                    kind: ArithmeticPatternKind::Multiply,
                    width_a: *width,
                    width_b: *width,
                    has_pipeline_regs: false,
                    has_accumulator: false,
                };
                if mapper.infer_dsp(&pattern) {
                    let cell_mut = netlist.cells.get_mut(cell_id);
                    cell_mut.kind = CellKind::Dsp(aion_ir::DspConfig {
                        width_a: *width,
                        width_b: *width,
                    });
                } else {
                    // Falls through to generic mapping below
                    map_to_luts_if_needed(netlist, cell_id, &kind, mapper);
                }
            }

            // DFF / Latch — pass through (already technology-independent primitives)
            CellKind::Dff { .. } | CellKind::Latch { .. } => {
                // DFFs and latches are kept as-is (MapResult::Ff)
            }

            // Already tech-mapped primitives — skip
            CellKind::Lut { .. }
            | CellKind::Carry { .. }
            | CellKind::Bram(_)
            | CellKind::Dsp(_)
            | CellKind::Pll(_)
            | CellKind::Iobuf(_) => {}

            // Instance / BlackBox — skip
            CellKind::Instance { .. } | CellKind::BlackBox { .. } => {}

            // Const cells — skip (no hardware needed)
            CellKind::Const { .. } => {}

            // All other generic cells: map via TechMapper
            _ => {
                map_to_luts_if_needed(netlist, cell_id, &kind, mapper);
            }
        }
    }

    let _ = sink; // Available for future diagnostics
}

/// Attempts to map a generic cell to LUTs using the tech mapper.
fn map_to_luts_if_needed(
    netlist: &mut Netlist,
    cell_id: CellId,
    kind: &CellKind,
    mapper: &dyn TechMapper,
) {
    let result = mapper.map_cell(kind);
    match result {
        MapResult::Luts(mappings) => {
            if mappings.is_empty() {
                return;
            }

            // Get the original cell's connections
            let cell = netlist.cells.get(cell_id);
            let input_signals: Vec<SignalRef> = cell
                .connections
                .iter()
                .filter(|c| c.direction == PortDirection::Input)
                .map(|c| c.signal.clone())
                .collect();
            let _output_signal: Option<SignalRef> = cell
                .connections
                .iter()
                .find(|c| c.direction == PortDirection::Output)
                .map(|c| c.signal.clone());

            if mappings.len() == 1 {
                // Single LUT replacement — keep the same connections
                let m = &mappings[0];
                let init_lv = lut_init_to_logic_vec(&m.init_bits, m.input_count);
                let cell_mut = netlist.cells.get_mut(cell_id);
                cell_mut.kind = CellKind::Lut {
                    width: m.input_count,
                    init: init_lv,
                };
            } else {
                // Multiple LUTs — need per-bit decomposition
                // For multi-bit operations, create one LUT per bit
                // Mark original cell as dead
                netlist.remove_cell(cell_id);

                for (i, m) in mappings.iter().enumerate() {
                    let init_lv = lut_init_to_logic_vec(&m.init_bits, m.input_count);
                    let bit_ty = netlist.types.intern(Type::Bit);
                    let lut_out = netlist.add_signal(&format!("lut{i}"), bit_ty, SignalKind::Wire);

                    let mut conns = Vec::new();
                    // Connect LUT inputs (from input signals, one bit each)
                    for (j, input_ref) in input_signals.iter().enumerate() {
                        conns.push(netlist.input_conn(&format!("I{j}"), input_ref.clone()));
                    }
                    conns.push(netlist.output_conn("Y", SignalRef::Signal(lut_out)));

                    netlist.add_cell(
                        &format!("lut{i}"),
                        CellKind::Lut {
                            width: m.input_count,
                            init: init_lv,
                        },
                        conns,
                    );
                }
            }
        }
        MapResult::Ff | MapResult::PassThrough => {
            // Keep as-is
        }
        MapResult::Bram => {
            // Already handled above for Memory cells
        }
        MapResult::Dsp => {
            // Already handled above for Mul cells
        }
        MapResult::Unmappable => {
            // Leave cell as generic — P&R will handle it or error
        }
    }
}

/// Converts LUT init bits (Vec<u8>) to a LogicVec.
fn lut_init_to_logic_vec(init_bits: &[u8], input_count: u32) -> LogicVec {
    let num_bits = 1u32 << input_count;
    let mut lv = LogicVec::new(num_bits);
    for (i, &bit) in init_bits.iter().enumerate() {
        if i < num_bits as usize && bit != 0 {
            lv.set(i as u32, aion_common::Logic::One);
        }
    }
    lv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use aion_arch::{LutMapping, MapResult, MemoryCell};
    use aion_common::{Interner, LogicVec};
    use aion_ir::{Arena, CellKind, Module, Signal, SignalId, SignalKind, SignalRef, Type, TypeDb};
    use aion_source::Span;

    /// A simple mock tech mapper for testing.
    struct MockMapper {
        lut_k: u32,
        max_bram_depth: u32,
        max_dsp_width: u32,
    }

    impl MockMapper {
        fn new() -> Self {
            Self {
                lut_k: 4,
                max_bram_depth: 9216,
                max_dsp_width: 18,
            }
        }
    }

    impl TechMapper for MockMapper {
        fn map_cell(&self, cell_kind: &CellKind) -> MapResult {
            match cell_kind {
                CellKind::And { width } if *width == 1 => MapResult::Luts(vec![LutMapping {
                    input_count: 2,
                    init_bits: vec![0, 0, 0, 1], // AND truth table
                }]),
                CellKind::Or { width } if *width == 1 => MapResult::Luts(vec![LutMapping {
                    input_count: 2,
                    init_bits: vec![0, 1, 1, 1], // OR truth table
                }]),
                CellKind::Not { width } if *width == 1 => MapResult::Luts(vec![LutMapping {
                    input_count: 1,
                    init_bits: vec![1, 0], // NOT truth table
                }]),
                CellKind::Xor { width } if *width == 1 => MapResult::Luts(vec![LutMapping {
                    input_count: 2,
                    init_bits: vec![0, 1, 1, 0], // XOR truth table
                }]),
                CellKind::Dff { .. } => MapResult::Ff,
                CellKind::Mux { width, .. } if *width == 1 => MapResult::Luts(vec![LutMapping {
                    input_count: 3,
                    init_bits: vec![0, 0, 1, 1, 0, 1, 0, 1], // MUX truth table
                }]),
                _ => MapResult::Unmappable,
            }
        }

        fn infer_bram(&self, memory: &MemoryCell) -> bool {
            memory.depth <= self.max_bram_depth && memory.width <= 36
        }

        fn infer_dsp(&self, pattern: &aion_arch::ArithmeticPattern) -> bool {
            pattern.width_a <= self.max_dsp_width && pattern.width_b <= self.max_dsp_width
        }

        fn map_to_luts(&self, _cell_kind: &CellKind) -> Vec<LutMapping> {
            vec![]
        }

        fn lut_input_count(&self) -> u32 {
            self.lut_k
        }

        fn max_bram_depth(&self) -> u32 {
            self.max_bram_depth
        }

        fn max_bram_width(&self) -> u32 {
            36
        }

        fn max_dsp_width_a(&self) -> u32 {
            self.max_dsp_width
        }

        fn max_dsp_width_b(&self) -> u32 {
            self.max_dsp_width
        }
    }

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
    fn tech_map_and_to_lut() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "and",
            CellKind::And { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.input_conn("B", SignalRef::Signal(SignalId::from_raw(1))),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        let has_lut = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Lut { .. }));
        assert!(has_lut, "AND should be mapped to LUT");
    }

    #[test]
    fn tech_map_dff_passthrough() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Reg);
        netlist.add_cell(
            "dff",
            CellKind::Dff {
                width: 1,
                has_reset: false,
                has_enable: false,
            },
            vec![
                netlist.input_conn("D", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.output_conn("Q", SignalRef::Signal(out)),
            ],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        // DFF should remain as DFF
        let has_dff = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Dff { .. }));
        assert!(has_dff, "DFF should be preserved");
    }

    #[test]
    fn tech_map_memory_to_bram() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "mem",
            CellKind::Memory {
                depth: 1024,
                width: 8,
                read_ports: 1,
                write_ports: 1,
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(out))],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        let has_bram = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Bram(_)));
        assert!(has_bram, "Memory should be mapped to BRAM");
    }

    #[test]
    fn tech_map_mul_to_dsp() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = netlist.types.intern(Type::BitVec {
            width: 16,
            signed: false,
        });
        let a = netlist.add_signal("ma", ty, SignalKind::Wire);
        let b = netlist.add_signal("mb", ty, SignalKind::Wire);
        let out = netlist.add_signal("out", ty, SignalKind::Wire);
        netlist.add_cell(
            "mul",
            CellKind::Mul { width: 16 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        let has_dsp = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Dsp(_)));
        assert!(has_dsp, "16-bit Mul should be mapped to DSP");
    }

    #[test]
    fn tech_map_large_mul_stays_generic() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let ty = netlist.types.intern(Type::BitVec {
            width: 32,
            signed: false,
        });
        let a = netlist.add_signal("ma", ty, SignalKind::Wire);
        let b = netlist.add_signal("mb", ty, SignalKind::Wire);
        let out = netlist.add_signal("out", ty, SignalKind::Wire);
        netlist.add_cell(
            "mul",
            CellKind::Mul { width: 32 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(a)),
                netlist.input_conn("B", SignalRef::Signal(b)),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        // 32-bit mul exceeds 18-bit DSP — should remain unmappable
        let has_dsp = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Dsp(_)));
        assert!(!has_dsp, "32-bit Mul should NOT be mapped to DSP");
    }

    #[test]
    fn tech_map_not_to_lut() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "not",
            CellKind::Not { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        let has_lut = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Lut { .. }));
        assert!(has_lut, "NOT should be mapped to LUT");
    }

    #[test]
    fn tech_map_already_mapped_skipped() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "lut",
            CellKind::Lut {
                width: 2,
                init: LogicVec::from_u64(0x8, 4),
            },
            vec![
                netlist.input_conn("I0", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.input_conn("I1", SignalRef::Signal(SignalId::from_raw(1))),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        // LUT should remain as LUT
        let lut_count = netlist
            .cells
            .iter()
            .filter(|(id, c)| !netlist.is_dead(*id) && matches!(&c.kind, CellKind::Lut { .. }))
            .count();
        assert_eq!(lut_count, 1);
    }

    #[test]
    fn tech_map_instance_skipped() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        netlist.add_cell(
            "inst",
            CellKind::Instance {
                module: aion_ir::ModuleId::from_raw(1),
                params: vec![],
            },
            vec![],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        let has_inst = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Instance { .. }));
        assert!(has_inst, "Instance should be preserved");
    }

    #[test]
    fn tech_map_const_skipped() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "const",
            CellKind::Const {
                value: LogicVec::from_bool(true),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(out))],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        let has_const = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Const { .. }));
        assert!(has_const, "Const should be preserved");
    }

    #[test]
    fn lut_init_to_logic_vec_correct() {
        let lv = lut_init_to_logic_vec(&[0, 0, 0, 1], 2);
        assert_eq!(lv.width(), 4);
        assert_eq!(lv.to_u64(), Some(0b1000));
    }

    #[test]
    fn lut_init_empty() {
        let lv = lut_init_to_logic_vec(&[], 2);
        assert_eq!(lv.width(), 4);
        assert_eq!(lv.to_u64(), Some(0));
    }

    #[test]
    fn tech_map_empty_netlist() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);
        assert_eq!(netlist.live_cell_count(), 0);
    }

    #[test]
    fn tech_map_or_to_lut() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "or",
            CellKind::Or { width: 1 },
            vec![
                netlist.input_conn("A", SignalRef::Signal(SignalId::from_raw(0))),
                netlist.input_conn("B", SignalRef::Signal(SignalId::from_raw(1))),
                netlist.output_conn("Y", SignalRef::Signal(out)),
            ],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        let has_lut = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Lut { .. }));
        assert!(has_lut, "OR should be mapped to LUT");
    }

    #[test]
    fn tech_map_memory_too_large_stays() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "mem",
            CellKind::Memory {
                depth: 100_000, // Way too large for BRAM
                width: 64,
                read_ports: 1,
                write_ports: 1,
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(out))],
        );

        let mapper = MockMapper::new();
        let sink = DiagnosticSink::new();
        tech_map(&mut netlist, &mapper, &sink);

        let has_bram = netlist
            .cells
            .iter()
            .any(|(id, c)| !netlist.is_dead(id) && matches!(&c.kind, CellKind::Bram(_)));
        assert!(!has_bram, "Oversized memory should NOT become BRAM");
    }
}
