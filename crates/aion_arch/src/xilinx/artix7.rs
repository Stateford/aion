//! Xilinx Artix-7 device model and technology mapper.
//!
//! The Artix-7 family uses 28nm CLB (Configurable Logic Block) architecture
//! with SLICEL/SLICEM slices. Each slice contains four 6-input LUTs, eight
//! flip-flops, and carry logic. Memory is provided by 36Kb Block RAMs
//! (BRAM36, configurable as 2x BRAM18) and DSP operations use DSP48E1 slices
//! supporting 25x18 multiplication.

use crate::tech_map::{ArithmeticPattern, LutMapping, MapResult, MemoryCell, TechMapper};
use crate::types::ResourceUsage;
use crate::Architecture;
use aion_ir::CellKind;

/// Device parameters for a specific Artix-7 part number.
struct Artix7Device {
    /// Part number string (e.g., "xc7a35ticpg236-1L").
    name: &'static str,
    /// Number of 6-input LUTs.
    luts: u32,
    /// Number of flip-flops.
    ffs: u32,
    /// Number of 36Kb Block RAM tiles.
    bram36: u32,
    /// Number of DSP48E1 slices.
    dsp48e1: u32,
    /// Number of user I/O pins.
    io: u32,
    /// Number of MMCM (Mixed-Mode Clock Manager) blocks.
    mmcm: u32,
}

/// Known Artix-7 device variants.
const ARTIX7_DEVICES: &[Artix7Device] = &[
    Artix7Device {
        name: "xc7a35ticpg236-1L",
        luts: 20_800,
        ffs: 41_600,
        bram36: 50,
        dsp48e1: 90,
        io: 106,
        mmcm: 5,
    },
    Artix7Device {
        name: "xc7a100tcsg324-1",
        luts: 63_400,
        ffs: 126_800,
        bram36: 135,
        dsp48e1: 240,
        io: 210,
        mmcm: 6,
    },
    Artix7Device {
        name: "xc7a200tffg1156-1",
        luts: 134_600,
        ffs: 269_200,
        bram36: 365,
        dsp48e1: 740,
        io: 500,
        mmcm: 10,
    },
];

/// The smallest Artix-7 device, used as fallback for unknown part numbers.
const FALLBACK_INDEX: usize = 0;

/// Architecture model for the Xilinx Artix-7 FPGA family.
///
/// Uses 6-input LUTs organized in SLICEL/SLICEM slices within CLBs.
/// Block RAM is provided as 36Kb tiles (BRAM36), each configurable as
/// one 36Kb or two 18Kb memories. DSP is provided by DSP48E1 slices
/// supporting 25x18 signed multiplication.
#[derive(Debug)]
pub struct Artix7 {
    /// Index into `ARTIX7_DEVICES` for the selected part.
    device_index: usize,
}

impl Artix7 {
    /// Creates an Artix-7 architecture for the given device part number.
    ///
    /// If the exact part number is not found, falls back to the smallest
    /// known device (xc7a35ticpg236-1L).
    pub fn new(device: &str) -> (Self, bool) {
        let index = ARTIX7_DEVICES
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
    fn device(&self) -> &Artix7Device {
        &ARTIX7_DEVICES[self.device_index]
    }
}

impl Architecture for Artix7 {
    fn family_name(&self) -> &str {
        "artix7"
    }

    fn device_name(&self) -> &str {
        self.device().name
    }

    fn total_luts(&self) -> u32 {
        self.device().luts
    }

    fn total_ffs(&self) -> u32 {
        self.device().ffs
    }

    fn total_bram(&self) -> u32 {
        self.device().bram36
    }

    fn total_dsp(&self) -> u32 {
        self.device().dsp48e1
    }

    fn total_io(&self) -> u32 {
        self.device().io
    }

    fn total_pll(&self) -> u32 {
        self.device().mmcm
    }

    fn lut_input_count(&self) -> u32 {
        6
    }

    fn resource_summary(&self) -> ResourceUsage {
        let dev = self.device();
        ResourceUsage {
            luts: dev.luts,
            ffs: dev.ffs,
            bram: dev.bram36,
            dsp: dev.dsp48e1,
            io: dev.io,
            pll: dev.mmcm,
        }
    }

