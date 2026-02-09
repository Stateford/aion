//! Intel Cyclone V device model and technology mapper.
//!
//! The Cyclone V family uses 28nm Adaptive Logic Modules (ALMs), each containing
//! a fracturable 8-input LUT decomposable into two 6-input LUTs, two flip-flops,
//! and arithmetic carry logic. Memory is provided by M10K blocks (10,240 bits each)
//! and DSP operations use 18x18 multiplier blocks.

use crate::tech_map::{ArithmeticPattern, LutMapping, MapResult, MemoryCell, TechMapper};
use crate::types::ResourceUsage;
use crate::Architecture;
use aion_ir::CellKind;

/// Device parameters for a specific Cyclone V part number.
struct CycloneVDevice {
    /// Part number string (e.g., "5CSEMA5F31C6").
    name: &'static str,
    /// Number of Adaptive Logic Modules.
    alms: u32,
    /// Number of flip-flops.
    ffs: u32,
    /// Number of M10K memory blocks.
    m10k: u32,
    /// Number of 18x18 DSP blocks.
    dsp: u32,
    /// Number of user I/O pins.
    io: u32,
    /// Number of PLLs.
    pll: u32,
}

/// Known Cyclone V device variants.
const CYCLONE_V_DEVICES: &[CycloneVDevice] = &[
    CycloneVDevice {
        name: "5CSEMA5F31C6",
        alms: 32_070,
        ffs: 64_140,
        m10k: 397,
        dsp: 87,
        io: 369,
        pll: 6,
    },
    CycloneVDevice {
        name: "5CSEBA6U23I7",
        alms: 41_910,
        ffs: 83_820,
        m10k: 553,
        dsp: 112,
        io: 240,
        pll: 6,
    },
    CycloneVDevice {
        name: "5CEBA4F23C7",
        alms: 18_480,
        ffs: 36_960,
        m10k: 308,
        dsp: 66,
        io: 224,
        pll: 4,
    },
];

/// The smallest Cyclone V device, used as fallback for unknown part numbers.
const FALLBACK_INDEX: usize = 2;

/// Architecture model for the Intel Cyclone V FPGA family.
///
/// Each ALM contains a fracturable 8-input LUT (decomposable into two 6-LUTs),
/// two flip-flops, and carry logic. LUT count is reported as ALMs (each ALM
/// provides the equivalent of ~2 four-input LUTs).
#[derive(Debug)]
pub struct CycloneV {
    /// Index into `CYCLONE_V_DEVICES` for the selected part.
    device_index: usize,
}

impl CycloneV {
    /// Creates a Cyclone V architecture for the given device part number.
    ///
    /// If the exact part number is not found, falls back to the smallest
    /// known device (5CEBA4F23C7).
    pub fn new(device: &str) -> (Self, bool) {
        let index = CYCLONE_V_DEVICES
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
    fn device(&self) -> &CycloneVDevice {
        &CYCLONE_V_DEVICES[self.device_index]
    }
}

impl Architecture for CycloneV {
    fn family_name(&self) -> &str {
        "cyclone_v"
    }

    fn device_name(&self) -> &str {
        self.device().name
    }

    fn total_luts(&self) -> u32 {
        self.device().alms
    }

    fn total_ffs(&self) -> u32 {
        self.device().ffs
    }

    fn total_bram(&self) -> u32 {
        self.device().m10k
    }

    fn total_dsp(&self) -> u32 {
        self.device().dsp
    }

    fn total_io(&self) -> u32 {
        self.device().io
    }

    fn total_pll(&self) -> u32 {
        self.device().pll
    }

    fn lut_input_count(&self) -> u32 {
        6
    }

    fn resource_summary(&self) -> ResourceUsage {
        let dev = self.device();
        ResourceUsage {
            luts: dev.alms,
            ffs: dev.ffs,
            bram: dev.m10k,
            dsp: dev.dsp,
            io: dev.io,
            pll: dev.pll,
        }
    }

