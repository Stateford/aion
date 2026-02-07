//! Shared foundational types used across the Aion FPGA toolchain.
//!
//! This crate provides core types including interned identifiers, content hashing,
//! frequency values, 4-state logic values, packed logic vectors, and common result types.

#![warn(missing_docs)]

pub mod frequency;
pub mod hash;
pub mod ident;
pub mod logic;
pub mod logic_vec;
pub mod result;

pub use frequency::{Frequency, ParseFrequencyError};
pub use hash::ContentHash;
pub use ident::{Ident, Interner};
pub use logic::Logic;
pub use logic_vec::LogicVec;
pub use result::{AionResult, InternalError};