    fn tech_mapper(&self) -> Box<dyn TechMapper> {
        Box::new(Artix7Mapper)
    }
}

/// Technology mapper for Xilinx Artix-7 devices.
///
/// Maps generic IR cells to Artix-7 primitives. CLBs use 6-input LUTs,
/// BRAM36 tiles have 36,864 bits capacity (max 72-bit width in SDP mode),
/// and DSP48E1 slices support 25x18 signed multiplication.
#[derive(Debug)]
pub struct Artix7Mapper;

impl Artix7Mapper {
    /// BRAM36 depth in bits (36 * 1024).
    const BRAM36_DEPTH: u32 = 36_864;
    /// BRAM36 maximum data width (72 bits in simple dual-port mode).
    const BRAM36_MAX_WIDTH: u32 = 72;
    /// DSP48E1 maximum A operand width.
    const DSP_MAX_WIDTH_A: u32 = 25;
    /// DSP48E1 maximum B operand width.
    const DSP_MAX_WIDTH_B: u32 = 18;

    /// Generates a LUT truth table for a 2-input AND gate.
    fn and2_truth_table() -> Vec<u8> {
        vec![0x08]
    }

    /// Generates a LUT truth table for a 2-input OR gate.
    fn or2_truth_table() -> Vec<u8> {
        vec![0x0E]
    }

    /// Generates a LUT truth table for a 2-input XOR gate.
    fn xor2_truth_table() -> Vec<u8> {
        vec![0x06]
    }

    /// Generates a LUT truth table for a 1-input NOT gate.
    fn not1_truth_table() -> Vec<u8> {
        vec![0x01]
    }
}

impl TechMapper for Artix7Mapper {
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
        total_bits <= Self::BRAM36_DEPTH as u64 * Self::BRAM36_MAX_WIDTH as u64
            && memory.width <= Self::BRAM36_MAX_WIDTH
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
        Self::BRAM36_DEPTH
    }

