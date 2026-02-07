//! Target resolution: merging global and target-specific configurations.

use crate::error::ConfigError;
use crate::types::{BuildConfig, ConstraintConfig, PinAssignment, ProjectConfig};
use std::collections::BTreeMap;

/// A fully resolved target configuration with global and target-specific settings merged.
///
/// Global pin assignments serve as the base, and target-specific pins overlay on top.
/// Target-specific constraints override global constraints entirely.
#[derive(Debug)]
pub struct ResolvedTarget {
    /// The target name.
    pub name: String,
    /// The full device part number.
    pub device: String,
    /// The device family name.
    pub family: String,
    /// Merged pin assignments (global base + target overlay).
    pub pins: BTreeMap<String, PinAssignment>,
    /// Resolved constraint configuration (target overrides global).
    pub constraints: ConstraintConfig,
    /// Build configuration.
    pub build: BuildConfig,
}

/// Resolves a named target by merging global settings with target-specific overrides.
///
/// Pin assignments are merged: global pins form the base, and target-specific pins
/// override any matching entries. Constraints from the target replace global constraints
/// entirely if present.
pub fn resolve_target(
    config: &ProjectConfig,
    target_name: &str,
) -> Result<ResolvedTarget, ConfigError> {
    let target = config
        .targets
        .get(target_name)
        .ok_or_else(|| ConfigError::UnknownTarget(target_name.to_string()))?;

    // Merge pins: start with global, overlay target-specific
    let mut pins = config.pins.clone();
    for (name, assignment) in &target.pins {
        pins.insert(name.clone(), assignment.clone());
    }

    // Constraints: target overrides global if present
    let constraints = target
        .constraints
        .clone()
        .unwrap_or_else(|| config.constraints.clone());

    Ok(ResolvedTarget {
        name: target_name.to_string(),
        device: target.device.clone(),
        family: target.family.clone(),
        pins,
        constraints,
        build: BuildConfig {
            optimization: config.build.optimization.clone(),
            target_frequency: config.build.target_frequency.clone(),
        },
    })
}

// Implement Clone for OptLevel manually since it's needed
impl Clone for crate::types::OptLevel {
    fn clone(&self) -> Self {
        match self {
            crate::types::OptLevel::Area => crate::types::OptLevel::Area,
            crate::types::OptLevel::Speed => crate::types::OptLevel::Speed,
            crate::types::OptLevel::Balanced => crate::types::OptLevel::Balanced,
        }
    }
}

impl Clone for ConstraintConfig {
    fn clone(&self) -> Self {
        Self {
            timing: self.timing.clone(),
        }
    }
}

impl Clone for BuildConfig {
    fn clone(&self) -> Self {
        Self {
            optimization: self.optimization.clone(),
            target_frequency: self.target_frequency.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_config_from_str;

    #[test]
    fn resolve_basic_target() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[targets.de10_nano]
device = "5CSEMA5F31C6"
family = "cyclone5"
"#;
        let config = load_config_from_str(toml).unwrap();
        let resolved = resolve_target(&config, "de10_nano").unwrap();
        assert_eq!(resolved.device, "5CSEMA5F31C6");
        assert_eq!(resolved.family, "cyclone5");
    }

    #[test]
    fn unknown_target_errors() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"
"#;
        let config = load_config_from_str(toml).unwrap();
        let err = resolve_target(&config, "nonexistent").unwrap_err();
        assert!(matches!(err, ConfigError::UnknownTarget(_)));
    }

    #[test]
    fn pin_merging() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[pins.clk]
pin = "PIN_AF14"
io_standard = "3.3-V LVTTL"

[pins.led0]
pin = "GLOBAL_LED"
io_standard = "3.3-V LVTTL"

[targets.board_a]
device = "5CSEMA5F31C6"
family = "cyclone5"

[targets.board_a.pins.led0]
pin = "PIN_W15"
io_standard = "3.3-V LVTTL"
"#;
        let config = load_config_from_str(toml).unwrap();
        let resolved = resolve_target(&config, "board_a").unwrap();

        // Global clk pin preserved
        assert_eq!(resolved.pins["clk"].pin, "PIN_AF14");
        // Target-specific led0 overrides global
        assert_eq!(resolved.pins["led0"].pin, "PIN_W15");
    }

    #[test]
    fn constraint_override() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[constraints]
timing = ["global.sdc"]

[targets.board_a]
device = "5CSEMA5F31C6"
family = "cyclone5"

[targets.board_a.constraints]
timing = ["board_a.sdc"]

[targets.board_b]
device = "xc7a35t"
family = "artix7"
"#;
        let config = load_config_from_str(toml).unwrap();

        // board_a overrides constraints
        let a = resolve_target(&config, "board_a").unwrap();
        assert_eq!(a.constraints.timing, vec!["board_a.sdc"]);

        // board_b falls back to global constraints
        let b = resolve_target(&config, "board_b").unwrap();
        assert_eq!(b.constraints.timing, vec!["global.sdc"]);
    }
}
