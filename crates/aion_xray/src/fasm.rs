//! FASM (FPGA Assembly) text emitter for debugging and validation.
//!
//! FASM is a simple text format where each line represents an enabled feature
//! in the FPGA configuration. It can be cross-validated with Project X-Ray's
//! Python tools (e.g., `fasm2frames`) to verify bitstream correctness.
//!
//! # Format
//!
//! ```text
//! CLBLL_L_X16Y149.SLICEL_X0.ALUT.INIT[63:0] = 64'h8000000000000000
//! CLBLL_L_X16Y149.SLICEL_X0.AFF.ZINI
//! INT_L_X16Y149.NL1BEG1.SS2END0
//! ```

use std::fmt::Write;

/// A single FASM feature annotation.
///
/// Represents one enabled feature in the device configuration, with its
/// tile, feature path, and optional value/range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FasmFeature {
    /// The tile name (e.g., "CLBLL_L_X16Y149").
    pub tile: String,
    /// The feature path within the tile (e.g., `SLICEL_X0.ALUT.INIT[63:0]`).
    pub feature: String,
    /// Optional value for the feature (e.g., for LUT INIT vectors).
    pub value: Option<u64>,
}

impl FasmFeature {
    /// Creates a new FASM feature with no value.
    pub fn new(tile: &str, feature: &str) -> Self {
        Self {
            tile: tile.to_string(),
            feature: feature.to_string(),
            value: None,
        }
    }

    /// Creates a new FASM feature with a value.
    pub fn with_value(tile: &str, feature: &str, value: u64) -> Self {
        Self {
            tile: tile.to_string(),
            feature: feature.to_string(),
            value: Some(value),
        }
    }

    /// Formats this feature as a FASM line.
    pub fn to_fasm_line(&self) -> String {
        match self.value {
            Some(v) => format!("{}.{} = {v:#x}", self.tile, self.feature),
            None => format!("{}.{}", self.tile, self.feature),
        }
    }
}

/// A collection of FASM features forming a complete device configuration.
#[derive(Debug, Clone, Default)]
pub struct FasmOutput {
    /// The features, in the order they were added.
    features: Vec<FasmFeature>,
}

impl FasmOutput {
    /// Creates a new empty FASM output.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a feature with no value.
    pub fn add_feature(&mut self, tile: &str, feature: &str) {
        self.features.push(FasmFeature::new(tile, feature));
    }

    /// Adds a feature with a value.
    pub fn add_feature_with_value(&mut self, tile: &str, feature: &str, value: u64) {
        self.features
            .push(FasmFeature::with_value(tile, feature, value));
    }

    /// Adds a pre-constructed feature.
    pub fn add(&mut self, feature: FasmFeature) {
        self.features.push(feature);
    }

    /// Returns the number of features.
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Returns whether no features have been added.
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Returns an iterator over the features.
    pub fn features(&self) -> &[FasmFeature] {
        &self.features
    }

    /// Renders all features as a FASM text string.
    ///
    /// Features are sorted by tile name then feature name for deterministic output.
    pub fn render(&self) -> String {
        let mut sorted = self.features.clone();
        sorted.sort_by(|a, b| a.tile.cmp(&b.tile).then_with(|| a.feature.cmp(&b.feature)));

        let mut output = String::new();
        for f in &sorted {
            writeln!(output, "{}", f.to_fasm_line()).unwrap();
        }
        output
    }

