//! Xilinx bitstream generation.
//!
//! Provides the `XilinxBitstreamGenerator` which converts a placed-and-routed
//! `PnrNetlist` into Xilinx BIT format bitstream files. Uses
//! `SimplifiedXilinxDb` for cell-to-config-bit mapping and the BIT format
//! writer for file output.

pub mod bit;
pub mod config_db;

use crate::config_bits::{ConfigBitDatabase, ConfigImage};
use crate::{compute_checksum, Bitstream, BitstreamFormat, BitstreamGenerator};
use aion_arch::Architecture;
use aion_common::{AionResult, InternalError};
use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink};
use aion_pnr::{PnrCellType, PnrNetlist, RouteResource};
use aion_source::Span;
use config_db::SimplifiedXilinxDb;

/// Bitstream generator for Xilinx FPGA devices.
///
/// Supports Artix-7 and other Xilinx 7-series device families.
/// Generates BIT format bitstreams.
#[derive(Debug)]
pub struct XilinxBitstreamGenerator {
    /// Supported output formats.
    formats: Vec<BitstreamFormat>,
}

impl Default for XilinxBitstreamGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl XilinxBitstreamGenerator {
    /// Creates a new Xilinx bitstream generator.
    pub fn new() -> Self {
        Self {
            formats: vec![BitstreamFormat::Bit],
        }
    }
}

impl BitstreamGenerator for XilinxBitstreamGenerator {
    fn generate(
        &self,
        netlist: &PnrNetlist,
        arch: &dyn Architecture,
        format: BitstreamFormat,
        sink: &DiagnosticSink,
    ) -> AionResult<Bitstream> {
        if !self.formats.contains(&format) {
            return Err(InternalError::new(format!(
                "Xilinx generator does not support format {}",
                format
            )));
        }

        let config_db = SimplifiedXilinxDb;
        let config = assemble_xilinx_config(netlist, &config_db, sink);

        let data = bit::write_bit(&config, arch.device_name(), "aion_design");

        let checksum = compute_checksum(&data);

        Ok(Bitstream {
            data,
            format,
            device: arch.device_name().to_string(),
            checksum,
        })
    }

    fn supported_formats(&self) -> &[BitstreamFormat] {
        &self.formats
    }
}

