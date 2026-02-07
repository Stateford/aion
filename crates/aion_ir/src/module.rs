//! Module definitions — the primary organizational unit of the IR.
//!
//! A [`Module`] contains ports, signals, cells, processes, and assignments
//! that collectively describe a piece of hardware. Modules form a hierarchy
//! through cell instantiations.

use crate::arena::Arena;
use crate::ids::{CellId, ClockDomainId, ModuleId, ProcessId, SignalId, TypeId};
use crate::port::Port;
use crate::process::{Edge, Process};
use crate::signal::SignalRef;
use crate::{cell::Cell, signal::Signal};
use aion_common::{ContentHash, Ident};
use aion_source::Span;
use serde::{Deserialize, Serialize};

use crate::const_value::ConstValue;
use crate::expr::Expr;

/// A module parameter (generic in VHDL, parameter in Verilog/SV).
///
/// Parameters are resolved to concrete values during elaboration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    /// The parameter name.
    pub name: Ident,
    /// The parameter type.
    pub ty: TypeId,
    /// The resolved value after elaboration.
    pub value: ConstValue,
    /// The source span of the parameter declaration.
    pub span: Span,
}

/// A direct combinational assignment (concurrent signal assignment).
///
/// Represents `assign` statements in Verilog or concurrent signal assignments
/// in VHDL that exist outside of processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    /// The target signal or signal slice.
    pub target: SignalRef,
    /// The value expression.
    pub value: Expr,
    /// The source span of the assignment.
    pub span: Span,
}

/// A clock domain annotation.
///
/// Groups signals that are clocked by the same clock signal and edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockDomain {
    /// The unique ID of this clock domain.
    pub id: ClockDomainId,
    /// The domain name (e.g., "clk_50", "sys_clk").
    pub name: Ident,
    /// The clock signal driving this domain.
    pub clock_signal: SignalId,
    /// The active clock edge.
    pub edge: Edge,
}

/// A single hardware module in the design.
///
/// Contains ports, signals, cells, behavioral processes, and concurrent
/// assignments. Modules form a hierarchy through [`CellKind::Instance`](crate::cell::CellKind::Instance).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Module {
    /// The unique ID of this module in the design.
    pub id: ModuleId,
    /// The module name.
    pub name: Ident,
    /// The source span of the module declaration.
    pub span: Span,
    /// Module parameters (resolved after elaboration).
    pub params: Vec<Parameter>,
    /// The module's external port interface.
    pub ports: Vec<Port>,
    /// All signals declared within this module.
    pub signals: Arena<SignalId, Signal>,
    /// Primitive cells and module instantiations.
    pub cells: Arena<CellId, Cell>,
    /// Behavioral processes (lowered to cells during synthesis).
    pub processes: Arena<ProcessId, Process>,
    /// Direct combinational assignments.
    pub assignments: Vec<Assignment>,
    /// Clock domain annotations.
    pub clock_domains: Vec<ClockDomain>,
    /// Content hash of this module's source inputs (for incremental compilation).
    pub content_hash: ContentHash,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;

    fn empty_module(id: u32, name: Ident) -> Module {
        Module {
            id: ModuleId::from_raw(id),
            name,
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(b"test"),
        }
    }

    #[test]
    fn module_construction() {
        let m = empty_module(0, Ident::from_raw(1));
        assert_eq!(m.id.as_raw(), 0);
        assert!(m.signals.is_empty());
        assert!(m.cells.is_empty());
        assert!(m.processes.is_empty());
    }

    #[test]
    fn module_with_signals() {
        let mut m = empty_module(0, Ident::from_raw(1));
        let sig = Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: TypeId::from_raw(0),
            kind: crate::signal::SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        };
        let sid = m.signals.alloc(sig);
        assert_eq!(m.signals.len(), 1);
        assert_eq!(m.signals[sid].name, Ident::from_raw(2));
    }

    #[test]
    fn module_with_assignment() {
        let mut m = empty_module(0, Ident::from_raw(1));
        m.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: Expr::Literal(aion_common::LogicVec::all_zero(8)),
            span: Span::DUMMY,
        });
        assert_eq!(m.assignments.len(), 1);
    }

    #[test]
    fn module_with_clock_domain() {
        let mut m = empty_module(0, Ident::from_raw(1));
        m.clock_domains.push(ClockDomain {
            id: ClockDomainId::from_raw(0),
            name: Ident::from_raw(5),
            clock_signal: SignalId::from_raw(0),
            edge: Edge::Posedge,
        });
        assert_eq!(m.clock_domains.len(), 1);
    }

    #[test]
    fn parameter_construction() {
        let param = Parameter {
            name: Ident::from_raw(1),
            ty: TypeId::from_raw(0),
            value: ConstValue::Int(256),
            span: Span::DUMMY,
        };
        assert_eq!(param.value, ConstValue::Int(256));
    }
}
