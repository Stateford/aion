//! Xilinx (AMD) FPGA device family models.
//!
//! This module provides architecture models for Xilinx 7-series FPGA families.
//! Currently supports Artix-7 with hardcoded device parameters.

pub mod artix7;

use serde::{Deserialize, Serialize};

/// Xilinx (AMD) FPGA device families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum XilinxFamily {
    /// Artix-7 — low-cost, low-power 28nm FPGA.
    Artix7,
    /// Kintex-7 — mid-range 28nm FPGA with high DSP density.
    Kintex7,
    /// Spartan-7 — ultra-low-cost 28nm FPGA.
    Spartan7,
    /// Zynq-7000 — FPGA + ARM Cortex-A9 SoC.
    Zynq7000,
}

impl XilinxFamily {
    /// Returns the human-readable name of this family.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Artix7 => "Artix-7",
            Self::Kintex7 => "Kintex-7",
            Self::Spartan7 => "Spartan-7",
            Self::Zynq7000 => "Zynq-7000",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names() {
        assert_eq!(XilinxFamily::Artix7.name(), "Artix-7");
        assert_eq!(XilinxFamily::Kintex7.name(), "Kintex-7");
        assert_eq!(XilinxFamily::Spartan7.name(), "Spartan-7");
        assert_eq!(XilinxFamily::Zynq7000.name(), "Zynq-7000");
    }

    #[test]
    fn family_equality() {
        assert_eq!(XilinxFamily::Artix7, XilinxFamily::Artix7);
        assert_ne!(XilinxFamily::Artix7, XilinxFamily::Kintex7);
    }

    #[test]
    fn family_serde_roundtrip() {
        let f = XilinxFamily::Artix7;
        let json = serde_json::to_string(&f).unwrap();
        let restored: XilinxFamily = serde_json::from_str(&json).unwrap();
        assert_eq!(f, restored);
    }
}