/// Assembles configuration bits from a PnR netlist using the Xilinx config database.
///
/// Iterates over all placed cells and routed nets, mapping each to physical
/// configuration bits. Unplaced cells and stub-routed nets emit warnings
/// and are skipped.
fn assemble_xilinx_config(
    netlist: &PnrNetlist,
    config_db: &dyn ConfigBitDatabase,
    sink: &DiagnosticSink,
) -> ConfigImage {
    let mut image = ConfigImage::new(config_db.frame_word_count(), config_db.total_frame_count());

    // Process cells
    for cell in &netlist.cells {
        let site = match cell.placement {
            Some(s) => s,
            None => {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::new(Category::Vendor, 501),
                    format!("cell '{}' is not placed, skipping config bits", cell.name),
                    Span::DUMMY,
                ));
                continue;
            }
        };

        let bits = match &cell.cell_type {
            PnrCellType::Lut { inputs, init } => config_db.lut_config_bits(site, init, *inputs),
            PnrCellType::Dff => config_db.ff_config_bits(site),
            PnrCellType::Iobuf {
                direction,
                standard,
            } => config_db.iobuf_config_bits(site, *direction, standard),
            PnrCellType::Bram(cfg) => config_db.bram_config_bits(site, cfg.width, cfg.depth),
            PnrCellType::Dsp(cfg) => config_db.dsp_config_bits(site, cfg.width_a, cfg.width_b),
            PnrCellType::Carry | PnrCellType::Pll(_) => config_db.ff_config_bits(site),
        };

        for bit in bits {
            image.set_bit(bit);
        }
    }

    // Process nets
    for net in &netlist.nets {
        match &net.routing {
            Some(route_tree) => {
                let pips = route_tree.pips_used();
                if pips.is_empty() {
                    if matches!(route_tree.root.resource, RouteResource::Direct) {
                        sink.emit(Diagnostic::warning(
                            DiagnosticCode::new(Category::Vendor, 502),
                            format!(
                                "net '{}' has stub routing, PIP config bits skipped",
                                net.name
                            ),
                            Span::DUMMY,
                        ));
                    }
                    continue;
                }
                for pip in pips {
                    let bits = config_db.pip_config_bits(pip);
                    for bit in bits {
                        image.set_bit(bit);
                    }
                }
            }
            None => {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::new(Category::Vendor, 502),
                    format!("net '{}' is not routed, PIP config bits skipped", net.name),
                    Span::DUMMY,
                ));
            }
        }
    }

    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_arch::ids::SiteId;
    use aion_common::LogicVec;
    use aion_ir::PortDirection;
    use aion_pnr::{PnrCell, PnrCellId, PnrNet, PnrNetId, PnrPin, PnrPinId, RouteTree};

    fn make_test_netlist() -> PnrNetlist {
        let mut nl = PnrNetlist::new();
        let cell_id = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 6,
                init: LogicVec::all_zero(64),
            },
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });
        let pin = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: cell_id,
            net: None,
        });
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "net_0".into(),
            driver: pin,
            sinks: vec![],
            routing: Some(RouteTree::stub()),
            timing_critical: false,
        });
        nl
    }

    #[test]
    fn supported_formats() {
        let gen = XilinxBitstreamGenerator::new();
        let fmts = gen.supported_formats();
        assert!(fmts.contains(&BitstreamFormat::Bit));
        assert!(!fmts.contains(&BitstreamFormat::Sof));
    }

    #[test]
    fn generate_bit_empty_netlist() {
        let gen = XilinxBitstreamGenerator::new();
        let nl = PnrNetlist::new();
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink = DiagnosticSink::new();
        let bs = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink)
            .unwrap();
        assert_eq!(bs.format, BitstreamFormat::Bit);
        assert!(!bs.data.is_empty());
        assert!(bs.device.starts_with("xc7a35t"));
    }

    #[test]
    fn generate_bit_with_cells() {
        let gen = XilinxBitstreamGenerator::new();
        let nl = make_test_netlist();
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink = DiagnosticSink::new();
        let bs = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink)
            .unwrap();
        assert!(!bs.data.is_empty());
        assert_ne!(bs.checksum, 0);
    }

    #[test]
    fn generate_sof_unsupported() {
        let gen = XilinxBitstreamGenerator::new();
        let nl = PnrNetlist::new();
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink = DiagnosticSink::new();
        let result = gen.generate(&nl, arch.as_ref(), BitstreamFormat::Sof, &sink);
        assert!(result.is_err());
    }

    #[test]
    fn unplaced_cell_warning() {
        let gen = XilinxBitstreamGenerator::new();
        let mut nl = PnrNetlist::new();
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "unplaced_lut".into(),
            cell_type: PnrCellType::Dff,
            placement: None,
            is_fixed: false,
        });
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink = DiagnosticSink::new();
        let _bs = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink)
            .unwrap();
        let diags = sink.diagnostics();
        assert!(diags.iter().any(|d| d.code.number == 501));
    }

    #[test]
    fn stub_routing_warning() {
        let gen = XilinxBitstreamGenerator::new();
        let nl = make_test_netlist();
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink = DiagnosticSink::new();
        let _bs = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink)
            .unwrap();
        let diags = sink.diagnostics();
        assert!(diags.iter().any(|d| d.code.number == 502));
    }

    #[test]
    fn unrouted_net_warning() {
        let gen = XilinxBitstreamGenerator::new();
        let mut nl = PnrNetlist::new();
        let cell_id = nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });
        let pin = nl.add_pin(PnrPin {
            id: PnrPinId::from_raw(0),
            name: "O".into(),
            direction: PortDirection::Output,
            cell: cell_id,
            net: None,
        });
        nl.add_net(PnrNet {
            id: PnrNetId::from_raw(0),
            name: "unrouted_net".into(),
            driver: pin,
            sinks: vec![],
            routing: None,
            timing_critical: false,
        });
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink = DiagnosticSink::new();
        let _bs = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink)
            .unwrap();
        let diags = sink.diagnostics();
        assert!(diags.iter().any(|d| d.code.number == 502));
    }

    #[test]
    fn deterministic_output() {
        let gen = XilinxBitstreamGenerator::new();
        let nl = make_test_netlist();
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink1 = DiagnosticSink::new();
        let sink2 = DiagnosticSink::new();
        let a = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink1)
            .unwrap();
        let b = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink2)
            .unwrap();
        assert_eq!(a.data, b.data);
        assert_eq!(a.checksum, b.checksum);
    }

    #[test]
    fn multi_cell_types() {
        let gen = XilinxBitstreamGenerator::new();
        let mut nl = PnrNetlist::new();

        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(0),
            name: "lut_0".into(),
            cell_type: PnrCellType::Lut {
                inputs: 6,
                init: LogicVec::all_zero(64),
            },
            placement: Some(SiteId::from_raw(0)),
            is_fixed: false,
        });
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(1),
            name: "ff_0".into(),
            cell_type: PnrCellType::Dff,
            placement: Some(SiteId::from_raw(1)),
            is_fixed: false,
        });
        nl.add_cell(PnrCell {
            id: PnrCellId::from_raw(2),
            name: "io_0".into(),
            cell_type: PnrCellType::Iobuf {
                direction: PortDirection::Output,
                standard: "LVCMOS33".into(),
            },
            placement: Some(SiteId::from_raw(2)),
            is_fixed: true,
        });

        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink = DiagnosticSink::new();
        let bs = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink)
            .unwrap();
        assert!(!bs.data.is_empty());
        assert_ne!(bs.checksum, 0);
    }

    #[test]
    fn checksum_nonzero() {
        let gen = XilinxBitstreamGenerator::new();
        let nl = make_test_netlist();
        let arch = aion_arch::load_architecture("artix7", "xc7a35t").unwrap();
        let sink = DiagnosticSink::new();
        let bs = gen
            .generate(&nl, arch.as_ref(), BitstreamFormat::Bit, &sink)
            .unwrap();
        assert_ne!(bs.checksum, 0);
    }
}
