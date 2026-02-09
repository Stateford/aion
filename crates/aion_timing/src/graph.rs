//! Timing graph data structures for static timing analysis.
//!
//! The [`TimingGraph`] is a device-independent directed graph of timing nodes
//! and edges. Nodes represent cell pins, routing nodes, clock sources, and
//! primary I/O. Edges represent propagation delays (cell delays, net delays,
//! setup/hold checks, clock-to-Q delays).
//!
//! The timing graph is built by the PnR crate's timing bridge, converting
//! placed-and-routed netlists into a form suitable for STA.

use crate::ids::{TimingEdgeId, TimingNodeId};
use aion_arch::types::Delay;
use serde::{Deserialize, Serialize};

/// A timing graph for static timing analysis.
///
/// Contains nodes (cell pins, routing points, I/O) and directed edges
/// (delays between nodes). The graph is built from a placed-and-routed
/// netlist and consumed by the STA algorithm.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimingGraph {
    /// All nodes in the timing graph.
    pub nodes: Vec<TimingNode>,
    /// All directed edges in the timing graph.
    pub edges: Vec<TimingEdge>,
}

impl TimingGraph {
    /// Creates an empty timing graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node to the timing graph and returns its ID.
    pub fn add_node(&mut self, name: String, node_type: TimingNodeType) -> TimingNodeId {
        let id = TimingNodeId::from_raw(self.nodes.len() as u32);
        self.nodes.push(TimingNode {
            id,
            name,
            node_type,
        });
        id
    }

    /// Adds a directed edge to the timing graph and returns its ID.
    pub fn add_edge(
        &mut self,
        from: TimingNodeId,
        to: TimingNodeId,
        delay: Delay,
        edge_type: TimingEdgeType,
    ) -> TimingEdgeId {
        let id = TimingEdgeId::from_raw(self.edges.len() as u32);
        self.edges.push(TimingEdge {
            id,
            from,
            to,
            delay,
            edge_type,
        });
        id
    }

    /// Returns the node with the given ID.
    pub fn node(&self, id: TimingNodeId) -> &TimingNode {
        &self.nodes[id.as_raw() as usize]
    }

    /// Returns the edge with the given ID.
    pub fn edge(&self, id: TimingEdgeId) -> &TimingEdge {
        &self.edges[id.as_raw() as usize]
    }

    /// Returns all edges originating from the given node.
    pub fn outgoing_edges(&self, node: TimingNodeId) -> Vec<&TimingEdge> {
        self.edges.iter().filter(|e| e.from == node).collect()
    }

    /// Returns all edges arriving at the given node.
    pub fn incoming_edges(&self, node: TimingNodeId) -> Vec<&TimingEdge> {
        self.edges.iter().filter(|e| e.to == node).collect()
    }

    /// Returns the total number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Returns the total number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Returns all source nodes (nodes with no incoming edges).
    pub fn source_nodes(&self) -> Vec<TimingNodeId> {
        let has_incoming: std::collections::HashSet<TimingNodeId> =
            self.edges.iter().map(|e| e.to).collect();
        self.nodes
            .iter()
            .filter(|n| !has_incoming.contains(&n.id))
            .map(|n| n.id)
            .collect()
    }

    /// Returns all sink nodes (nodes with no outgoing edges).
    pub fn sink_nodes(&self) -> Vec<TimingNodeId> {
        let has_outgoing: std::collections::HashSet<TimingNodeId> =
            self.edges.iter().map(|e| e.from).collect();
        self.nodes
            .iter()
            .filter(|n| !has_outgoing.contains(&n.id))
            .map(|n| n.id)
            .collect()
    }
}

/// A node in the timing graph.
///
/// Each node represents a point where timing is measured: a cell pin,
/// a routing node, a clock source, or a primary I/O port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingNode {
    /// The unique ID of this node.
    pub id: TimingNodeId,
    /// Human-readable name of this node (e.g., "lut_0/O", "clk_buf/I").
    pub name: String,
    /// The functional type of this node.
    pub node_type: TimingNodeType,
}

