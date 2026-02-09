//! Technology mapping traits and pattern types.
//!
//! The [`TechMapper`] trait defines the interface for mapping generic IR cells
//! to device-specific primitives. Pattern types like [`MemoryCell`],
//! [`ArithmeticPattern`], and [`LogicCone`] describe higher-level structures
//! that can be inferred and mapped to dedicated hardware resources (BRAM, DSP).

use aion_ir::{CellId, CellKind, SignalId};
use serde::{Deserialize, Serialize};

/// A memory cell pattern recognized during inference.
///
/// Describes a memory structure that may be mappable to a hardware block RAM
/// primitive, depending on the device's BRAM capacity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCell {
    /// Memory depth in words.
    pub depth: u32,
    /// Word width in bits.
    pub width: u32,
    /// Number of read ports.
    pub read_ports: u32,
    /// Number of write ports.
    pub write_ports: u32,
    /// Whether the read output is registered (synchronous read).
    pub has_registered_output: bool,
    /// The clock signal driving this memory, if known.
    pub clock_signal: Option<SignalId>,
}

/// The kind of arithmetic operation detected in a DSP inference pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArithmeticPatternKind {
    /// A simple multiply operation (A * B).
    Multiply,
    /// A multiply-add operation (A * B + C).
    MultiplyAdd,
    /// A multiply-accumulate operation (ACC += A * B).
    MultiplyAccumulate,
}

/// An arithmetic pattern recognized during DSP inference.
///
/// Describes a multiply or multiply-accumulate structure that may be mappable
/// to a hardware DSP block, depending on the device's DSP capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArithmeticPattern {
    /// The kind of arithmetic operation.
    pub kind: ArithmeticPatternKind,
    /// Width of the A operand in bits.
    pub width_a: u32,
    /// Width of the B operand in bits.
    pub width_b: u32,
    /// Whether the pattern includes pipeline registers.
    pub has_pipeline_regs: bool,
    /// Whether the pattern includes an accumulator feedback path.
    pub has_accumulator: bool,
}

/// A logic cone representing a set of cells feeding a single output.
///
/// Used during LUT mapping to identify clusters of combinational logic
/// that can be packed into one or more LUTs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicCone {
    /// The output signal of this logic cone.
    pub output: SignalId,
    /// The cells that make up this logic cone, in topological order.
    pub cells: Vec<CellId>,
    /// The input signals feeding into this cone from outside.
    pub input_signals: Vec<SignalId>,
}

/// The result of mapping a single cell to device-specific primitives.
///
/// A single IR cell may expand to multiple device cells (e.g., a wide AND
/// gate decomposed into multiple LUTs) or may be absorbed into a larger
/// primitive (e.g., a multiply absorbed into a DSP block).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MapResult {
    /// The cell maps to one or more LUTs with the given truth tables.
    Luts(Vec<LutMapping>),
    /// The cell maps directly to a flip-flop.
    Ff,
    /// The cell maps to a block RAM primitive.
    Bram,
    /// The cell maps to a DSP block primitive.
    Dsp,
    /// The cell passes through unchanged (already technology-mapped).
    PassThrough,
    /// The cell cannot be mapped by this device's tech mapper.
    Unmappable,
}

/// A single LUT mapping result with its truth table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LutMapping {
    /// Number of inputs to this LUT.
    pub input_count: u32,
    /// The truth table initialization value as a bit vector.
    pub init_bits: Vec<u8>,
}

/// Technology mapping interface for a specific device family.
///
/// Implementations translate generic IR [`CellKind`] primitives into
/// device-specific resources. The trait provides methods for mapping
/// individual cells, inferring BRAM/DSP from patterns, and querying
/// device-specific limits.
pub trait TechMapper {
    /// Maps a single IR cell kind to device-specific primitives.
    ///
    /// Returns a [`MapResult`] indicating how the cell should be implemented
    /// on this device.
    fn map_cell(&self, cell_kind: &CellKind) -> MapResult;

    /// Attempts to infer a block RAM mapping for the given memory pattern.
    ///
    /// Returns `true` if the memory fits within this device's BRAM resources.
    fn infer_bram(&self, memory: &MemoryCell) -> bool;

    /// Attempts to infer a DSP block mapping for the given arithmetic pattern.
    ///
    /// Returns `true` if the arithmetic pattern fits within this device's DSP blocks.
    fn infer_dsp(&self, pattern: &ArithmeticPattern) -> bool;

    /// Decomposes the given cell kind into LUTs.
    ///
    /// Returns the LUT mappings needed to implement this cell, or an empty
    /// vector if the cell cannot be decomposed into LUTs.
    fn map_to_luts(&self, cell_kind: &CellKind) -> Vec<LutMapping>;

