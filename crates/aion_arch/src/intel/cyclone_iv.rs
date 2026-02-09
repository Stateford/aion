//! Intel (Altera) Cyclone IV E device model and technology mapper.
//!
//! The Cyclone IV E family uses 60nm logic elements (LEs), each containing a
//! single 4-input LUT and one flip-flop. Memory is provided by M9K blocks
//! (9,216 bits each, max 36-bit width) and DSP operations use embedded 18x18
//! multiplier blocks. The Cyclone IV E is a popular hobbyist and educational
//! FPGA family, with the EP4CE22F17C6N being one of the most widely used parts
//! (featured on the DE0-Nano and many other development boards).

use crate::tech_map::{ArithmeticPattern, LutMapping, MapResult, MemoryCell, TechMapper};
use crate::types::ResourceUsage;
use crate::Architecture;
use aion_ir::CellKind;

/// Device parameters for a specific Cyclone IV E part number.
struct CycloneIvDevice {
    /// Part number string (e.g., "EP4CE22F17C6N").
    name: &'static str,
    /// Number of logic elements (each LE = 1 LUT4 + 1 FF).
    les: u32,
    /// Number of M9K memory blocks.
    m9k: u32,
    /// Number of embedded 18x18 multipliers.
    multipliers: u32,
    /// Number of user I/O pins.
    io: u32,
    /// Number of PLLs.
    pll: u32,
}

/// Known Cyclone IV E device variants.
const CYCLONE_IV_DEVICES: &[CycloneIvDevice] = &[
    CycloneIvDevice {
        name: "EP4CE6E22C8N",
        les: 6_272,
        m9k: 30,
        multipliers: 15,
        io: 91,
        pll: 2,
    },
    CycloneIvDevice {
        name: "EP4CE10F17C8N",
        les: 10_320,
        m9k: 46,
        multipliers: 23,
        io: 136,
        pll: 2,
    },
    CycloneIvDevice {
        name: "EP4CE22F17C6N",
        les: 22_320,
        m9k: 66,
        multipliers: 66,
        io: 154,
        pll: 4,
    },
    CycloneIvDevice {
        name: "EP4CE55F23C8N",
        les: 55_856,
        m9k: 260,
        multipliers: 154,
        io: 325,
        pll: 4,
    },
    CycloneIvDevice {
        name: "EP4CE115F29C7N",
        les: 114_480,
        m9k: 432,
        multipliers: 266,
        io: 528,
        pll: 4,
    },
];

/// The smallest Cyclone IV device, used as fallback for unknown part numbers.
const FALLBACK_INDEX: usize = 0;

/// Architecture model for the Intel (Altera) Cyclone IV E FPGA family.
///
/// Each logic element (LE) contains a single 4-input LUT and one flip-flop,
/// so `total_luts()` and `total_ffs()` both equal the LE count. The Cyclone IV
/// uses a simpler architecture than the Cyclone V's ALM-based design, making
/// it easier to learn but less resource-efficient per area.
#[derive(Debug)]
pub struct CycloneIv {
    /// Index into `CYCLONE_IV_DEVICES` for the selected part.
    device_index: usize,
}

impl CycloneIv {
    /// Creates a Cyclone IV architecture for the given device part number.
    ///
    /// If the exact part number is not found, falls back to the smallest
    /// known device (EP4CE6E22C8N).
    pub fn new(device: &str) -> (Self, bool) {
        let index = CYCLONE_IV_DEVICES
            .iter()
            .position(|d| d.name.eq_ignore_ascii_case(device));
        match index {
            Some(i) => (Self { device_index: i }, true),
            None => (
                Self {
                    device_index: FALLBACK_INDEX,
                },
                false,
            ),
        }
    }

    /// Returns the device parameters for the selected part.
    fn device(&self) -> &CycloneIvDevice {
        &CYCLONE_IV_DEVICES[self.device_index]
    }
}

impl Architecture for CycloneIv {
    fn family_name(&self) -> &str {
        "cyclone_iv"
    }

    fn device_name(&self) -> &str {
        self.device().name
    }

    fn total_luts(&self) -> u32 {
        // Each LE contains one 4-input LUT
        self.device().les
    }

    fn total_ffs(&self) -> u32 {
        // Each LE contains one flip-flop
        self.device().les
    }

    fn total_bram(&self) -> u32 {
        self.device().m9k
    }

    fn total_dsp(&self) -> u32 {
        self.device().multipliers
    }

    fn total_io(&self) -> u32 {
        self.device().io
    }

    fn total_pll(&self) -> u32 {
        self.device().pll
    }

    fn lut_input_count(&self) -> u32 {
        4
    }

