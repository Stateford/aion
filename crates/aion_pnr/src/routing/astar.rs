//! A* search for single-net routing.
//!
//! Finds the lowest-cost path from a driver pin to a sink pin through the
//! device routing graph. The cost function includes wire delay, history
//! penalty, and congestion penalty from the [`CongestionMap`].

use crate::route_tree::{RouteNode, RouteResource, RouteTree};
use crate::routing::congestion::CongestionMap;
use aion_arch::ids::{SiteId, WireId};
use aion_arch::types::RoutingGraph;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// A search state in the A* priority queue.
#[derive(Debug, Clone)]
struct AStarState {
    /// The wire currently being explored.
    wire: WireId,
    /// Total cost from start to this wire (g-score).
    cost: f64,
    /// Estimated total cost including heuristic (f-score = g + h).
    estimated_total: f64,
}

impl PartialEq for AStarState {
    fn eq(&self, other: &Self) -> bool {
        self.estimated_total == other.estimated_total
    }
}

impl Eq for AStarState {}

impl Ord for AStarState {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order for min-heap (BinaryHeap is max-heap by default)
        other
            .estimated_total
            .partial_cmp(&self.estimated_total)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for AStarState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Finds a route from `source_site` to `target_site` using A* search.
///
/// Returns a [`RouteTree`] representing the path, or `None` if no route
/// exists. Uses the routing graph's wire and PIP connectivity, with costs
/// from the congestion map.
pub(crate) fn astar_route(
    routing_graph: &RoutingGraph,
    congestion: &CongestionMap,
    source_site: SiteId,
    target_site: SiteId,
) -> Option<RouteTree> {
    if routing_graph.wires.is_empty() {
        // No routing graph available — return stub
        return Some(RouteTree::stub());
    }

    // Build adjacency from PIPs
    let mut adjacency: HashMap<WireId, Vec<(WireId, f64)>> = HashMap::new();
    for pip in &routing_graph.pips {
        let base_cost = pip.delay.max_ns + congestion.wire_cost(pip.dst_wire);
        adjacency
            .entry(pip.src_wire)
            .or_default()
            .push((pip.dst_wire, base_cost));
    }

    // A* search
    let start_wire = WireId::from_raw(source_site.as_raw());
    let end_wire = WireId::from_raw(target_site.as_raw());

    let mut open = BinaryHeap::new();
    let mut g_scores: HashMap<WireId, f64> = HashMap::new();
    let mut came_from: HashMap<WireId, WireId> = HashMap::new();

    g_scores.insert(start_wire, 0.0);
    open.push(AStarState {
        wire: start_wire,
        cost: 0.0,
        estimated_total: heuristic(start_wire, end_wire),
    });

    while let Some(current) = open.pop() {
        if current.wire == end_wire {
            // Reconstruct path
            return Some(reconstruct_path(&came_from, start_wire, end_wire));
        }

        let current_g = *g_scores.get(&current.wire).unwrap_or(&f64::INFINITY);
        if current.cost > current_g {
            continue; // Stale entry
        }

        if let Some(neighbors) = adjacency.get(&current.wire) {
            for &(next_wire, edge_cost) in neighbors {
                let tentative_g = current_g + edge_cost;
                if tentative_g < *g_scores.get(&next_wire).unwrap_or(&f64::INFINITY) {
                    g_scores.insert(next_wire, tentative_g);
                    came_from.insert(next_wire, current.wire);
                    open.push(AStarState {
                        wire: next_wire,
                        cost: tentative_g,
                        estimated_total: tentative_g + heuristic(next_wire, end_wire),
                    });
                }
            }
        }
    }

    None // No path found
}

/// Manhattan distance heuristic between two wire IDs (synthetic coordinates).
fn heuristic(from: WireId, to: WireId) -> f64 {
    let dx = (from.as_raw() as i64 - to.as_raw() as i64).unsigned_abs();
    dx as f64 * 0.1 // Scale factor for heuristic
}

/// Reconstructs the route tree from the came_from map.
fn reconstruct_path(came_from: &HashMap<WireId, WireId>, start: WireId, end: WireId) -> RouteTree {
    let mut path = vec![end];
    let mut current = end;
    while current != start {
        match came_from.get(&current) {
            Some(&prev) => {
                path.push(prev);
                current = prev;
            }
            None => break,
        }
    }
    path.reverse();

    // Build route tree from path
    let mut root = RouteNode {
        resource: RouteResource::Wire(path[0]),
        children: Vec::new(),
    };

    let mut current_node = &mut root;
    for &wire in &path[1..] {
        let child = RouteNode {
            resource: RouteResource::Wire(wire),
            children: Vec::new(),
        };
        current_node.children.push(child);
        current_node = current_node.children.last_mut().unwrap();
    }

    RouteTree::new(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_arch::types::{Delay, Pip, RoutingGraph, Wire};

    #[test]
    fn astar_empty_graph_returns_stub() {
        let graph = RoutingGraph::default();
        let congestion = CongestionMap::new();
        let result = astar_route(
            &graph,
            &congestion,
            SiteId::from_raw(0),
            SiteId::from_raw(1),
        );
        assert!(result.is_some());
        let rt = result.unwrap();
        assert_eq!(rt.resource_count(), 1); // stub
    }

    #[test]
    fn astar_simple_path() {
        let graph = RoutingGraph {
            wires: vec![
                Wire {
                    id: WireId::from_raw(0),
                    name: "w0".into(),
                },
                Wire {
                    id: WireId::from_raw(1),
                    name: "w1".into(),
                },
                Wire {
                    id: WireId::from_raw(2),
                    name: "w2".into(),
                },
            ],
            pips: vec![
                Pip {
                    id: aion_arch::ids::PipId::from_raw(0),
                    src_wire: WireId::from_raw(0),
                    dst_wire: WireId::from_raw(1),
                    delay: Delay::new(0.0, 0.1, 0.2),
                },
                Pip {
                    id: aion_arch::ids::PipId::from_raw(1),
                    src_wire: WireId::from_raw(1),
                    dst_wire: WireId::from_raw(2),
                    delay: Delay::new(0.0, 0.1, 0.2),
                },
            ],
        };

        let congestion = CongestionMap::new();
        let result = astar_route(
            &graph,
            &congestion,
            SiteId::from_raw(0),
            SiteId::from_raw(2),
        );
        assert!(result.is_some());
        let rt = result.unwrap();
        assert!(rt.resource_count() >= 2);
    }

    #[test]
    fn heuristic_same_node() {
        let w = WireId::from_raw(5);
        assert_eq!(heuristic(w, w), 0.0);
    }

    #[test]
    fn heuristic_different_nodes() {
        let a = WireId::from_raw(0);
        let b = WireId::from_raw(10);
        assert!(heuristic(a, b) > 0.0);
    }

    #[test]
    fn reconstruct_single_hop() {
        let mut came_from = HashMap::new();
        let start = WireId::from_raw(0);
        let end = WireId::from_raw(1);
        came_from.insert(end, start);

        let rt = reconstruct_path(&came_from, start, end);
        assert_eq!(rt.resource_count(), 2);
    }
}
