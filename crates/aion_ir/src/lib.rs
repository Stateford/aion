//! AionIR — the unified intermediate representation for the Aion FPGA toolchain.
//!
//! This crate defines the core IR types including [`Design`], [`Module`], [`Signal`],
//! [`Cell`], and [`Process`] that serve as the lingua franca between all pipeline
//! stages after elaboration.
//!
//! # Architecture
//!
//! - **[`Arena`]** provides dense, ID-indexed storage for all IR entities.
//! - **Opaque IDs** ([`ModuleId`], [`SignalId`], etc.) are `Copy` + `Hash` for cheap references.
//! - **[`TypeDb`]** interns all hardware types for O(1) equality checks.
//! - **[`SourceMap`]** traces every IR entity back to its original source location.
//!
//! All types derive `Serialize`/`Deserialize` for `bincode` stage boundaries.

#![warn(missing_docs)]

pub mod arena;
pub mod cell;
pub mod const_value;
pub mod design;
pub mod expr;
pub mod ids;
pub mod module;
pub mod port;
pub mod process;
pub mod signal;
pub mod source_map;
pub mod stmt;
pub mod types;

// Re-export primary types for convenience.
pub use arena::{Arena, ArenaId};
pub use cell::{BramConfig, Cell, CellKind, Connection, DspConfig, IobufConfig, PllConfig};
pub use const_value::ConstValue;
pub use design::Design;
pub use expr::{BinaryOp, Expr, UnaryOp};
pub use ids::{CellId, ClockDomainId, ModuleId, PortId, ProcessId, SignalId, TypeId};
pub use module::{Assignment, ClockDomain, Module, Parameter};
pub use port::{Port, PortDirection};
pub use process::{Edge, EdgeSensitivity, Process, ProcessKind, Sensitivity};
pub use signal::{Signal, SignalKind, SignalRef};
pub use source_map::SourceMap;
pub use stmt::{AssertionKind, CaseArm, Statement};
pub use types::{Type, TypeDb};
