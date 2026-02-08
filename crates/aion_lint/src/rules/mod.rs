//! All built-in lint rule implementations.
//!
//! This module re-exports all individual rule types and provides
//! `register_builtin_rules` to add all 15 rules to a `LintEngine`.

mod c201;
mod c202;
mod c203;
mod c204;
mod e102;
mod e104;
mod e105;
mod w101;
mod w102;
mod w103;
mod w104;
mod w105;
mod w106;
mod w107;
mod w108;

pub use c201::{
    is_camel_case, is_pascal_case, is_snake_case, is_upper_snake_case, NamingViolation,
};
pub use c202::MissingDoc;
pub use c203::MagicNumber;
pub use c204::InconsistentStyle;
pub use e102::NonSynthesizable;
pub use e104::MultipleDrivers;
pub use e105::PortMismatch;
pub use w101::UnusedSignal;
pub use w102::UndrivenSignal;
pub use w103::WidthMismatch;
pub use w104::MissingReset;
pub use w105::IncompleteSensitivity;
pub use w106::LatchInferred;
pub use w107::Truncation;
pub use w108::DeadLogic;

use crate::LintEngine;

/// Registers all 15 built-in lint rules with the engine.
///
/// This adds rules W101-W108, E102/E104/E105, and C201-C204.
pub fn register_builtin_rules(engine: &mut LintEngine) {
    engine.register(Box::new(UnusedSignal));
    engine.register(Box::new(UndrivenSignal));
    engine.register(Box::new(WidthMismatch));
    engine.register(Box::new(MissingReset));
    engine.register(Box::new(IncompleteSensitivity));
    engine.register(Box::new(LatchInferred));
    engine.register(Box::new(Truncation));
    engine.register(Box::new(DeadLogic));
    engine.register(Box::new(NonSynthesizable));
    engine.register(Box::new(MultipleDrivers));
    engine.register(Box::new(PortMismatch));
    engine.register(Box::new(NamingViolation));
    engine.register(Box::new(MissingDoc));
    engine.register(Box::new(MagicNumber));
    engine.register(Box::new(InconsistentStyle));
}
