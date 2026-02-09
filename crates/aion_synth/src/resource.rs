//! Resource usage counting for synthesized netlists.
//!
//! Walks the cells in a netlist and tallies LUTs, FFs, BRAMs, DSPs, and I/Os
//! to produce a [`ResourceUsage`] summary.

use crate::netlist::Netlist;
use aion_arch::ResourceUsage;
use aion_ir::{CellKind, PortDirection};

/// Counts resource usage from the cells in a netlist.
///
/// Examines each live cell and categorizes it as a LUT, FF, BRAM, DSP, or I/O
/// based on its [`CellKind`].
pub(crate) fn count_resources(netlist: &Netlist) -> ResourceUsage {
    let mut usage = ResourceUsage::default();

    for (cell_id, cell) in netlist.cells.iter() {
        if netlist.is_dead(cell_id) {
            continue;
        }

        match &cell.kind {
            CellKind::Lut { .. } => {
                // Each LUT counts as one LUT resource
                usage.luts += 1;
            }

            CellKind::Dff { width, .. } => {
                // Each DFF bit counts as one FF resource
                usage.ffs += *width;
            }

            CellKind::Latch { width } => {
                // Latches consume FF resources (implemented as FFs in most architectures)
                usage.ffs += *width;
            }

            CellKind::Bram(_) => {
                usage.bram += 1;
            }

            CellKind::Dsp(_) => {
                usage.dsp += 1;
            }

            CellKind::Iobuf(_) => {
                usage.io += 1;
            }

            CellKind::Pll(_) => {
                usage.pll += 1;
            }

            // Generic gates that haven't been tech-mapped yet
            // count as LUTs (approximate)
            CellKind::And { width }
            | CellKind::Or { width }
            | CellKind::Xor { width }
            | CellKind::Not { width } => {
                usage.luts += *width;
            }

            CellKind::Mux { width, .. } => {
                usage.luts += *width;
            }

            CellKind::Add { width }
            | CellKind::Sub { width }
            | CellKind::Shl { width }
            | CellKind::Shr { width } => {
                // Arithmetic uses roughly width LUTs
                usage.luts += *width;
            }

            CellKind::Mul { width } => {
                // Unmapped multiplier — very rough estimate
                usage.luts += width * width;
            }

            CellKind::Eq { width } | CellKind::Lt { width } => {
                usage.luts += *width;
            }

            CellKind::Carry { width } => {
                usage.luts += *width;
            }

            CellKind::Memory { depth, width, .. } => {
                // Unmapped memory — estimate as LUT-RAM
                usage.luts += depth * width / 16;
            }

            // These don't consume physical resources
            CellKind::Const { .. }
            | CellKind::Concat
            | CellKind::Slice { .. }
            | CellKind::Repeat { .. }
            | CellKind::Instance { .. }
            | CellKind::BlackBox { .. } => {}
        }
    }

    // Count I/O from ports
    for port in &netlist.ports {
        if port.direction == PortDirection::Input
            || port.direction == PortDirection::Output
            || port.direction == PortDirection::InOut
        {
            usage.io += 1;
        }
    }

    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use aion_common::{Interner, LogicVec};
    use aion_ir::{
        Arena, BramConfig, CellKind, DspConfig, Module, Port, PortDirection, Signal, SignalId,
        SignalKind, SignalRef, Type, TypeDb,
    };
    use aion_source::Span;

    fn make_netlist(interner: &Interner) -> Netlist<'_> {
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
            content_hash: aion_common::ContentHash::from_bytes(b"test"),
        };
        Netlist::from_module(&module, &types, interner)
    }

    fn make_netlist_with_ports(interner: &Interner) -> Netlist<'_> {
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
    fn count_luts() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "lut",
            CellKind::Lut {
                width: 4,
                init: LogicVec::from_u64(0x8, 16),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(out))],
        );

        let usage = count_resources(&netlist);
        assert_eq!(usage.luts, 1);
        assert_eq!(usage.ffs, 0);
    }

    #[test]
    fn count_ffs() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "dff",
            CellKind::Dff {
                width: 8,
                has_reset: true,
                has_enable: false,
            },
            vec![netlist.output_conn("Q", SignalRef::Signal(out))],
        );

        let usage = count_resources(&netlist);
        assert_eq!(usage.ffs, 8);
    }

    #[test]
    fn count_bram() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "bram",
            CellKind::Bram(BramConfig {
                depth: 1024,
                width: 8,
            }),
            vec![netlist.output_conn("Y", SignalRef::Signal(out))],
        );

        let usage = count_resources(&netlist);
        assert_eq!(usage.bram, 1);
    }

    #[test]
    fn count_dsp() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "dsp",
            CellKind::Dsp(DspConfig {
                width_a: 18,
                width_b: 18,
            }),
            vec![netlist.output_conn("Y", SignalRef::Signal(out))],
        );

        let usage = count_resources(&netlist);
        assert_eq!(usage.dsp, 1);
    }

    #[test]
    fn count_io_from_ports() {
        let interner = Interner::new();
        let netlist = make_netlist_with_ports(&interner);
        let usage = count_resources(&netlist);
        assert_eq!(usage.io, 2); // 1 input + 1 output
    }

    #[test]
    fn count_dead_cells_excluded() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out = netlist.add_signal("out", bit_ty, SignalKind::Wire);
        let cell_id = netlist.add_cell(
            "lut",
            CellKind::Lut {
                width: 4,
                init: LogicVec::from_u64(0x8, 16),
            },
            vec![netlist.output_conn("Y", SignalRef::Signal(out))],
        );
        netlist.remove_cell(cell_id);

        let usage = count_resources(&netlist);
        assert_eq!(usage.luts, 0);
    }

    #[test]
    fn count_generic_gates() {
        let interner = Interner::new();
        let mut netlist = make_netlist(&interner);
        let bit_ty = netlist.types.intern(Type::Bit);
        let out1 = netlist.add_signal("o1", bit_ty, SignalKind::Wire);
        let out2 = netlist.add_signal("o2", bit_ty, SignalKind::Wire);
        netlist.add_cell(
            "and",
            CellKind::And { width: 1 },
            vec![netlist.output_conn("Y", SignalRef::Signal(out1))],
        );
        netlist.add_cell(
            "or",
            CellKind::Or { width: 4 },
            vec![netlist.output_conn("Y", SignalRef::Signal(out2))],
        );

        let usage = count_resources(&netlist);
        assert_eq!(usage.luts, 5); // 1 + 4
    }

    #[test]
    fn count_empty() {
        let interner = Interner::new();
        let netlist = make_netlist(&interner);
        let usage = count_resources(&netlist);
        assert_eq!(usage.luts, 0);
        assert_eq!(usage.ffs, 0);
        assert_eq!(usage.bram, 0);
        assert_eq!(usage.dsp, 0);
        assert_eq!(usage.io, 0);
        assert_eq!(usage.pll, 0);
    }

    #[test]
    fn count_const_no_resources() {
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

        let usage = count_resources(&netlist);
        assert_eq!(usage.luts, 0);
    }
}
