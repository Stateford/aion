//! Configuration types deserialized from `aion.toml`.

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

/// The top-level project configuration parsed from `aion.toml`.
///
/// Contains all project metadata, target definitions, pin assignments,
/// constraints, clock definitions, dependencies, and build/test/lint settings.
#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    /// Core project metadata (name, version, top module, etc.).
    pub project: ProjectMeta,
    /// Named target configurations (e.g., "de10_nano", "arty_a7").
    #[serde(default)]
    pub targets: BTreeMap<String, TargetConfig>,
    /// Global pin assignments shared across all targets.
    #[serde(default)]
    pub pins: BTreeMap<String, PinAssignment>,
    /// Global constraint configuration.
    #[serde(default)]
    pub constraints: ConstraintConfig,
    /// Named clock definitions.
    #[serde(default)]
    pub clocks: BTreeMap<String, ClockDef>,
    /// External HDL library dependencies.
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    /// Build settings (optimization level, target frequency).
    #[serde(default)]
    pub build: BuildConfig,
    /// Test settings (waveform format).
    #[serde(default)]
    pub test: TestConfig,
    /// Lint settings (deny/allow/warn rules, naming conventions).
    #[serde(default)]
    pub lint: LintConfig,
}

/// Core project metadata required in every `aion.toml`.
#[derive(Debug, Deserialize)]
pub struct ProjectMeta {
    /// The project name.
    pub name: String,
    /// The project version string.
    pub version: String,
    /// A brief description of the project.
    #[serde(default)]
    pub description: String,
    /// List of project authors.
    #[serde(default)]
    pub authors: Vec<String>,
    /// Path to the top-level HDL file.
    pub top: String,
    /// SPDX license identifier.
    #[serde(default)]
    pub license: Option<String>,
}

/// Configuration for a specific hardware target (device + board).
#[derive(Debug, Deserialize)]
pub struct TargetConfig {
    /// Full part number (e.g., "5CSEMA5F31C6", "xc7a35ticpg236-1L").
    pub device: String,
    /// Device family name (e.g., "cyclone5", "artix7").
    pub family: String,
    /// Target-specific pin assignments that override/extend global pins.
    #[serde(default)]
    pub pins: BTreeMap<String, PinAssignment>,
    /// Target-specific constraint configuration.
    #[serde(default)]
    pub constraints: Option<ConstraintConfig>,
}

/// A single pin assignment mapping a signal name to a physical pin.
#[derive(Debug, Clone, Deserialize)]
pub struct PinAssignment {
    /// The physical pin identifier (e.g., "PIN_AF14", "E3").
    pub pin: String,
    /// The I/O standard (e.g., "3.3-V LVTTL", "LVCMOS33").
    pub io_standard: String,
}

/// Constraint file paths for timing constraints.
#[derive(Debug, Default, Deserialize)]
pub struct ConstraintConfig {
    /// Paths to SDC or XDC timing constraint files.
    #[serde(default)]
    pub timing: Vec<String>,
}

/// A clock signal definition with frequency and port binding.
#[derive(Debug, Deserialize)]
pub struct ClockDef {
    /// The clock frequency as a string (e.g., "50MHz"), parsed to [`Frequency`](aion_common::Frequency).
    pub frequency: String,
    /// The port name this clock is connected to.
    pub port: String,
}

/// Specification of an external HDL library dependency.
///
/// Uses serde's untagged enum to distinguish between git, path, and registry sources.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    /// A git repository dependency.
    Git {
        /// The git repository URL.
        git: String,
        /// An optional git tag.
        tag: Option<String>,
        /// An optional git branch.
        branch: Option<String>,
        /// An optional git revision hash.
        rev: Option<String>,
    },
    /// A local filesystem path dependency.
    Path {
        /// The filesystem path to the dependency.
        path: String,
    },
    /// A registry dependency (future use).
    Registry {
        /// The version requirement string.
        version: String,
    },
}

/// Build configuration controlling optimization and synthesis.
#[derive(Debug, Default, Deserialize)]
pub struct BuildConfig {
    /// The optimization level for synthesis.
    #[serde(default)]
    pub optimization: OptLevel,
    /// Target clock frequency for timing-driven optimization.
    pub target_frequency: Option<String>,
    /// Output bitstream formats (e.g., `"sof"`, `["sof", "pof"]`, or `"all"`).
    ///
    /// Accepts either a single string or a list of strings. Defaults to an
    /// empty vec, meaning the vendor's primary format will be used.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub output_formats: Vec<String>,
}

/// Deserializes a field that can be either a single string or a list of strings.
///
/// Allows TOML config to accept both `output_formats = "sof"` (string) and
/// `output_formats = ["sof", "pof"]` (array of strings).
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string or a list of strings")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(vec![v.to_string()])
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut vec = Vec::new();
            while let Some(val) = seq.next_element::<String>()? {
                vec.push(val);
            }
            Ok(vec)
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