    fn max_bram_width(&self) -> u32 {
        Self::BRAM36_MAX_WIDTH
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
    fn artix7_known_device_small() {
        let (a7, exact) = Artix7::new("xc7a35ticpg236-1L");
        assert!(exact);
        assert_eq!(a7.device_name(), "xc7a35ticpg236-1L");
        assert_eq!(a7.total_luts(), 20_800);
        assert_eq!(a7.total_ffs(), 41_600);
        assert_eq!(a7.total_bram(), 50);
        assert_eq!(a7.total_dsp(), 90);
        assert_eq!(a7.total_io(), 106);
        assert_eq!(a7.total_pll(), 5);
    }

    #[test]
    fn artix7_case_insensitive() {
        let (a7, exact) = Artix7::new("XC7A35TICPG236-1L");
        assert!(exact);
        assert_eq!(a7.device_name(), "xc7a35ticpg236-1L");
    }

    #[test]
    fn artix7_medium_device() {
        let (a7, exact) = Artix7::new("xc7a100tcsg324-1");
        assert!(exact);
        assert_eq!(a7.total_luts(), 63_400);
        assert_eq!(a7.total_ffs(), 126_800);
        assert_eq!(a7.total_bram(), 135);
    }

    #[test]
    fn artix7_large_device() {
        let (a7, exact) = Artix7::new("xc7a200tffg1156-1");
        assert!(exact);
        assert_eq!(a7.total_luts(), 134_600);
        assert_eq!(a7.total_ffs(), 269_200);
        assert_eq!(a7.total_dsp(), 740);
        assert_eq!(a7.total_pll(), 10);
    }

    #[test]
    fn artix7_unknown_device_fallback() {
        let (a7, exact) = Artix7::new("UNKNOWN_PART");
        assert!(!exact);
        assert_eq!(a7.device_name(), "xc7a35ticpg236-1L");
    }

    #[test]
    fn artix7_family_name() {
        let (a7, _) = Artix7::new("xc7a35ticpg236-1L");
        assert_eq!(a7.family_name(), "artix7");
    }

    #[test]
    fn artix7_lut_input_count() {
        let (a7, _) = Artix7::new("xc7a35ticpg236-1L");
        assert_eq!(a7.lut_input_count(), 6);
    }

    #[test]
    fn artix7_resource_summary() {
        let (a7, _) = Artix7::new("xc7a100tcsg324-1");
        let summary = a7.resource_summary();
        assert_eq!(summary.luts, 63_400);
        assert_eq!(summary.ffs, 126_800);
        assert_eq!(summary.bram, 135);
        assert_eq!(summary.dsp, 240);
        assert_eq!(summary.io, 210);
        assert_eq!(summary.pll, 6);
    }

    #[test]
    fn mapper_and_gate() {
        let mapper = Artix7Mapper;
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
        let mapper = Artix7Mapper;
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
        let mapper = Artix7Mapper;
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
        let mapper = Artix7Mapper;
        let result = mapper.map_cell(&CellKind::Not { width: 8 });
        if let MapResult::Luts(luts) = result {
            assert_eq!(luts.len(), 8);
            assert_eq!(luts[0].input_count, 1);
        } else {
            panic!("expected Luts result");
        }
    }

    #[test]
    fn mapper_dff() {
        let mapper = Artix7Mapper;
        let result = mapper.map_cell(&CellKind::Dff {
            width: 1,
            has_reset: true,
            has_enable: false,
        });
        assert!(matches!(result, MapResult::Ff));
    }

    #[test]
    fn mapper_passthrough() {
        let mapper = Artix7Mapper;
        let result = mapper.map_cell(&CellKind::Carry { width: 4 });
        assert!(matches!(result, MapResult::PassThrough));
    }

    #[test]
    fn mapper_unmappable() {
        let mapper = Artix7Mapper;
        let result = mapper.map_cell(&CellKind::Mul { width: 32 });
        assert!(matches!(result, MapResult::Unmappable));
    }

    #[test]
    fn mapper_infer_bram_fits() {
        let mapper = Artix7Mapper;
        let mem = MemoryCell {
            depth: 4096,
            width: 36,
            read_ports: 1,
            write_ports: 1,
            has_registered_output: true,
            clock_signal: None,
        };
        assert!(mapper.infer_bram(&mem));
    }

    #[test]
    fn mapper_infer_bram_too_wide() {
        let mapper = Artix7Mapper;
        let mem = MemoryCell {
            depth: 256,
            width: 128, // exceeds BRAM36 max width of 72
            read_ports: 1,
            write_ports: 1,
            has_registered_output: true,
            clock_signal: None,
        };
        assert!(!mapper.infer_bram(&mem));
    }

    #[test]
    fn mapper_infer_dsp_fits() {
        let mapper = Artix7Mapper;
        let pat = ArithmeticPattern {
            kind: ArithmeticPatternKind::Multiply,
            width_a: 25,
            width_b: 18,
            has_pipeline_regs: false,
            has_accumulator: false,
        };
        assert!(mapper.infer_dsp(&pat));
    }

    #[test]
    fn mapper_infer_dsp_too_wide() {
        let mapper = Artix7Mapper;
        let pat = ArithmeticPattern {
            kind: ArithmeticPatternKind::Multiply,
            width_a: 26,
            width_b: 18,
            has_pipeline_regs: false,
            has_accumulator: false,
        };
        assert!(!mapper.infer_dsp(&pat));
    }

    #[test]
    fn mapper_limits() {
        let mapper = Artix7Mapper;
        assert_eq!(mapper.lut_input_count(), 6);
        assert_eq!(mapper.max_bram_depth(), 36_864);
        assert_eq!(mapper.max_bram_width(), 72);
        assert_eq!(mapper.max_dsp_width_a(), 25);
        assert_eq!(mapper.max_dsp_width_b(), 18);
    }

    #[test]
    fn mapper_map_to_luts() {
        let mapper = Artix7Mapper;
        let luts = mapper.map_to_luts(&CellKind::Or { width: 3 });
        assert_eq!(luts.len(), 3);

        let luts = mapper.map_to_luts(&CellKind::Bram(aion_ir::BramConfig {
            depth: 1024,
            width: 8,
        }));
        assert!(luts.is_empty());
    }

    #[test]
    fn tech_mapper_via_architecture() {
        let (a7, _) = Artix7::new("xc7a100tcsg324-1");
        let mapper = a7.tech_mapper();
        assert_eq!(mapper.lut_input_count(), 6);
        assert_eq!(mapper.max_dsp_width_a(), 25);
    }

    #[test]
    fn default_grid_methods() {
        let (a7, _) = Artix7::new("xc7a35ticpg236-1L");
        let (cols, rows) = a7.grid_dimensions();
        assert_eq!(cols, 0);
        assert_eq!(rows, 0);
        assert!(a7.get_tile(0, 0).is_none());
        assert!(a7.get_site(crate::ids::SiteId::from_raw(0)).is_none());
    }
}
