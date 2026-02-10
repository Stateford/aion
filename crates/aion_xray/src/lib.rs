//! Project X-Ray database integration for Xilinx 7-series FPGAs.
//!
//! This crate loads and parses the [Project X-Ray](https://github.com/f4pga/prjxray)
//! open-source database to provide real device data for Xilinx Artix-7 FPGAs.
//! It implements the [`Architecture`](aion_arch::Architecture) and
//! [`ConfigBitDatabase`](aion_bitstream::config_bits::ConfigBitDatabase) traits
//! using actual tile grids, segbit mappings, and routing connectivity from the
//! X-Ray database.
//!
//! # Database files
//!
//! The X-Ray database contains several file types per device:
//!
//! - `tilegrid.json` — tile positions, frame base addresses, and site assignments
//! - `segbits_*.db` — feature-to-config-bit mappings per tile type
//! - `tile_type_*.json` — PIP definitions and site pin-to-wire mappings
//!
//! # Usage
//!
//! Point `AION_XRAY_DB` to a clone of the `prjxray-db` repository, or set
//! `xray_db_path` in `aion.toml`:
//!
//! ```text
//! [device]
//! family = "artix7"
//! device = "xc7a35t"
//! xray_db_path = "/path/to/prjxray-db"
//! ```

#![warn(missing_docs)]

pub mod arch_impl;
pub mod config_db_impl;
pub mod db;
pub mod fasm;
pub mod segbits;
pub mod tile_type;
pub mod tilegrid;

pub use arch_impl::Artix7XRay;
pub use db::XRayDatabase;