    fn resource_summary(&self) -> ResourceUsage {
        let dev = self.device();
        ResourceUsage {
            luts: dev.les,
            ffs: dev.les,
            bram: dev.m9k,
            dsp: dev.multipliers,
            io: dev.io,
            pll: dev.pll,
        }
    }

    fn tech_mapper(&self) -> Box<dyn TechMapper> {
        Box::new(CycloneIvMapper)
    }
}

/// Technology mapper for Intel (Altera) Cyclone IV E devices.
///
/// Maps generic IR cells to Cyclone IV primitives. LEs use 4-input LUTs,
/// M9K blocks have 9,216 bits capacity (max 36-bit width), and embedded
/// multipliers support 18x18 multiplication.
#[derive(Debug)]
pub struct CycloneIvMapper;

impl CycloneIvMapper {
    /// M9K block capacity in bits.
    const M9K_DEPTH: u32 = 9_216;
    /// M9K maximum data width in bits.
    const M9K_MAX_WIDTH: u32 = 36;
    /// Embedded multiplier maximum A operand width.
    const DSP_MAX_WIDTH_A: u32 = 18;
    /// Embedded multiplier maximum B operand width.
    const DSP_MAX_WIDTH_B: u32 = 18;

    /// Generates a LUT truth table for a 2-input AND gate.
    fn and2_truth_table() -> Vec<u8> {
        // 2-input AND: 00->0, 01->0, 10->0, 11->1 = 0b1000 = 0x8
        vec![0x08]
    }

    /// Generates a LUT truth table for a 2-input OR gate.
    fn or2_truth_table() -> Vec<u8> {
        // 2-input OR: 00->0, 01->1, 10->1, 11->1 = 0b1110 = 0xE
        vec![0x0E]
    }

    /// Generates a LUT truth table for a 2-input XOR gate.
    fn xor2_truth_table() -> Vec<u8> {
        // 2-input XOR: 00->0, 01->1, 10->1, 11->0 = 0b0110 = 0x6
        vec![0x06]
    }

    /// Generates a LUT truth table for a 1-input NOT gate.
    fn not1_truth_table() -> Vec<u8> {
        // 1-input NOT: 0->1, 1->0 = 0b01 = 0x1
        vec![0x01]
    }
}

impl TechMapper for CycloneIvMapper {
    fn map_cell(&self, cell_kind: &CellKind) -> MapResult {
        match cell_kind {
            CellKind::And { width } => {
                let luts: Vec<LutMapping> = (0..*width)
                    .map(|_| LutMapping {
                        input_count: 2,
                        init_bits: Self::and2_truth_table(),
                    })
                    .collect();
                MapResult::Luts(luts)
            }
            CellKind::Or { width } => {
                let luts: Vec<LutMapping> = (0..*width)
                    .map(|_| LutMapping {
                        input_count: 2,
                        init_bits: Self::or2_truth_table(),
                    })
                    .collect();
                MapResult::Luts(luts)
            }
            CellKind::Xor { width } => {
                let luts: Vec<LutMapping> = (0..*width)
                    .map(|_| LutMapping {
                        input_count: 2,
                        init_bits: Self::xor2_truth_table(),
                    })
                    .collect();
                MapResult::Luts(luts)
            }
            CellKind::Not { width } => {
                let luts: Vec<LutMapping> = (0..*width)
                    .map(|_| LutMapping {
                        input_count: 1,
                        init_bits: Self::not1_truth_table(),
                    })
                    .collect();
                MapResult::Luts(luts)
            }
            CellKind::Dff { .. } | CellKind::Latch { .. } => MapResult::Ff,
            CellKind::Bram(_) => MapResult::Bram,
            CellKind::Dsp(_) => MapResult::Dsp,
            CellKind::Lut { .. }
            | CellKind::Carry { .. }
            | CellKind::Pll(_)
            | CellKind::Iobuf(_) => MapResult::PassThrough,
            _ => MapResult::Unmappable,
        }
    }

    fn infer_bram(&self, memory: &MemoryCell) -> bool {
        let total_bits = memory.depth as u64 * memory.width as u64;
        total_bits <= Self::M9K_DEPTH as u64 * Self::M9K_MAX_WIDTH as u64
            && memory.width <= Self::M9K_MAX_WIDTH
    }

    fn infer_dsp(&self, pattern: &ArithmeticPattern) -> bool {
        pattern.width_a <= Self::DSP_MAX_WIDTH_A && pattern.width_b <= Self::DSP_MAX_WIDTH_B
    }

    fn map_to_luts(&self, cell_kind: &CellKind) -> Vec<LutMapping> {
        match self.map_cell(cell_kind) {
            MapResult::Luts(luts) => luts,
            _ => Vec::new(),
        }
    }