    /// Renders features without sorting (preserves insertion order).
    pub fn render_unsorted(&self) -> String {
        let mut output = String::new();
        for f in &self.features {
            writeln!(output, "{}", f.to_fasm_line()).unwrap();
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_no_value() {
        let f = FasmFeature::new("CLBLL_L_X0Y0", "SLICEL_X0.AFF.ZINI");
        assert_eq!(f.to_fasm_line(), "CLBLL_L_X0Y0.SLICEL_X0.AFF.ZINI");
        assert!(f.value.is_none());
    }

    #[test]
    fn feature_with_value() {
        let f = FasmFeature::with_value("CLBLL_L_X0Y0", "SLICEL_X0.ALUT.INIT[63:0]", 0x8000);
        assert_eq!(
            f.to_fasm_line(),
            "CLBLL_L_X0Y0.SLICEL_X0.ALUT.INIT[63:0] = 0x8000"
        );
    }

    #[test]
    fn feature_with_zero_value() {
        let f = FasmFeature::with_value("TILE", "FEAT", 0);
        assert_eq!(f.to_fasm_line(), "TILE.FEAT = 0x0");
    }

    #[test]
    fn feature_equality() {
        let a = FasmFeature::new("T", "F");
        let b = FasmFeature::new("T", "F");
        assert_eq!(a, b);
    }

    #[test]
    fn feature_inequality_tile() {
        let a = FasmFeature::new("T1", "F");
        let b = FasmFeature::new("T2", "F");
        assert_ne!(a, b);
    }

    #[test]
    fn fasm_output_empty() {
        let out = FasmOutput::new();
        assert!(out.is_empty());
        assert_eq!(out.len(), 0);
        assert_eq!(out.render(), "");
    }

    #[test]
    fn fasm_output_add_features() {
        let mut out = FasmOutput::new();
        out.add_feature("TILE_A", "FEAT1");
        out.add_feature("TILE_B", "FEAT2");
        assert_eq!(out.len(), 2);
        assert!(!out.is_empty());
    }

    #[test]
    fn fasm_output_add_with_value() {
        let mut out = FasmOutput::new();
        out.add_feature_with_value("TILE", "INIT", 0xFF);
        assert_eq!(out.len(), 1);
        assert_eq!(out.features()[0].value, Some(0xFF));
    }

    #[test]
    fn fasm_output_sorted_render() {
        let mut out = FasmOutput::new();
        out.add_feature("TILE_B", "FEAT2");
        out.add_feature("TILE_A", "FEAT1");
        out.add_feature("TILE_A", "FEAT0");

        let rendered = out.render();
        let lines: Vec<&str> = rendered.trim().lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "TILE_A.FEAT0");
        assert_eq!(lines[1], "TILE_A.FEAT1");
        assert_eq!(lines[2], "TILE_B.FEAT2");
    }

    #[test]
    fn fasm_output_unsorted_render() {
        let mut out = FasmOutput::new();
        out.add_feature("TILE_B", "FEAT2");
        out.add_feature("TILE_A", "FEAT1");

        let rendered = out.render_unsorted();
        let lines: Vec<&str> = rendered.trim().lines().collect();
        assert_eq!(lines[0], "TILE_B.FEAT2");
        assert_eq!(lines[1], "TILE_A.FEAT1");
    }

    #[test]
    fn fasm_output_add_prebuilt() {
        let mut out = FasmOutput::new();
        let f = FasmFeature::with_value("T", "F", 42);
        out.add(f.clone());
        assert_eq!(out.features()[0], f);
    }

    #[test]
    fn fasm_output_mixed_features() {
        let mut out = FasmOutput::new();
        out.add_feature("CLBLL_L_X0Y0", "SLICEL_X0.AFF.ZINI");
        out.add_feature_with_value("CLBLL_L_X0Y0", "SLICEL_X0.ALUT.INIT[63:0]", 0x8000);
        out.add_feature("INT_L_X0Y0", "NL1BEG1.SS2END0");

        let rendered = out.render();
        assert!(rendered.contains("SLICEL_X0.AFF.ZINI"));
        assert!(rendered.contains("= 0x8000"));
        assert!(rendered.contains("NL1BEG1.SS2END0"));
    }

    #[test]
    fn fasm_feature_clone() {
        let f = FasmFeature::new("T", "F");
        let f2 = f.clone();
        assert_eq!(f, f2);
    }

    #[test]
    fn fasm_output_features_accessor() {
        let mut out = FasmOutput::new();
        out.add_feature("T1", "F1");
        out.add_feature("T2", "F2");
        let features = out.features();
        assert_eq!(features.len(), 2);
        assert_eq!(features[0].tile, "T1");
        assert_eq!(features[1].tile, "T2");
    }
}
