//! Cell definitions for primitives and module instantiations.
//!
//! A [`Cell`] represents either a primitive operation (gate, LUT, DFF) or
//! an instantiation of another module. Cells are the structural building
//! blocks of the netlist after synthesis.

use crate::ids::{CellId, ModuleId};
use crate::port::PortDirection;
use crate::signal::SignalRef;
use aion_common::{Ident, LogicVec};
use aion_source::Span;
use serde::{Deserialize, Serialize};

use crate::const_value::ConstValue;

/// Configuration for a block RAM primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BramConfig {
    /// Memory depth (number of words).
    pub depth: u32,
    /// Word width in bits.
    pub width: u32,
}

/// Configuration for a DSP block primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DspConfig {
    /// Width of the A input operand.
    pub width_a: u32,
    /// Width of the B input operand.
    pub width_b: u32,
}

/// Configuration for a PLL/clock management primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PllConfig {
    /// Input frequency in Hz.
    pub input_freq: u32,
    /// Output frequency in Hz.
    pub output_freq: u32,
}

/// Configuration for an I/O buffer primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IobufConfig {
    /// The I/O standard (e.g., LVCMOS33, LVTTL).
    pub standard: Ident,
}

/// The kind of a cell, distinguishing primitives from instantiations.
///
/// Pre-synthesis cells include behavioral primitives like `And`, `Or`, `Dff`.
/// Post-synthesis cells include technology-mapped primitives like `Lut`, `Bram`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CellKind {
    /// Instantiation of another module.
    Instance {
        /// The module being instantiated.
        module: ModuleId,
        /// Resolved parameter values.
        params: Vec<(Ident, ConstValue)>,
    },

    // --- Combinational primitives ---
    /// Bitwise AND gate.
    And {
        /// Operand width in bits.
        width: u32,
    },
    /// Bitwise OR gate.
    Or {
        /// Operand width in bits.
        width: u32,
    },
    /// Bitwise XOR gate.
    Xor {
        /// Operand width in bits.
        width: u32,
    },
    /// Bitwise NOT gate.
    Not {
        /// Operand width in bits.
        width: u32,
    },
    /// Multiplexer.
    Mux {
        /// Data width in bits.
        width: u32,
        /// Select signal width in bits.
        select_width: u32,
    },
    /// Adder.
    Add {
        /// Operand width in bits.
        width: u32,
    },
    /// Subtractor.
    Sub {
        /// Operand width in bits.
        width: u32,
    },
    /// Multiplier.
    Mul {
        /// Operand width in bits.
        width: u32,
    },
    /// Left shift.
    Shl {
        /// Operand width in bits.
        width: u32,
    },
    /// Right shift.
    Shr {
        /// Operand width in bits.
        width: u32,
    },
    /// Equality comparator.
    Eq {
        /// Operand width in bits.
        width: u32,
    },
    /// Less-than comparator.
    Lt {
        /// Operand width in bits.
        width: u32,
    },
    /// Bit concatenation.
    Concat,
    /// Bit slice extraction.
    Slice {
        /// Starting bit offset.
        offset: u32,
        /// Width of the slice in bits.
        width: u32,
    },
    /// Bit repetition.
    Repeat {
        /// Number of repetitions.
        count: u32,
    },
    /// Constant value source.
    Const {
        /// The constant value.
        value: LogicVec,
    },

    // --- Sequential primitives ---
    /// D flip-flop.
    Dff {
        /// Data width in bits.
        width: u32,
        /// Whether the DFF has a reset input.
        has_reset: bool,
        /// Whether the DFF has a clock enable input.
        has_enable: bool,
    },
    /// Level-sensitive latch.
    Latch {
        /// Data width in bits.
        width: u32,
    },

    // --- Memory primitives ---
    /// Memory block (pre-tech-mapping).
    Memory {
        /// Memory depth (number of words).
        depth: u32,
        /// Word width in bits.
        width: u32,
        /// Number of read ports.
        read_ports: u32,
        /// Number of write ports.
        write_ports: u32,
    },

    // --- Technology-mapped primitives ---
    /// Look-up table (post-tech-mapping).
    Lut {
        /// Number of inputs.
        width: u32,
        /// LUT initialization pattern.
        init: LogicVec,
    },
    /// Carry chain element.
    Carry {
        /// Chain width in bits.
        width: u32,
    },
    /// Block RAM (post-tech-mapping).
    Bram(BramConfig),
    /// DSP block (post-tech-mapping).
    Dsp(DspConfig),
    /// PLL/clock management (post-tech-mapping).
    Pll(PllConfig),
    /// I/O buffer (post-tech-mapping).
    Iobuf(IobufConfig),

    /// Black box (unresolved or errored module).
    BlackBox {
        /// The port names of the black box.
        port_names: Vec<Ident>,
    },
}