/// The type of a timing graph node.
///
/// Determines how the STA algorithm treats this node during
/// forward and backward propagation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimingNodeType {
    /// A pin on a cell (LUT input/output, FF D/Q/CLK).
    CellPin,
    /// A routing node in the interconnect fabric.
    RoutingNode,
    /// A clock source (PLL output, clock buffer output).
    ClockSource,
    /// A primary input port of the design.
    PrimaryInput,
    /// A primary output port of the design.
    PrimaryOutput,
}

/// A directed edge in the timing graph representing a delay.
///
/// Connects two nodes with a propagation delay and a semantic type
/// that determines how the delay is used in timing analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingEdge {
    /// The unique ID of this edge.
    pub id: TimingEdgeId,
    /// The source node of this edge.
    pub from: TimingNodeId,
    /// The destination node of this edge.
    pub to: TimingNodeId,
    /// The propagation delay along this edge.
    pub delay: Delay,
    /// The semantic type of this edge.
    pub edge_type: TimingEdgeType,
}

/// The type of a timing graph edge.
///
/// Determines how the edge's delay contributes to path timing
/// during STA forward/backward propagation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimingEdgeType {
    /// Combinational delay through a cell (input pin to output pin).
    CellDelay,
    /// Interconnect delay along a routed net.
    NetDelay,
    /// Setup time check at a flip-flop data pin relative to clock.
    SetupCheck,
    /// Hold time check at a flip-flop data pin relative to clock.
    HoldCheck,
    /// Clock-to-output delay at a flip-flop (clock pin to Q output).
    ClockToQ,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_arch::types::Delay;

    #[test]
    fn empty_graph() {
        let g = TimingGraph::new();
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
        assert!(g.source_nodes().is_empty());
        assert!(g.sink_nodes().is_empty());
    }

    #[test]
    fn add_nodes() {
        let mut g = TimingGraph::new();
        let n0 = g.add_node("input_a".into(), TimingNodeType::PrimaryInput);
        let n1 = g.add_node("lut_0/O".into(), TimingNodeType::CellPin);
        assert_eq!(n0.as_raw(), 0);
        assert_eq!(n1.as_raw(), 1);
        assert_eq!(g.node_count(), 2);
        assert_eq!(g.node(n0).name, "input_a");
        assert_eq!(g.node(n1).node_type, TimingNodeType::CellPin);
    }

    #[test]
    fn add_edges() {
        let mut g = TimingGraph::new();
        let n0 = g.add_node("src".into(), TimingNodeType::PrimaryInput);
        let n1 = g.add_node("dst".into(), TimingNodeType::CellPin);
        let e = g.add_edge(n0, n1, Delay::new(0.1, 0.2, 0.3), TimingEdgeType::NetDelay);
        assert_eq!(e.as_raw(), 0);
        assert_eq!(g.edge_count(), 1);
        let edge = g.edge(e);
        assert_eq!(edge.from, n0);
        assert_eq!(edge.to, n1);
        assert_eq!(edge.delay.typ_ns, 0.2);
    }

    #[test]
    fn outgoing_edges() {
        let mut g = TimingGraph::new();
        let n0 = g.add_node("a".into(), TimingNodeType::PrimaryInput);
        let n1 = g.add_node("b".into(), TimingNodeType::CellPin);
        let n2 = g.add_node("c".into(), TimingNodeType::CellPin);
        g.add_edge(n0, n1, Delay::ZERO, TimingEdgeType::NetDelay);
        g.add_edge(n0, n2, Delay::ZERO, TimingEdgeType::NetDelay);
        g.add_edge(n1, n2, Delay::ZERO, TimingEdgeType::CellDelay);
        assert_eq!(g.outgoing_edges(n0).len(), 2);
        assert_eq!(g.outgoing_edges(n1).len(), 1);
        assert_eq!(g.outgoing_edges(n2).len(), 0);
    }

    #[test]
    fn incoming_edges() {
        let mut g = TimingGraph::new();
        let n0 = g.add_node("a".into(), TimingNodeType::PrimaryInput);
        let n1 = g.add_node("b".into(), TimingNodeType::CellPin);
        let n2 = g.add_node("c".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(n0, n1, Delay::ZERO, TimingEdgeType::NetDelay);
        g.add_edge(n1, n2, Delay::ZERO, TimingEdgeType::CellDelay);
        assert_eq!(g.incoming_edges(n0).len(), 0);
        assert_eq!(g.incoming_edges(n1).len(), 1);
        assert_eq!(g.incoming_edges(n2).len(), 1);
    }

    #[test]
    fn source_and_sink_nodes() {
        let mut g = TimingGraph::new();
        let n0 = g.add_node("in".into(), TimingNodeType::PrimaryInput);
        let n1 = g.add_node("mid".into(), TimingNodeType::CellPin);
        let n2 = g.add_node("out".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(n0, n1, Delay::ZERO, TimingEdgeType::NetDelay);
        g.add_edge(n1, n2, Delay::ZERO, TimingEdgeType::CellDelay);
        let sources = g.source_nodes();
        let sinks = g.sink_nodes();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0], n0);
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0], n2);
    }

    #[test]
    fn node_type_variants() {
        let types = [
            TimingNodeType::CellPin,
            TimingNodeType::RoutingNode,
            TimingNodeType::ClockSource,
            TimingNodeType::PrimaryInput,
            TimingNodeType::PrimaryOutput,
        ];
        for (i, t) in types.iter().enumerate() {
            for (j, u) in types.iter().enumerate() {
                if i == j {
                    assert_eq!(t, u);
                } else {
                    assert_ne!(t, u);
                }
            }
        }
    }

    #[test]
    fn edge_type_variants() {
        let types = [
            TimingEdgeType::CellDelay,
            TimingEdgeType::NetDelay,
            TimingEdgeType::SetupCheck,
            TimingEdgeType::HoldCheck,
            TimingEdgeType::ClockToQ,
        ];
        for (i, t) in types.iter().enumerate() {
            for (j, u) in types.iter().enumerate() {
                if i == j {
                    assert_eq!(t, u);
                } else {
                    assert_ne!(t, u);
                }
            }
        }
    }

    #[test]
    fn graph_serde_roundtrip() {
        let mut g = TimingGraph::new();
        let n0 = g.add_node("a".into(), TimingNodeType::PrimaryInput);
        let n1 = g.add_node("b".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(n0, n1, Delay::new(0.5, 1.0, 1.5), TimingEdgeType::NetDelay);

        let json = serde_json::to_string(&g).unwrap();
        let restored: TimingGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.node_count(), 2);
        assert_eq!(restored.edge_count(), 1);
        assert_eq!(restored.nodes[0].name, "a");
    }

    #[test]
    fn multi_fanout_graph() {
        let mut g = TimingGraph::new();
        let src = g.add_node("clk".into(), TimingNodeType::ClockSource);
        let d0 = g.add_node("ff0/CLK".into(), TimingNodeType::CellPin);
        let d1 = g.add_node("ff1/CLK".into(), TimingNodeType::CellPin);
        let d2 = g.add_node("ff2/CLK".into(), TimingNodeType::CellPin);
        g.add_edge(src, d0, Delay::new(0.1, 0.2, 0.3), TimingEdgeType::NetDelay);
        g.add_edge(src, d1, Delay::new(0.1, 0.2, 0.3), TimingEdgeType::NetDelay);
        g.add_edge(src, d2, Delay::new(0.1, 0.2, 0.3), TimingEdgeType::NetDelay);
        assert_eq!(g.outgoing_edges(src).len(), 3);
        assert_eq!(g.source_nodes(), vec![src]);
        assert_eq!(g.sink_nodes().len(), 3);
    }

    #[test]
    fn diamond_graph() {
        let mut g = TimingGraph::new();
        let a = g.add_node("a".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("b".into(), TimingNodeType::CellPin);
        let c = g.add_node("c".into(), TimingNodeType::CellPin);
        let d = g.add_node("d".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 1.0, 2.0), TimingEdgeType::NetDelay);
        g.add_edge(a, c, Delay::new(0.0, 2.0, 4.0), TimingEdgeType::NetDelay);
        g.add_edge(b, d, Delay::new(0.0, 1.0, 2.0), TimingEdgeType::CellDelay);
        g.add_edge(c, d, Delay::new(0.0, 0.5, 1.0), TimingEdgeType::CellDelay);
        assert_eq!(g.source_nodes(), vec![a]);
        assert_eq!(g.sink_nodes(), vec![d]);
        assert_eq!(g.incoming_edges(d).len(), 2);
    }
}