    /// Returns the number of inputs per LUT on this device (typically 4 or 6).
    fn lut_input_count(&self) -> u32;

    /// Returns the maximum depth of a single BRAM primitive in words.
    fn max_bram_depth(&self) -> u32;

    /// Returns the maximum width of a single BRAM primitive in bits.
    fn max_bram_width(&self) -> u32;

    /// Returns the maximum width of the A operand for a DSP block.
    fn max_dsp_width_a(&self) -> u32;

    /// Returns the maximum width of the B operand for a DSP block.
    fn max_dsp_width_b(&self) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_cell_construction() {
        let mem = MemoryCell {
            depth: 1024,
            width: 32,
            read_ports: 1,
            write_ports: 1,
            has_registered_output: true,
            clock_signal: Some(SignalId::from_raw(0)),
        };
        assert_eq!(mem.depth, 1024);
        assert_eq!(mem.width, 32);
        assert!(mem.has_registered_output);
        assert!(mem.clock_signal.is_some());
    }

    #[test]
    fn memory_cell_no_clock() {
        let mem = MemoryCell {
            depth: 256,
            width: 8,
            read_ports: 2,
            write_ports: 1,
            has_registered_output: false,
            clock_signal: None,
        };
        assert!(!mem.has_registered_output);
        assert!(mem.clock_signal.is_none());
    }

    #[test]
    fn arithmetic_pattern_multiply() {
        let pat = ArithmeticPattern {
            kind: ArithmeticPatternKind::Multiply,
            width_a: 18,
            width_b: 18,
            has_pipeline_regs: false,
            has_accumulator: false,
        };
        assert_eq!(pat.kind, ArithmeticPatternKind::Multiply);
        assert_eq!(pat.width_a, 18);
        assert!(!pat.has_accumulator);
    }

    #[test]
    fn arithmetic_pattern_mac() {
        let pat = ArithmeticPattern {
            kind: ArithmeticPatternKind::MultiplyAccumulate,
            width_a: 25,
            width_b: 18,
            has_pipeline_regs: true,
            has_accumulator: true,
        };
        assert_eq!(pat.kind, ArithmeticPatternKind::MultiplyAccumulate);
        assert!(pat.has_pipeline_regs);
        assert!(pat.has_accumulator);
    }

    #[test]
    fn logic_cone_construction() {
        let cone = LogicCone {
            output: SignalId::from_raw(10),
            cells: vec![CellId::from_raw(0), CellId::from_raw(1)],
            input_signals: vec![SignalId::from_raw(0), SignalId::from_raw(1)],
        };
        assert_eq!(cone.output, SignalId::from_raw(10));
        assert_eq!(cone.cells.len(), 2);
        assert_eq!(cone.input_signals.len(), 2);
    }

    #[test]
    fn map_result_variants() {
        let lut = MapResult::Luts(vec![LutMapping {
            input_count: 4,
            init_bits: vec![0x88],
        }]);
        assert!(matches!(lut, MapResult::Luts(_)));
        assert!(matches!(MapResult::Ff, MapResult::Ff));
        assert!(matches!(MapResult::Bram, MapResult::Bram));
        assert!(matches!(MapResult::Dsp, MapResult::Dsp));
        assert!(matches!(MapResult::PassThrough, MapResult::PassThrough));
        assert!(matches!(MapResult::Unmappable, MapResult::Unmappable));
    }

    #[test]
    fn lut_mapping_construction() {
        let lm = LutMapping {
            input_count: 6,
            init_bits: vec![0xFF; 8],
        };
        assert_eq!(lm.input_count, 6);
        assert_eq!(lm.init_bits.len(), 8);
    }

    #[test]
    fn memory_cell_serde_roundtrip() {
        let mem = MemoryCell {
            depth: 512,
            width: 16,
            read_ports: 1,
            write_ports: 1,
            has_registered_output: true,
            clock_signal: None,
        };
        let json = serde_json::to_string(&mem).unwrap();
        let restored: MemoryCell = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.depth, 512);
        assert_eq!(restored.width, 16);
    }

    #[test]
    fn arithmetic_pattern_serde_roundtrip() {
        let pat = ArithmeticPattern {
            kind: ArithmeticPatternKind::MultiplyAdd,
            width_a: 18,
            width_b: 25,
            has_pipeline_regs: true,
            has_accumulator: false,
        };
        let json = serde_json::to_string(&pat).unwrap();
        let restored: ArithmeticPattern = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.kind, ArithmeticPatternKind::MultiplyAdd);
        assert_eq!(restored.width_a, 18);
    }
}
