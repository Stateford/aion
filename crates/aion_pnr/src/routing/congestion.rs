//! Congestion tracking for negotiated congestion routing.
//!
//! Tracks how many nets use each routing resource and maintains history
//! costs that increase each iteration for overused resources, encouraging
//! nets to find alternative paths.

use aion_arch::ids::WireId;
use std::collections::HashMap;

/// Tracks per-resource congestion state across PathFinder iterations.
///
/// Each routing resource has a present demand (current usage), capacity
/// (typically 1 for exclusive wires), and history cost (accumulated
/// penalty for repeated overuse).
#[derive(Debug, Clone)]
pub struct CongestionMap {
    /// Present demand: number of nets currently using each wire.
    demand: HashMap<WireId, u32>,
    /// History cost: accumulated penalty for each wire across iterations.
    history: HashMap<WireId, f64>,
    /// Capacity of each wire (typically 1).
    capacity: u32,
    /// History cost increment per iteration.
    history_factor: f64,
}

impl CongestionMap {
    /// Creates a new congestion map with default parameters.
    pub fn new() -> Self {
        Self {
            demand: HashMap::new(),
            history: HashMap::new(),
            capacity: 1,
            history_factor: 1.0,
        }
    }

    /// Records that a net is using the given wire.
    pub fn add_usage(&mut self, wire: WireId) {
        *self.demand.entry(wire).or_insert(0) += 1;
    }

    /// Removes a net's usage of the given wire.
    #[cfg(test)]
    pub fn remove_usage(&mut self, wire: WireId) {
        if let Some(d) = self.demand.get_mut(&wire) {
            *d = d.saturating_sub(1);
        }
    }

    /// Returns whether any wire is overused (demand > capacity).
    pub fn has_congestion(&self) -> bool {
        self.demand.values().any(|&d| d > self.capacity)
    }

    /// Returns the number of overused wires.
    pub fn overused_count(&self) -> usize {
        self.demand.values().filter(|&&d| d > self.capacity).count()
    }

    /// Returns the congestion cost for routing through the given wire.
    ///
    /// This is the sum of the present congestion penalty and the history cost,
    /// encouraging nets to avoid repeatedly-overused resources.
    pub fn wire_cost(&self, wire: WireId) -> f64 {
        let demand = *self.demand.get(&wire).unwrap_or(&0);
        let present_penalty = if demand > self.capacity {
            (demand - self.capacity) as f64
        } else {
            0.0
        };
        let history = *self.history.get(&wire).unwrap_or(&0.0);
        present_penalty + history
    }

    /// Updates history costs at the end of an iteration.
    ///
    /// Increases the history cost for every overused wire, making it more
    /// expensive in future iterations and encouraging rerouting.
    pub fn update_history(&mut self) {
        for (&wire, &demand) in &self.demand {
            if demand > self.capacity {
                let overflow = (demand - self.capacity) as f64;
                *self.history.entry(wire).or_insert(0.0) += overflow * self.history_factor;
            }
        }
    }

    /// Resets all demand counters (called at the start of each iteration).
    pub fn reset_demand(&mut self) {
        self.demand.clear();
    }
}

impl Default for CongestionMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_congestion_map() {
        let map = CongestionMap::new();
        assert!(!map.has_congestion());
        assert_eq!(map.overused_count(), 0);
    }

    #[test]
    fn single_usage_no_congestion() {
        let mut map = CongestionMap::new();
        let w = WireId::from_raw(0);
        map.add_usage(w);
        assert!(!map.has_congestion());
        assert_eq!(map.wire_cost(w), 0.0);
    }

    #[test]
    fn double_usage_causes_congestion() {
        let mut map = CongestionMap::new();
        let w = WireId::from_raw(0);
        map.add_usage(w);
        map.add_usage(w);
        assert!(map.has_congestion());
        assert_eq!(map.overused_count(), 1);
        assert!(map.wire_cost(w) > 0.0);
    }

    #[test]
    fn remove_usage_resolves_congestion() {
        let mut map = CongestionMap::new();
        let w = WireId::from_raw(0);
        map.add_usage(w);
        map.add_usage(w);
        assert!(map.has_congestion());
        map.remove_usage(w);
        assert!(!map.has_congestion());
    }

    #[test]
    fn history_accumulates() {
        let mut map = CongestionMap::new();
        let w = WireId::from_raw(0);
        map.add_usage(w);
        map.add_usage(w);
        map.update_history();

        let cost_after_1 = map.wire_cost(w);
        assert!(cost_after_1 > 0.0);

        map.update_history();
        let cost_after_2 = map.wire_cost(w);
        assert!(cost_after_2 > cost_after_1);
    }

    #[test]
    fn reset_demand_clears_usage() {
        let mut map = CongestionMap::new();
        let w = WireId::from_raw(0);
        map.add_usage(w);
        map.add_usage(w);
        assert!(map.has_congestion());

        map.reset_demand();
        assert!(!map.has_congestion());
    }

    #[test]
    fn unused_wire_zero_cost() {
        let map = CongestionMap::new();
        let w = WireId::from_raw(999);
        assert_eq!(map.wire_cost(w), 0.0);
    }

    #[test]
    fn multiple_wires_independent() {
        let mut map = CongestionMap::new();
        let w0 = WireId::from_raw(0);
        let w1 = WireId::from_raw(1);
        map.add_usage(w0);
        map.add_usage(w0);
        map.add_usage(w1);
        assert_eq!(map.overused_count(), 1);
        assert!(map.wire_cost(w0) > 0.0);
        assert_eq!(map.wire_cost(w1), 0.0);
    }

    #[test]
    fn history_persists_after_reset() {
        let mut map = CongestionMap::new();
        let w = WireId::from_raw(0);
        map.add_usage(w);
        map.add_usage(w);
        map.update_history();
        map.reset_demand();

        // Demand cleared but history persists
        assert!(!map.has_congestion());
        let cost = map.wire_cost(w);
        assert!(cost > 0.0, "history cost should persist after demand reset");
    }
}
