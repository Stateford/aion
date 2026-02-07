//! AST-to-AionIR elaboration engine.
//!
//! This crate transforms parsed HDL ASTs into the unified [`aion_ir`] intermediate
//! representation, performing hierarchy resolution, type checking, and generic/generate
//! expansion.

#![warn(missing_docs)]
