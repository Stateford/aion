//! Conversion from synthesized [`MappedDesign`] to [`PnrNetlist`].
//!
//! Flattens the hierarchical module structure into a flat netlist of PnR cells,
//! pins, and nets. Top-level ports become I/O buffer cells with fixed placement.
//! Signal connectivity is traced through cell port connections to build nets.

use crate::data::{PnrCell, PnrCellType, PnrNet, PnrNetlist, PnrPin};
use crate::ids::{PnrCellId, PnrNetId, PnrPinId};
use aion_common::{Interner, LogicVec};
use aion_ir::{CellKind, PortDirection, SignalRef};
use aion_synth::{MappedDesign, MappedModule};
use std::collections::HashMap;

/// Converts a [`MappedDesign`] into a flat [`PnrNetlist`].
///
/// Creates PnR cells for every IR cell in the top module, creates I/O buffer
/// cells for top-level ports, and builds nets from signal connectivity.
pub fn convert_to_pnr(design: &MappedDesign, interner: &Interner) -> PnrNetlist {
    let mut netlist = PnrNetlist::new();
    let top = design.modules.get(design.top);

    // Track signal→net mapping for net construction
    let mut signal_nets: HashMap<u32, PnrNetId> = HashMap::new();
    // Track signal→driver pin for building nets
    let mut signal_drivers: HashMap<u32, PnrPinId> = HashMap::new();
    // Track signal→sink pins for building nets
    let mut signal_sinks: HashMap<u32, Vec<PnrPinId>> = HashMap::new();

    // 1. Create I/O buffer cells for top-level ports
    create_io_cells(
        top,
        interner,
        &mut netlist,
        &mut signal_drivers,
        &mut signal_sinks,
    );

    // 2. Create PnR cells for each IR cell in the top module
    create_logic_cells(
        top,
        interner,
        &mut netlist,
        &mut signal_drivers,
        &mut signal_sinks,
    );

    // 3. Build nets from signal connectivity
    build_nets(
        top,
        interner,
        &mut netlist,
        &signal_drivers,
        &signal_sinks,
        &mut signal_nets,
    );

    netlist
}

/// Creates I/O buffer cells for top-level ports.
fn create_io_cells(
    module: &MappedModule,
    interner: &Interner,
    netlist: &mut PnrNetlist,
    signal_drivers: &mut HashMap<u32, PnrPinId>,
    signal_sinks: &mut HashMap<u32, Vec<PnrPinId>>,
) {
    for port in &module.ports {
        let port_name = interner.resolve(port.name).to_string();
        let cell_id = netlist.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: format!("io_{port_name}"),
            cell_type: PnrCellType::Iobuf {
                direction: port.direction,
                standard: "LVCMOS33".into(),
            },
            placement: None,
            is_fixed: true,
        });

        // I/O buffers have a single pin
        let pin_dir = match port.direction {
            PortDirection::Input => PortDirection::Output, // IO drives internal net
            PortDirection::Output => PortDirection::Input, // IO receives from internal net
            PortDirection::InOut => PortDirection::InOut,
        };

        let pin_id = netlist.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: port_name,
            direction: pin_dir,
            cell: cell_id,
            net: None,
        });

        let sig_raw = port.signal.as_raw();
        match port.direction {
            PortDirection::Input => {
                signal_drivers.insert(sig_raw, pin_id);
            }
            PortDirection::Output => {
                signal_sinks.entry(sig_raw).or_default().push(pin_id);
            }
            PortDirection::InOut => {
                signal_drivers.insert(sig_raw, pin_id);
            }
        }
    }
}

/// Creates PnR cells for each IR cell in the module.
fn create_logic_cells(
    module: &MappedModule,
    interner: &Interner,
    netlist: &mut PnrNetlist,
    signal_drivers: &mut HashMap<u32, PnrPinId>,
    signal_sinks: &mut HashMap<u32, Vec<PnrPinId>>,
) {
    for (cell_id, cell) in module.cells.iter() {
        let cell_name = format!("cell_{}", cell_id.as_raw());
        let cell_type = ir_cell_to_pnr_type(&cell.kind);

        let pnr_cell_id = netlist.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: cell_name,
            cell_type,
            placement: None,
            is_fixed: false,
        });

        // Create pins for each connection on the IR cell
        for conn in &cell.connections {
            let pin_name = interner.resolve(conn.port_name).to_string();

            let pin_id = netlist.add_pin(PnrPin {
                id: PnrPinId::from_raw(0),
                name: pin_name,
                direction: conn.direction,
                cell: pnr_cell_id,
                net: None,
            });

            // Track connectivity based on signal reference
            if let SignalRef::Signal(sig_id) = &conn.signal {
                let sig_raw = sig_id.as_raw();
                match conn.direction {
                    PortDirection::Output => {
                        signal_drivers.insert(sig_raw, pin_id);
                    }
                    PortDirection::Input | PortDirection::InOut => {
                        signal_sinks.entry(sig_raw).or_default().push(pin_id);
                    }
                }
            }
        }
    }
}

