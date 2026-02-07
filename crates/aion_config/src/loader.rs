//! Configuration file loading and validation.

use crate::error::ConfigError;
use crate::types::ProjectConfig;
use std::path::Path;

/// Loads and validates an `aion.toml` configuration from a project directory.
///
/// Reads `<project_dir>/aion.toml`, parses it, and validates required fields.
pub fn load_config(project_dir: &Path) -> Result<ProjectConfig, ConfigError> {
    let config_path = project_dir.join("aion.toml");
    let content = std::fs::read_to_string(&config_path)?;
    load_config_from_str(&content)
}

/// Parses and validates an `aion.toml` configuration from a string.
///
/// Useful for testing without filesystem dependencies.
pub fn load_config_from_str(content: &str) -> Result<ProjectConfig, ConfigError> {
    let config: ProjectConfig =
        toml::from_str(content).map_err(|e| ConfigError::ParseError(e.to_string()))?;
    validate_config(&config)?;
    Ok(config)
}

/// Validates that required fields are present and configuration values are consistent.
fn validate_config(config: &ProjectConfig) -> Result<(), ConfigError> {
    if config.project.name.is_empty() {
        return Err(ConfigError::MissingField("project.name".to_string()));
    }
    if config.project.top.is_empty() {
        return Err(ConfigError::MissingField("project.top".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[project]
name = "blinky"
version = "0.1.0"
top = "src/top.vhd"
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.project.name, "blinky");
        assert_eq!(config.project.version, "0.1.0");
        assert_eq!(config.project.top, "src/top.vhd");
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[project]
name = "blinky"
version = "0.1.0"
description = "LED blinker"
authors = ["Alice", "Bob"]
top = "src/top.vhd"
license = "MIT"

[targets.de10_nano]
device = "5CSEMA5F31C6"
family = "cyclone5"

[targets.de10_nano.pins.led0]
pin = "PIN_W15"
io_standard = "3.3-V LVTTL"

[pins.clk]
pin = "PIN_AF14"
io_standard = "3.3-V LVTTL"

[constraints]
timing = ["constraints/timing.sdc"]

[clocks.sys_clk]
frequency = "50MHz"
port = "clk"

[dependencies.uart_lib]
git = "https://github.com/example/uart.git"
tag = "v1.0"

[build]
optimization = "speed"
target_frequency = "100MHz"

[test]
waveform_format = "vcd"

[lint]
deny = ["W101"]
allow = ["C201"]
warn = ["W102"]

[lint.naming]
module = "snake_case"
signal = "snake_case"
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.project.name, "blinky");
        assert_eq!(config.project.authors.len(), 2);
        assert!(config.targets.contains_key("de10_nano"));
        assert!(config.pins.contains_key("clk"));
        assert_eq!(config.constraints.timing.len(), 1);
        assert!(config.clocks.contains_key("sys_clk"));
        assert!(config.dependencies.contains_key("uart_lib"));
        assert_eq!(config.build.optimization, crate::types::OptLevel::Speed);
        assert_eq!(
            config.test.waveform_format,
            crate::types::WaveformFormat::Vcd
        );
        assert_eq!(config.lint.deny, vec!["W101"]);
    }

    #[test]
    fn missing_name_errors() {
        let toml = r#"
[project]
name = ""
version = "0.1.0"
top = "src/top.vhd"
"#;
        let err = load_config_from_str(toml).unwrap_err();
        assert!(matches!(err, ConfigError::MissingField(_)));
    }

    #[test]
    fn missing_top_errors() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = ""
"#;
        let err = load_config_from_str(toml).unwrap_err();
        assert!(matches!(err, ConfigError::MissingField(_)));
    }

    #[test]
    fn invalid_toml_errors() {
        let toml = "this is not valid toml {{{}}}";
        let err = load_config_from_str(toml).unwrap_err();
        assert!(matches!(err, ConfigError::ParseError(_)));
    }

    #[test]
    fn default_values() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.build.optimization, crate::types::OptLevel::Balanced);
        assert_eq!(
            config.test.waveform_format,
            crate::types::WaveformFormat::Fst
        );
        assert!(config.targets.is_empty());
        assert!(config.pins.is_empty());
        assert!(config.dependencies.is_empty());
    }

    #[test]
    fn dependency_spec_git_with_tag() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[dependencies.uart]
git = "https://github.com/example/uart.git"
tag = "v1.0"
"#;
        let config = load_config_from_str(toml).unwrap();
        match &config.dependencies["uart"] {
            crate::types::DependencySpec::Git { git, tag, .. } => {
                assert_eq!(git, "https://github.com/example/uart.git");
                assert_eq!(tag.as_deref(), Some("v1.0"));
            }
            _ => panic!("expected Git dependency"),
        }
    }

    #[test]
    fn dependency_spec_path() {
        let toml = r#"
[project]
name = "test"
version = "0.1.0"
top = "src/top.vhd"

[dependencies.local_lib]
path = "../libs/local_lib"
"#;
        let config = load_config_from_str(toml).unwrap();
        match &config.dependencies["local_lib"] {
            crate::types::DependencySpec::Path { path } => {
                assert_eq!(path, "../libs/local_lib");
            }
            _ => panic!("expected Path dependency"),
        }
    }

    #[test]
    fn io_error_from_nonexistent_dir() {
        let err = load_config(Path::new("/nonexistent/dir")).unwrap_err();
        assert!(matches!(err, ConfigError::IoError(_)));
    }
}