    fn tech_mapper(&self) -> Box<dyn TechMapper> {
        Box::new(CycloneVMapper)
    }
}

/// Technology mapper for Intel Cyclone V devices.
///
/// Maps generic IR cells to Cyclone V primitives. ALMs use 6-input LUTs,
/// M10K blocks have 10,240 bits capacity (max 40-bit width), and DSP blocks
/// support 18x18 multiplication.
#[derive(Debug)]
pub struct CycloneVMapper;

impl CycloneVMapper {
    /// M10K block depth in bits.
    const M10K_DEPTH: u32 = 10_240;
    /// M10K maximum data width.
    const M10K_MAX_WIDTH: u32 = 40;
    /// DSP block maximum A operand width.
    const DSP_MAX_WIDTH_A: u32 = 18;
    /// DSP block maximum B operand width.
    const DSP_MAX_WIDTH_B: u32 = 18;

    /// Generates a LUT truth table for a 2-input AND gate.
    fn and2_truth_table() -> Vec<u8> {
        // 2-input AND: output 1 only when both inputs are 1
        // Inputs: B A -> bit index = B*2 + A
        // 00->0, 01->0, 10->0, 11->1 = 0b1000 = 0x8
        vec![0x08]
    }

    /// Generates a LUT truth table for a 2-input OR gate.
    fn or2_truth_table() -> Vec<u8> {
        // 2-input OR: output 1 when either input is 1
        // 00->0, 01->1, 10->1, 11->1 = 0b1110 = 0xE
        vec![0x0E]
    }

    /// Generates a LUT truth table for a 2-input XOR gate.
    fn xor2_truth_table() -> Vec<u8> {
        // 2-input XOR: output 1 when inputs differ
        // 00->0, 01->1, 10->1, 11->0 = 0b0110 = 0x6
        vec![0x06]
    }

    /// Generates a LUT truth table for a 1-input NOT gate.
    fn not1_truth_table() -> Vec<u8> {
        // 1-input NOT: output is inverse of input
        // 0->1, 1->0 = 0b01 = 0x1
        vec![0x01]
    }
}

impl TechMapper for CycloneVMapper {
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
        total_bits <= Self::M10K_DEPTH as u64 * Self::M10K_MAX_WIDTH as u64
            && memory.width <= Self::M10K_MAX_WIDTH
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
        6
    }

    fn max_bram_depth(&self) -> u32 {
        Self::M10K_DEPTH
    }

