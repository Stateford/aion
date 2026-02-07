//! Parsing and validation of `aion.toml` project configuration files.
//!
//! This crate reads the project configuration file and produces a strongly-typed
//! [`ProjectConfig`] with target resolution, pin merging, and constraint handling.

#![warn(missing_docs)]

pub mod error;
pub mod loader;
pub mod resolve;
pub mod types;

pub use error::ConfigError;
pub use loader::{load_config, load_config_from_str};
pub use resolve::{resolve_target, ResolvedTarget};
pub use types::*;