/// A connection between a cell port and a signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    /// The name of the port on the cell.
    pub port_name: Ident,
    /// The direction of data flow.
    pub direction: PortDirection,
    /// The signal or signal slice connected to this port.
    pub signal: SignalRef,
}

/// A cell in the netlist — either a primitive operation or a module instantiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cell {
    /// The unique ID of this cell within its module.
    pub id: CellId,
    /// The cell instance name.
    pub name: Ident,
    /// The kind of cell (primitive type or module instance).
    pub kind: CellKind,
    /// The port-to-signal connections.
    pub connections: Vec<Connection>,
    /// The source span where this cell was instantiated.
    pub span: Span,
}

impl Cell {
    /// Returns the [`TypeId`] if this is a `Const` cell kind, else `None`.
    ///
    /// This is a convenience for pattern matching on the cell kind.
    pub fn module_id(&self) -> Option<ModuleId> {
        match &self.kind {
            CellKind::Instance { module, .. } => Some(*module),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SignalId;

    fn dummy_cell(kind: CellKind) -> Cell {
        Cell {
            id: CellId::from_raw(0),
            name: Ident::from_raw(1),
            kind,
            connections: Vec::new(),
            span: Span::DUMMY,
        }
    }

    #[test]
    fn instance_cell() {
        let cell = dummy_cell(CellKind::Instance {
            module: ModuleId::from_raw(5),
            params: vec![],
        });
        assert_eq!(cell.module_id(), Some(ModuleId::from_raw(5)));
    }

    #[test]
    fn primitive_cell() {
        let cell = dummy_cell(CellKind::And { width: 8 });
        assert_eq!(cell.module_id(), None);
    }

    #[test]
    fn dff_cell() {
        let cell = dummy_cell(CellKind::Dff {
            width: 1,
            has_reset: true,
            has_enable: false,
        });
        if let CellKind::Dff {
            has_reset,
            has_enable,
            ..
        } = &cell.kind
        {
            assert!(*has_reset);
            assert!(!*has_enable);
        } else {
            panic!("expected Dff");
        }
    }

    #[test]
    fn cell_with_connections() {
        let cell = Cell {
            id: CellId::from_raw(0),
            name: Ident::from_raw(1),
            kind: CellKind::And { width: 1 },
            connections: vec![
                Connection {
                    port_name: Ident::from_raw(2),
                    direction: PortDirection::Input,
                    signal: SignalRef::Signal(SignalId::from_raw(0)),
                },
                Connection {
                    port_name: Ident::from_raw(3),
                    direction: PortDirection::Output,
                    signal: SignalRef::Signal(SignalId::from_raw(1)),
                },
            ],
            span: Span::DUMMY,
        };
        assert_eq!(cell.connections.len(), 2);
    }

    #[test]
    fn lut_cell() {
        let init = LogicVec::all_zero(16);
        let cell = dummy_cell(CellKind::Lut {
            width: 4,
            init: init.clone(),
        });
        if let CellKind::Lut { width, init: i } = &cell.kind {
            assert_eq!(*width, 4);
            assert_eq!(i.width(), 16);
        } else {
            panic!("expected Lut");
        }
    }

    #[test]
    fn memory_cell() {
        let cell = dummy_cell(CellKind::Memory {
            depth: 1024,
            width: 32,
            read_ports: 1,
            write_ports: 1,
        });
        if let CellKind::Memory {
            depth,
            width,
            read_ports,
            write_ports,
        } = &cell.kind
        {
            assert_eq!(*depth, 1024);
            assert_eq!(*width, 32);
            assert_eq!(*read_ports, 1);
            assert_eq!(*write_ports, 1);
        } else {
            panic!("expected Memory");
        }
    }

    #[test]
    fn black_box_cell() {
        let cell = dummy_cell(CellKind::BlackBox {
            port_names: vec![Ident::from_raw(10), Ident::from_raw(11)],
        });
        if let CellKind::BlackBox { port_names } = &cell.kind {
            assert_eq!(port_names.len(), 2);
        } else {
            panic!("expected BlackBox");
        }
    }
}