    fn lut_input_count(&self) -> u32 {
        4
    }

    fn max_bram_depth(&self) -> u32 {
        Self::M9K_DEPTH
    }

    fn max_bram_width(&self) -> u32 {
        Self::M9K_MAX_WIDTH
    }

    fn max_dsp_width_a(&self) -> u32 {
        Self::DSP_MAX_WIDTH_A
    }

    fn max_dsp_width_b(&self) -> u32 {
        Self::DSP_MAX_WIDTH_B
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tech_map::ArithmeticPatternKind;

    #[test]
    fn cyclone_iv_ep4ce22() {
        let (c4, exact) = CycloneIv::new("EP4CE22F17C6N");
        assert!(exact);
        assert_eq!(c4.device_name(), "EP4CE22F17C6N");
        assert_eq!(c4.total_luts(), 22_320);
        assert_eq!(c4.total_ffs(), 22_320); // 1 FF per LE
        assert_eq!(c4.total_bram(), 66);
        assert_eq!(c4.total_dsp(), 66);
        assert_eq!(c4.total_io(), 154);
        assert_eq!(c4.total_pll(), 4);
    }

    #[test]
    fn cyclone_iv_case_insensitive() {
        let (c4, exact) = CycloneIv::new("ep4ce22f17c6n");
        assert!(exact);
        assert_eq!(c4.device_name(), "EP4CE22F17C6N");
    }

    #[test]
    fn cyclone_iv_smallest_device() {
        let (c4, exact) = CycloneIv::new("EP4CE6E22C8N");
        assert!(exact);
        assert_eq!(c4.total_luts(), 6_272);
        assert_eq!(c4.total_bram(), 30);
        assert_eq!(c4.total_dsp(), 15);
        assert_eq!(c4.total_pll(), 2);
    }

    #[test]
    fn cyclone_iv_mid_device() {
        let (c4, exact) = CycloneIv::new("EP4CE10F17C8N");
        assert!(exact);
        assert_eq!(c4.total_luts(), 10_320);
        assert_eq!(c4.total_bram(), 46);
    }

    #[test]
    fn cyclone_iv_large_device() {
        let (c4, exact) = CycloneIv::new("EP4CE55F23C8N");
        assert!(exact);
        assert_eq!(c4.total_luts(), 55_856);
        assert_eq!(c4.total_bram(), 260);
    }

    #[test]
    fn cyclone_iv_largest_device() {
        let (c4, exact) = CycloneIv::new("EP4CE115F29C7N");
        assert!(exact);
        assert_eq!(c4.total_luts(), 114_480);
        assert_eq!(c4.total_bram(), 432);
        assert_eq!(c4.total_dsp(), 266);
        assert_eq!(c4.total_io(), 528);
    }

    #[test]
    fn cyclone_iv_unknown_device_fallback() {
        let (c4, exact) = CycloneIv::new("UNKNOWN_PART");
        assert!(!exact);
        assert_eq!(c4.device_name(), "EP4CE6E22C8N");
    }

    #[test]
    fn cyclone_iv_family_name() {
        let (c4, _) = CycloneIv::new("EP4CE22F17C6N");
        assert_eq!(c4.family_name(), "cyclone_iv");
    }

    #[test]
    fn cyclone_iv_lut_input_count() {
        let (c4, _) = CycloneIv::new("EP4CE22F17C6N");
        assert_eq!(c4.lut_input_count(), 4);
    }

    #[test]
    fn cyclone_iv_resource_summary() {
        let (c4, _) = CycloneIv::new("EP4CE22F17C6N");
        let summary = c4.resource_summary();
        assert_eq!(summary.luts, 22_320);
        assert_eq!(summary.ffs, 22_320);
        assert_eq!(summary.bram, 66);
        assert_eq!(summary.dsp, 66);
        assert_eq!(summary.io, 154);
        assert_eq!(summary.pll, 4);
        assert_eq!(summary.total_logic(), 44_640);
    }

    #[test]
    fn mapper_and_gate() {
        let mapper = CycloneIvMapper;
        let result = mapper.map_cell(&CellKind::And { width: 4 });
        if let MapResult::Luts(luts) = result {
            assert_eq!(luts.len(), 4);
            assert_eq!(luts[0].input_count, 2);
            assert_eq!(luts[0].init_bits, vec![0x08]);
        } else {
            panic!("expected Luts result");
        }
    }

    #[test]
    fn mapper_or_gate() {
        let mapper = CycloneIvMapper;
        let result = mapper.map_cell(&CellKind::Or { width: 1 });
        if let MapResult::Luts(luts) = result {
            assert_eq!(luts.len(), 1);
            assert_eq!(luts[0].init_bits, vec![0x0E]);
        } else {
            panic!("expected Luts result");
        }
    }

    #[test]
    fn mapper_xor_gate() {
        let mapper = CycloneIvMapper;
        let result = mapper.map_cell(&CellKind::Xor { width: 2 });
        if let MapResult::Luts(luts) = result {
            assert_eq!(luts.len(), 2);
            assert_eq!(luts[0].init_bits, vec![0x06]);
        } else {
            panic!("expected Luts result");
        }
    }

    #[test]
    fn mapper_not_gate() {
        let mapper = CycloneIvMapper;
        let result = mapper.map_cell(&CellKind::Not { width: 8 });
        if let MapResult::Luts(luts) = result {
            assert_eq!(luts.len(), 8);
            assert_eq!(luts[0].input_count, 1);
            assert_eq!(luts[0].init_bits, vec![0x01]);
        } else {
            panic!("expected Luts result");
        }
    }

    #[test]
    fn mapper_dff() {
        let mapper = CycloneIvMapper;
        let result = mapper.map_cell(&CellKind::Dff {
            width: 1,
            has_reset: true,
            has_enable: false,
        });
        assert!(matches!(result, MapResult::Ff));
    }

    #[test]
    fn mapper_passthrough() {
        let mapper = CycloneIvMapper;
        let result = mapper.map_cell(&CellKind::Carry { width: 4 });
        assert!(matches!(result, MapResult::PassThrough));
    }

    #[test]
    fn mapper_unmappable() {
        let mapper = CycloneIvMapper;
        let result = mapper.map_cell(&CellKind::Mul { width: 32 });
        assert!(matches!(result, MapResult::Unmappable));
    }

    #[test]
    fn mapper_infer_bram_fits() {
        let mapper = CycloneIvMapper;
        let mem = MemoryCell {
            depth: 512,
            width: 18,
            read_ports: 1,
            write_ports: 1,
            has_registered_output: true,
            clock_signal: None,
        };
        assert!(mapper.infer_bram(&mem));
    }

    #[test]
    fn mapper_infer_bram_too_wide() {
        let mapper = CycloneIvMapper;
        let mem = MemoryCell {
            depth: 256,
            width: 40, // exceeds M9K max width of 36
            read_ports: 1,
            write_ports: 1,
            has_registered_output: true,
            clock_signal: None,
        };
        assert!(!mapper.infer_bram(&mem));
    }

    #[test]
    fn mapper_infer_dsp_fits() {
        let mapper = CycloneIvMapper;
        let pat = ArithmeticPattern {
            kind: ArithmeticPatternKind::Multiply,
            width_a: 18,
            width_b: 18,
            has_pipeline_regs: false,
            has_accumulator: false,
        };
        assert!(mapper.infer_dsp(&pat));
    }

    #[test]
    fn mapper_infer_dsp_too_wide() {
        let mapper = CycloneIvMapper;
        let pat = ArithmeticPattern {
            kind: ArithmeticPatternKind::Multiply,
            width_a: 25,
            width_b: 18,
            has_pipeline_regs: false,
            has_accumulator: false,
        };
        assert!(!mapper.infer_dsp(&pat));
    }

    #[test]
    fn mapper_limits() {
        let mapper = CycloneIvMapper;
        assert_eq!(mapper.lut_input_count(), 4);
        assert_eq!(mapper.max_bram_depth(), 9_216);
        assert_eq!(mapper.max_bram_width(), 36);
        assert_eq!(mapper.max_dsp_width_a(), 18);
        assert_eq!(mapper.max_dsp_width_b(), 18);
    }

    #[test]
    fn mapper_map_to_luts() {
        let mapper = CycloneIvMapper;
        let luts = mapper.map_to_luts(&CellKind::And { width: 2 });
        assert_eq!(luts.len(), 2);

        let luts = mapper.map_to_luts(&CellKind::Dff {
            width: 1,
            has_reset: false,
            has_enable: false,
        });
        assert!(luts.is_empty());
    }

    #[test]
    fn tech_mapper_via_architecture() {
        let (c4, _) = CycloneIv::new("EP4CE22F17C6N");
        let mapper = c4.tech_mapper();
        assert_eq!(mapper.lut_input_count(), 4);
        assert_eq!(mapper.max_bram_depth(), 9_216);
    }

    #[test]
    fn default_grid_methods() {
        let (c4, _) = CycloneIv::new("EP4CE22F17C6N");
        let (cols, rows) = c4.grid_dimensions();
        assert_eq!(cols, 0);
        assert_eq!(rows, 0);
        assert!(c4.get_tile(0, 0).is_none());
        assert!(c4.get_site(crate::ids::SiteId::from_raw(0)).is_none());
    }
}
