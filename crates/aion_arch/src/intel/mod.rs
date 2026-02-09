//! Intel FPGA device family models.
//!
//! This module provides architecture models for Intel (formerly Altera) FPGA
//! families. Currently supports Cyclone IV E and Cyclone V with hardcoded
//! device parameters.

pub mod cyclone_iv;
pub mod cyclone_v;

use serde::{Deserialize, Serialize};

/// Intel FPGA device families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntelFamily {
    /// Cyclone IV E — 60nm FPGA with 4-input LEs, popular for hobbyist boards.
    CycloneIV,
    /// Cyclone V — mid-range 28nm FPGA with ALM-based logic.
    CycloneV,
    /// Cyclone 10 LP — low-power variant of the Cyclone family.
    Cyclone10Lp,
    /// MAX 10 — non-volatile FPGA with integrated flash.
    Max10,
    /// Stratix V — high-performance 28nm FPGA.
    StratixV,
}

impl IntelFamily {
    /// Returns the human-readable name of this family.
    pub fn name(&self) -> &'static str {
        match self {
            Self::CycloneIV => "Cyclone IV E",
            Self::CycloneV => "Cyclone V",
            Self::Cyclone10Lp => "Cyclone 10 LP",
            Self::Max10 => "MAX 10",
            Self::StratixV => "Stratix V",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names() {
        assert_eq!(IntelFamily::CycloneIV.name(), "Cyclone IV E");
        assert_eq!(IntelFamily::CycloneV.name(), "Cyclone V");
        assert_eq!(IntelFamily::Cyclone10Lp.name(), "Cyclone 10 LP");
        assert_eq!(IntelFamily::Max10.name(), "MAX 10");
        assert_eq!(IntelFamily::StratixV.name(), "Stratix V");
    }

    #[test]
    fn family_equality() {
        assert_eq!(IntelFamily::CycloneV, IntelFamily::CycloneV);
        assert_ne!(IntelFamily::CycloneV, IntelFamily::StratixV);
    }

    #[test]
    fn family_serde_roundtrip() {
        let f = IntelFamily::CycloneV;
        let json = serde_json::to_string(&f).unwrap();
        let restored: IntelFamily = serde_json::from_str(&json).unwrap();
        assert_eq!(f, restored);
    }
}