/// Converts an IR [`CellKind`] to a [`PnrCellType`].
fn ir_cell_to_pnr_type(kind: &CellKind) -> PnrCellType {
    match kind {
        CellKind::And { .. } | CellKind::Or { .. } | CellKind::Xor { .. } => PnrCellType::Lut {
            inputs: 2,
            init: LogicVec::from_bool(false),
        },
        CellKind::Not { .. } => PnrCellType::Lut {
            inputs: 1,
            init: LogicVec::from_bool(false),
        },
        CellKind::Mux { .. } => PnrCellType::Lut {
            inputs: 3,
            init: LogicVec::from_bool(false),
        },
        CellKind::Dff { .. } => PnrCellType::Dff,
        CellKind::Eq { .. } | CellKind::Lt { .. } => PnrCellType::Lut {
            inputs: 2,
            init: LogicVec::from_bool(false),
        },
        CellKind::Add { .. } | CellKind::Sub { .. } => PnrCellType::Carry,
        _ => PnrCellType::Lut {
            inputs: 2,
            init: LogicVec::from_bool(false),
        },
    }
}

/// Builds nets from signal connectivity information.
fn build_nets(
    module: &MappedModule,
    interner: &Interner,
    netlist: &mut PnrNetlist,
    signal_drivers: &HashMap<u32, PnrPinId>,
    signal_sinks: &HashMap<u32, Vec<PnrPinId>>,
    signal_nets: &mut HashMap<u32, PnrNetId>,
) {
    for (sig_id, signal) in module.signals.iter() {
        let sig_raw = sig_id.as_raw();
        let driver = signal_drivers.get(&sig_raw);
        let sinks = signal_sinks.get(&sig_raw);

        // Only create nets for signals that have at least a driver or sinks
        if driver.is_none() && sinks.is_none() {
            continue;
        }

        let sig_name = interner.resolve(signal.name).to_string();

        // Use a dummy pin if no driver is found
        let driver_pin = match driver {
            Some(&pin) => pin,
            None => {
                // Create a dummy driver pin
                let dummy_cell = netlist.add_cell(PnrCell {
                    id: PnrCellId::from_raw(0),
                    name: format!("dummy_driver_{sig_name}"),
                    cell_type: PnrCellType::Lut {
                        inputs: 0,
                        init: LogicVec::from_bool(false),
                    },
                    placement: None,
                    is_fixed: false,
                });
                netlist.add_pin(PnrPin {
                    id: PnrPinId::from_raw(0),
                    name: "O".into(),
                    direction: PortDirection::Output,
                    cell: dummy_cell,
                    net: None,
                })
            }
        };

        let sink_pins = sinks.cloned().unwrap_or_default();

        let net_id = netlist.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: sig_name,
            driver: driver_pin,
            sinks: sink_pins,
            routing: None,
            timing_critical: false,
        });

        signal_nets.insert(sig_raw, net_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Interner};
    use aion_ir::{
        Arena, Cell, CellId, Connection, ModuleId, Port, PortDirection, Signal, SignalId,
        SignalKind, SignalRef, Type, TypeDb,
    };
    use aion_source::Span;
    use aion_synth::MappedDesign;

    fn make_simple_mapped_design() -> (MappedDesign, Interner) {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);

        let clk_name = interner.get_or_intern("clk");
        let in_name = interner.get_or_intern("data_in");
        let out_name = interner.get_or_intern("data_out");
        let d_name = interner.get_or_intern("D");
        let clk_pin = interner.get_or_intern("CLK");
        let q_name = interner.get_or_intern("Q");
        let dff_name = interner.get_or_intern("dff_0");

        let mut signals = Arena::new();
        let clk_id = signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: clk_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let in_id = signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: in_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        let out_id = signals.alloc(Signal {
            id: SignalId::from_raw(2),
            name: out_name,
            ty: bit_ty,
            kind: SignalKind::Reg,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let ports = vec![
            Port {
                id: aion_ir::PortId::from_raw(0),
                name: clk_name,
                direction: PortDirection::Input,
                ty: bit_ty,
                signal: clk_id,
                span: Span::DUMMY,
            },
            Port {
                id: aion_ir::PortId::from_raw(1),
                name: in_name,
                direction: PortDirection::Input,
                ty: bit_ty,
                signal: in_id,
                span: Span::DUMMY,
            },
            Port {
                id: aion_ir::PortId::from_raw(2),
                name: out_name,
                direction: PortDirection::Output,
                ty: bit_ty,
                signal: out_id,
                span: Span::DUMMY,
            },
        ];

        // Create a DFF cell
        let mut cells: Arena<CellId, Cell> = Arena::new();
        cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: dff_name,
            kind: CellKind::Dff {
                width: 1,
                has_reset: false,
                has_enable: false,
            },
            connections: vec![
                Connection {
                    port_name: d_name,
                    direction: PortDirection::Input,
                    signal: SignalRef::Signal(in_id),
                },
                Connection {
                    port_name: clk_pin,
                    direction: PortDirection::Input,
                    signal: SignalRef::Signal(clk_id),
                },
                Connection {
                    port_name: q_name,
                    direction: PortDirection::Output,
                    signal: SignalRef::Signal(out_id),
                },
            ],
            span: Span::DUMMY,
        });

        let mut modules: Arena<ModuleId, MappedModule> = Arena::new();
        modules.alloc(MappedModule {
            id: ModuleId::from_raw(0),
            name: interner.get_or_intern("top"),
            ports,
            signals,
            cells,
            resource_usage: aion_arch::ResourceUsage::default(),
            content_hash: ContentHash::from_bytes(b"test"),
            span: Span::DUMMY,
        });

        let design = MappedDesign {
            modules,
            top: ModuleId::from_raw(0),
            types,
            resource_usage: aion_arch::ResourceUsage::default(),
        };

        (design, interner)
    }

    #[test]
    fn convert_simple_design() {
        let (design, interner) = make_simple_mapped_design();
        let nl = convert_to_pnr(&design, &interner);

        // 3 IO cells + 1 DFF cell
        assert_eq!(nl.cell_count(), 4);
        assert!(nl.net_count() > 0);
        assert!(nl.pin_count() > 0);
    }

    #[test]
    fn io_cells_are_fixed() {
        let (design, interner) = make_simple_mapped_design();
        let nl = convert_to_pnr(&design, &interner);

        let io_cells: Vec<_> = nl.cells.iter().filter(|c| c.is_fixed).collect();
        assert_eq!(io_cells.len(), 3); // clk, data_in, data_out
    }

    #[test]
    fn logic_cells_not_fixed() {
        let (design, interner) = make_simple_mapped_design();
        let nl = convert_to_pnr(&design, &interner);

        let logic_cells: Vec<_> = nl.cells.iter().filter(|c| !c.is_fixed).collect();
        assert!(!logic_cells.is_empty());
        assert!(logic_cells.iter().all(|c| !c.is_fixed));
    }

    #[test]
    fn nets_have_drivers() {
        let (design, interner) = make_simple_mapped_design();
        let nl = convert_to_pnr(&design, &interner);

        for net in &nl.nets {
            // Every net should have a valid driver pin
            let driver_pin = nl.pin(net.driver);
            assert!(
                driver_pin.direction == PortDirection::Output
                    || driver_pin.direction == PortDirection::InOut
            );
        }
    }

    #[test]
    fn convert_empty_module() {
        let interner = Interner::new();
        let types = TypeDb::new();
        let mod_name = interner.get_or_intern("empty");

        let mut modules: Arena<ModuleId, MappedModule> = Arena::new();
        modules.alloc(MappedModule {
            id: ModuleId::from_raw(0),
            name: mod_name,
            ports: vec![],
            signals: Arena::new(),
            cells: Arena::new(),
            resource_usage: aion_arch::ResourceUsage::default(),
            content_hash: ContentHash::from_bytes(b"empty"),
            span: Span::DUMMY,
        });

        let design = MappedDesign {
            modules,
            top: ModuleId::from_raw(0),
            types,
            resource_usage: aion_arch::ResourceUsage::default(),
        };

        let nl = convert_to_pnr(&design, &interner);
        assert_eq!(nl.cell_count(), 0);
        assert_eq!(nl.net_count(), 0);
    }

    #[test]
    fn ir_cell_to_pnr_type_dff() {
        let t = ir_cell_to_pnr_type(&CellKind::Dff {
            width: 1,
            has_reset: false,
            has_enable: false,
        });
        assert!(matches!(t, PnrCellType::Dff));
    }

    #[test]
    fn ir_cell_to_pnr_type_and() {
        let t = ir_cell_to_pnr_type(&CellKind::And { width: 1 });
        assert!(matches!(t, PnrCellType::Lut { inputs: 2, .. }));
    }

    #[test]
    fn ir_cell_to_pnr_type_add() {
        let t = ir_cell_to_pnr_type(&CellKind::Add { width: 8 });
        assert!(matches!(t, PnrCellType::Carry));
    }

    #[test]
    fn ir_cell_to_pnr_type_not() {
        let t = ir_cell_to_pnr_type(&CellKind::Not { width: 1 });
        assert!(matches!(t, PnrCellType::Lut { inputs: 1, .. }));
    }

    #[test]
    fn cell_names_unique() {
        let (design, interner) = make_simple_mapped_design();
        let nl = convert_to_pnr(&design, &interner);

        let names: Vec<&str> = nl.cells.iter().map(|c| c.name.as_str()).collect();
        let unique: std::collections::HashSet<&&str> = names.iter().collect();
        assert_eq!(names.len(), unique.len(), "cell names should be unique");
    }
}
