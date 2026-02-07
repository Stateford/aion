//! Process definitions for behavioral hardware descriptions.
//!
//! A [`Process`] represents a VHDL process or Verilog always block,
//! containing behavioral statements that describe sequential or combinational logic.

use crate::ids::{ProcessId, SignalId};
use crate::stmt::Statement;
use aion_common::Ident;
use aion_source::Span;
use serde::{Deserialize, Serialize};

/// The kind of process, determining how it is synthesized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessKind {
    /// Combinational logic (`always_comb`, VHDL combinational process).
    Combinational,
    /// Sequential logic (`always_ff`, VHDL clocked process).
    Sequential,
    /// Latched logic (`always_latch`).
    Latched,
    /// Initial block (testbench only, not synthesizable).
    Initial,
}

/// A clock/reset edge type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Edge {
    /// Rising edge (0→1).
    Posedge,
    /// Falling edge (1→0).
    Negedge,
    /// Both edges.
    Both,
}

/// A signal with its associated edge in a sensitivity list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSensitivity {
    /// The signal being monitored.
    pub signal: SignalId,
    /// The edge to trigger on.
    pub edge: Edge,
}

/// The sensitivity list of a process.
///
/// Determines when the process is re-evaluated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Sensitivity {
    /// Sensitive to all read signals (`always_comb`, `process(all)`).
    All,
    /// Sensitive to specific signal edges (`always_ff @(posedge clk)`).
    EdgeList(Vec<EdgeSensitivity>),
    /// Sensitive to specific signal values (`process(a, b, c)`).
    SignalList(Vec<SignalId>),
}

/// A behavioral process in a module.
///
/// Processes contain sequential statements that describe hardware behavior.
/// During synthesis, combinational processes become logic gates and
/// sequential processes become flip-flops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Process {
    /// The unique ID of this process within its module.
    pub id: ProcessId,
    /// An optional process label/name.
    pub name: Option<Ident>,
    /// The kind of process (combinational, sequential, etc.).
    pub kind: ProcessKind,
    /// The process body.
    pub body: Statement,
    /// The sensitivity list.
    pub sensitivity: Sensitivity,
    /// The source span of the process declaration.
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stmt::Statement;

    #[test]
    fn combinational_process() {
        let proc = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            body: Statement::Nop,
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        };
        assert_eq!(proc.kind, ProcessKind::Combinational);
        assert!(proc.name.is_none());
    }

    #[test]
    fn sequential_process_with_edge() {
        let proc = Process {
            id: ProcessId::from_raw(1),
            name: Some(Ident::from_raw(10)),
            kind: ProcessKind::Sequential,
            body: Statement::Nop,
            sensitivity: Sensitivity::EdgeList(vec![
                EdgeSensitivity {
                    signal: SignalId::from_raw(0),
                    edge: Edge::Posedge,
                },
                EdgeSensitivity {
                    signal: SignalId::from_raw(1),
                    edge: Edge::Negedge,
                },
            ]),
            span: Span::DUMMY,
        };
        assert_eq!(proc.kind, ProcessKind::Sequential);
        assert!(proc.name.is_some());
        if let Sensitivity::EdgeList(edges) = &proc.sensitivity {
            assert_eq!(edges.len(), 2);
            assert_eq!(edges[0].edge, Edge::Posedge);
            assert_eq!(edges[1].edge, Edge::Negedge);
        } else {
            panic!("expected EdgeList");
        }
    }

    #[test]
    fn process_kinds_distinct() {
        let kinds = [
            ProcessKind::Combinational,
            ProcessKind::Sequential,
            ProcessKind::Latched,
            ProcessKind::Initial,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for (j, b) in kinds.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }

    #[test]
    fn edge_types_distinct() {
        assert_ne!(Edge::Posedge, Edge::Negedge);
        assert_ne!(Edge::Posedge, Edge::Both);
        assert_ne!(Edge::Negedge, Edge::Both);
    }

    #[test]
    fn sensitivity_signal_list() {
        let sens = Sensitivity::SignalList(vec![
            SignalId::from_raw(0),
            SignalId::from_raw(1),
            SignalId::from_raw(2),
        ]);
        if let Sensitivity::SignalList(sigs) = &sens {
            assert_eq!(sigs.len(), 3);
        } else {
            panic!("expected SignalList");
        }
    }
}