/// Optimization level for synthesis passes.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OptLevel {
    /// Optimize for minimum area usage.
    Area,
    /// Optimize for maximum clock speed.
    Speed,
    /// Balance between area and speed (default).
    #[default]
    Balanced,
}

/// Test configuration for simulation.
#[derive(Debug, Default, Deserialize)]
pub struct TestConfig {
    /// The waveform output format for simulation.
    #[serde(default)]
    pub waveform_format: WaveformFormat,
}

/// Waveform output format for simulation dumps.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WaveformFormat {
    /// Value Change Dump (IEEE 1364).
    Vcd,
    /// Fast Signal Trace (GTKWave native format, default).
    #[default]
    Fst,
    /// GHDL Waveform format.
    Ghw,
}

/// Lint configuration controlling which rules are enabled or disabled.
#[derive(Debug, Default, Deserialize)]
pub struct LintConfig {
    /// Rule codes to treat as errors.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Rule codes to suppress.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Rule codes to treat as warnings.
    #[serde(default)]
    pub warn: Vec<String>,
    /// Naming convention configuration.
    #[serde(default)]
    pub naming: Option<NamingConfig>,
}

/// Naming convention rules for HDL identifiers.
#[derive(Debug, Deserialize)]
pub struct NamingConfig {
    /// Convention for module/entity names.
    pub module: Option<NamingConvention>,
    /// Convention for signal/wire names.
    pub signal: Option<NamingConvention>,
    /// Convention for parameter/generic names.
    pub parameter: Option<NamingConvention>,
    /// Convention for constant names.
    pub constant: Option<NamingConvention>,
}

/// A naming convention for identifiers.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NamingConvention {
    /// `snake_case` — lowercase with underscores.
    SnakeCase,
    /// `camelCase` — lowercase first word, capitalized subsequent words.
    CamelCase,
    /// `UPPER_SNAKE_CASE` — uppercase with underscores.
    UpperSnakeCase,
    /// `PascalCase` — capitalized words with no separator.
    PascalCase,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_config_from_str;

    #[test]
    fn naming_convention_all_variants() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[lint.naming]
module = "snake_case"
signal = "camel_case"
parameter = "upper_snake_case"
constant = "pascal_case"
"#;
        let config = load_config_from_str(toml).unwrap();
        let naming = config.lint.naming.unwrap();
        assert_eq!(naming.module, Some(NamingConvention::SnakeCase));
        assert_eq!(naming.signal, Some(NamingConvention::CamelCase));
        assert_eq!(naming.parameter, Some(NamingConvention::UpperSnakeCase));
        assert_eq!(naming.constant, Some(NamingConvention::PascalCase));
    }

    #[test]
    fn opt_level_all_variants() {
        for (input, expected) in [
            ("area", OptLevel::Area),
            ("speed", OptLevel::Speed),
            ("balanced", OptLevel::Balanced),
        ] {
            let toml = format!(
                r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[build]
optimization = "{input}"
"#
            );
            let config = load_config_from_str(&toml).unwrap();
            assert_eq!(config.build.optimization, expected);
        }
    }

    #[test]
    fn waveform_format_all_variants() {
        for (input, expected) in [
            ("vcd", WaveformFormat::Vcd),
            ("fst", WaveformFormat::Fst),
            ("ghw", WaveformFormat::Ghw),
        ] {
            let toml = format!(
                r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[test]
waveform_format = "{input}"
"#
            );
            let config = load_config_from_str(&toml).unwrap();
            assert_eq!(config.test.waveform_format, expected);
        }
    }

    #[test]
    fn output_formats_single_string() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[build]
output_formats = "sof"
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.build.output_formats, vec!["sof"]);
    }

    #[test]
    fn output_formats_list() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[build]
output_formats = ["sof", "pof"]
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.build.output_formats, vec!["sof", "pof"]);
    }

    #[test]
    fn output_formats_all_string() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[build]
output_formats = "all"
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.build.output_formats, vec!["all"]);
    }

    #[test]
    fn output_formats_default_empty() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[build]
optimization = "balanced"
"#;
        let config = load_config_from_str(toml).unwrap();
        assert!(config.build.output_formats.is_empty());
    }

    #[test]
    fn dependency_spec_registry() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[dependencies.some_lib]
version = "1.2.3"
"#;
        let config = load_config_from_str(toml).unwrap();
        match &config.dependencies["some_lib"] {
            DependencySpec::Registry { version } => {
                assert_eq!(version, "1.2.3");
            }
            _ => panic!("expected Registry dependency"),
        }
    }
}