    fn max_bram_width(&self) -> u32 {
        Self::M10K_MAX_WIDTH
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
    fn cyclone_v_known_device() {
        let (cv, exact) = CycloneV::new("5CSEMA5F31C6");
        assert!(exact);
        assert_eq!(cv.device_name(), "5CSEMA5F31C6");
        assert_eq!(cv.total_luts(), 32_070);
        assert_eq!(cv.total_ffs(), 64_140);
        assert_eq!(cv.total_bram(), 397);
        assert_eq!(cv.total_dsp(), 87);
        assert_eq!(cv.total_io(), 369);
        assert_eq!(cv.total_pll(), 6);
    }

    #[test]
    fn cyclone_v_case_insensitive() {
        let (cv, exact) = CycloneV::new("5csema5f31c6");
        assert!(exact);
        assert_eq!(cv.device_name(), "5CSEMA5F31C6");
    }

    #[test]
    fn cyclone_v_second_device() {
        let (cv, exact) = CycloneV::new("5CSEBA6U23I7");
        assert!(exact);
        assert_eq!(cv.total_luts(), 41_910);
        assert_eq!(cv.total_ffs(), 83_820);
    }

    #[test]
    fn cyclone_v_third_device() {
        let (cv, exact) = CycloneV::new("5CEBA4F23C7");
        assert!(exact);
        assert_eq!(cv.total_luts(), 18_480);
        assert_eq!(cv.total_pll(), 4);
    }

    #[test]
    fn cyclone_v_unknown_device_fallback() {
        let (cv, exact) = CycloneV::new("UNKNOWN_PART");
        assert!(!exact);
        // Falls back to smallest device
        assert_eq!(cv.device_name(), "5CEBA4F23C7");
    }

    #[test]
    fn cyclone_v_family_name() {
        let (cv, _) = CycloneV::new("5CSEMA5F31C6");
        assert_eq!(cv.family_name(), "cyclone_v");
    }

    #[test]
    fn cyclone_v_lut_input_count() {
        let (cv, _) = CycloneV::new("5CSEMA5F31C6");
        assert_eq!(cv.lut_input_count(), 6);
    }

    #[test]
    fn cyclone_v_resource_summary() {
        let (cv, _) = CycloneV::new("5CSEMA5F31C6");
        let summary = cv.resource_summary();
        assert_eq!(summary.luts, 32_070);
        assert_eq!(summary.ffs, 64_140);
        assert_eq!(summary.bram, 397);
        assert_eq!(summary.dsp, 87);
        assert_eq!(summary.io, 369);
        assert_eq!(summary.pll, 6);
    }

    #[test]
    fn mapper_and_gate() {
        let mapper = CycloneVMapper;
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
        let mapper = CycloneVMapper;
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
        let mapper = CycloneVMapper;
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
        let mapper = CycloneVMapper;
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
        let mapper = CycloneVMapper;
        let result = mapper.map_cell(&CellKind::Dff {
            width: 1,
            has_reset: true,
            has_enable: false,
        });
        assert!(matches!(result, MapResult::Ff));
    }

    #[test]
    fn mapper_passthrough() {
        let mapper = CycloneVMapper;
        let result = mapper.map_cell(&CellKind::Carry { width: 4 });
        assert!(matches!(result, MapResult::PassThrough));
    }

    #[test]
    fn mapper_unmappable() {
        let mapper = CycloneVMapper;
        let result = mapper.map_cell(&CellKind::Mul { width: 32 });
        assert!(matches!(result, MapResult::Unmappable));
    }

    #[test]
    fn mapper_infer_bram_fits() {
        let mapper = CycloneVMapper;
        let mem = MemoryCell {
            depth: 1024,
            width: 32,
            read_ports: 1,
            write_ports: 1,
            has_registered_output: true,
            clock_signal: None,
        };
        assert!(mapper.infer_bram(&mem));
    }

    #[test]
    fn mapper_infer_bram_too_wide() {
        let mapper = CycloneVMapper;
        let mem = MemoryCell {
            depth: 256,
            width: 64, // exceeds M10K max width of 40
            read_ports: 1,
            write_ports: 1,
            has_registered_output: true,
            clock_signal: None,
        };
        assert!(!mapper.infer_bram(&mem));
    }

    #[test]
    fn mapper_infer_dsp_fits() {
        let mapper = CycloneVMapper;
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
        let mapper = CycloneVMapper;
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
        let mapper = CycloneVMapper;
        assert_eq!(mapper.lut_input_count(), 6);
        assert_eq!(mapper.max_bram_depth(), 10_240);
        assert_eq!(mapper.max_bram_width(), 40);
        assert_eq!(mapper.max_dsp_width_a(), 18);
        assert_eq!(mapper.max_dsp_width_b(), 18);
    }

    #[test]
    fn mapper_map_to_luts() {
        let mapper = CycloneVMapper;
        let luts = mapper.map_to_luts(&CellKind::And { width: 2 });
        assert_eq!(luts.len(), 2);

        // Non-LUT-mappable cell returns empty
        let luts = mapper.map_to_luts(&CellKind::Dff {
            width: 1,
            has_reset: false,
            has_enable: false,
        });
        assert!(luts.is_empty());
    }

    #[test]
    fn tech_mapper_via_architecture() {
        let (cv, _) = CycloneV::new("5CSEMA5F31C6");
        let mapper = cv.tech_mapper();
        assert_eq!(mapper.lut_input_count(), 6);
    }

    #[test]
    fn default_grid_methods() {
        let (cv, _) = CycloneV::new("5CSEMA5F31C6");
        let (cols, rows) = cv.grid_dimensions();
        assert_eq!(cols, 0);
        assert_eq!(rows, 0);
        assert!(cv.get_tile(0, 0).is_none());
        assert!(cv.get_site(crate::ids::SiteId::from_raw(0)).is_none());
    }
}
