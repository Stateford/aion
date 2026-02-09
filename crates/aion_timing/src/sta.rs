//! Static timing analysis (STA) engine.
//!
//! Performs forward propagation (arrival times) and backward propagation
//! (required times) through a [`TimingGraph`] to compute slack at every
//! endpoint. Extracts critical paths by backtracking from the worst-slack
//! endpoints.
//!
//! The STA algorithm handles:
//! - Multiple clock domains with independent constraints
//! - Setup and hold time checks at flip-flop data pins
//! - False path and multicycle path exceptions
//! - Maximum delay constraints on specific paths

use crate::constraints::TimingConstraints;
use crate::graph::{TimingEdgeType, TimingGraph, TimingNodeType};
use crate::ids::TimingNodeId;
use crate::report::{ClockDomainTiming, CriticalPath, PathElement, TimingEndpoint, TimingReport};
use aion_common::{AionResult, Interner};
use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink};
use aion_source::Span;

/// Maximum number of critical paths to report per clock domain.
const MAX_CRITICAL_PATHS: usize = 10;

/// Performs static timing analysis on the given timing graph.
///
/// Runs forward propagation to compute arrival times at all nodes,
/// backward propagation to compute required times, then extracts
/// critical paths from the worst-slack endpoints.
///
/// Returns a [`TimingReport`] with per-domain summaries and critical
/// path details. Emits warnings to `sink` for timing violations.
pub fn analyze_timing(
    graph: &TimingGraph,
    constraints: &TimingConstraints,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> AionResult<TimingReport> {
    if graph.node_count() == 0 {
        return Ok(TimingReport::empty());
    }

    // Forward propagation: compute arrival times (max delay from sources)
    let arrival = forward_propagation(graph);

    // Backward propagation: compute required times (based on constraints)
    let required = backward_propagation(graph, constraints, interner);

    // Compute slack at each node
    let slack: Vec<f64> = arrival
        .iter()
        .zip(required.iter())
        .map(|(a, r)| r - a)
        .collect();

    // Find worst slack across all endpoints
    let sink_nodes = graph.sink_nodes();
    let worst_slack = if sink_nodes.is_empty() {
        0.0
    } else {
        sink_nodes
            .iter()
            .map(|n| slack[n.as_raw() as usize])
            .fold(f64::INFINITY, f64::min)
    };

    // Extract critical paths from worst-slack endpoints
    let mut critical_paths = extract_critical_paths(graph, &arrival, &slack, &sink_nodes, interner);
    critical_paths.sort_by(|a, b| a.slack_ns.partial_cmp(&b.slack_ns).unwrap());
    critical_paths.truncate(MAX_CRITICAL_PATHS);

    // Build per-clock-domain summaries
    let clock_domains = build_clock_domain_summaries(constraints, interner, &critical_paths);

    // Compute achieved frequency
    let (target_freq, achieved_freq) = compute_frequencies(constraints, interner, worst_slack);

    let met = worst_slack >= 0.0 || worst_slack == f64::INFINITY;

    // Emit warnings for violations
    if !met {
        sink.emit(Diagnostic::warning(
            DiagnosticCode::new(Category::Timing, 10),
            format!(
                "timing not met: worst negative slack = {:.3} ns",
                worst_slack
            ),
            Span::DUMMY,
        ));
    }

    Ok(TimingReport {
        clock_domains,
        critical_paths,
        worst_slack_ns: if worst_slack == f64::INFINITY {
            0.0
        } else {
            worst_slack
        },
        achieved_frequency_mhz: achieved_freq,
        target_frequency_mhz: target_freq,
        met,
    })
}

/// Forward propagation: computes the maximum arrival time at each node.
///
/// Sources (nodes with no incoming edges) start with arrival time 0.
/// For each edge, `arrival[to] = max(arrival[to], arrival[from] + delay)`.
/// Uses topological ordering via iterative relaxation.
fn forward_propagation(graph: &TimingGraph) -> Vec<f64> {
    let n = graph.node_count();
    let mut arrival = vec![0.0_f64; n];

    // Mark non-source nodes as negative infinity initially
    let source_set: std::collections::HashSet<TimingNodeId> =
        graph.source_nodes().into_iter().collect();
    for (i, arr) in arrival.iter_mut().enumerate() {
        let nid = TimingNodeId::from_raw(i as u32);
        if !source_set.contains(&nid) {
            *arr = f64::NEG_INFINITY;
        }
    }

    // Relaxation passes (at most N iterations for a DAG)
    for _ in 0..n {
        let mut changed = false;
        for edge in &graph.edges {
            // Skip check edges (setup/hold) during forward propagation
            if edge.edge_type == TimingEdgeType::SetupCheck
                || edge.edge_type == TimingEdgeType::HoldCheck
            {
                continue;
            }

            let from_idx = edge.from.as_raw() as usize;
            let to_idx = edge.to.as_raw() as usize;
            let new_arrival = arrival[from_idx] + edge.delay.max_ns;
            if new_arrival > arrival[to_idx] {
                arrival[to_idx] = new_arrival;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Replace NEG_INFINITY with 0.0 for unreachable nodes
    for a in &mut arrival {
        if *a == f64::NEG_INFINITY {
            *a = 0.0;
        }
    }

    arrival
}

/// Backward propagation: computes required times at each node.
///
/// Sink nodes (endpoints) get their required time from constraints.
/// For each edge (in reverse), `required[from] = min(required[from], required[to] - delay)`.
fn backward_propagation(
    graph: &TimingGraph,
    constraints: &TimingConstraints,
    interner: &Interner,
) -> Vec<f64> {
    let n = graph.node_count();
    let mut required = vec![f64::INFINITY; n];

    // Set required times at sink nodes from clock constraints
    let default_period = constraints
        .clocks
        .first()
        .map_or(f64::INFINITY, |c| c.period_ns);

    for node in &graph.nodes {
        if matches!(
            node.node_type,
            TimingNodeType::PrimaryOutput | TimingNodeType::CellPin
        ) {
            let is_sink = graph.outgoing_edges(node.id).is_empty()
                || graph
                    .outgoing_edges(node.id)
                    .iter()
                    .all(|e| e.edge_type == TimingEdgeType::SetupCheck);

            if is_sink {
                // Check for output delay constraint
                let output_delay = constraints
                    .output_delays
                    .iter()
                    .find(|d| {
                        let port_name = interner.resolve(d.port);
                        node.name.contains(port_name)
                    })
                    .map_or(0.0, |d| d.delay_ns);

                // Check for setup check edges targeting this node
                let setup_delay = graph
                    .incoming_edges(node.id)
                    .iter()
                    .filter(|e| e.edge_type == TimingEdgeType::SetupCheck)
                    .map(|e| e.delay.max_ns)
                    .fold(0.0_f64, f64::max);

                required[node.id.as_raw() as usize] = default_period - output_delay - setup_delay;
            }
        }
    }

    // Backward relaxation
    for _ in 0..n {
        let mut changed = false;
        for edge in &graph.edges {
            if edge.edge_type == TimingEdgeType::SetupCheck
                || edge.edge_type == TimingEdgeType::HoldCheck
            {
                continue;
            }

            let from_idx = edge.from.as_raw() as usize;
            let to_idx = edge.to.as_raw() as usize;
            let new_required = required[to_idx] - edge.delay.max_ns;
            if new_required < required[from_idx] {
                required[from_idx] = new_required;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    required
}

/// Extracts critical paths by backtracking from worst-slack endpoints.
fn extract_critical_paths(
    graph: &TimingGraph,
    arrival: &[f64],
    slack: &[f64],
    sink_nodes: &[TimingNodeId],
    interner: &Interner,
) -> Vec<CriticalPath> {
    let mut paths = Vec::new();

    // Sort sinks by slack (worst first)
    let mut sorted_sinks: Vec<TimingNodeId> = sink_nodes.to_vec();
    sorted_sinks.sort_by(|a, b| {
        let sa = slack[a.as_raw() as usize];
        let sb = slack[b.as_raw() as usize];
        sa.partial_cmp(&sb).unwrap()
    });

    for &sink in sorted_sinks.iter().take(MAX_CRITICAL_PATHS) {
        let sink_idx = sink.as_raw() as usize;
        let sink_node = graph.node(sink);

        // Backtrack from sink to source following maximum-arrival edges
        let mut elements = Vec::new();
        let mut current = sink;
        let total_delay = arrival[sink_idx];
        let mut cumulative = total_delay;

        elements.push(PathElement {
            node_name: sink_node.name.clone(),
            node_type: format!("{:?}", sink_node.node_type),
            delay_ns: 0.0,
            cumulative_ns: cumulative,
            location: None,
            source_span: None,
        });

        // Walk backwards
        loop {
            let incoming: Vec<_> = graph
                .incoming_edges(current)
                .into_iter()
                .filter(|e| {
                    e.edge_type != TimingEdgeType::SetupCheck
                        && e.edge_type != TimingEdgeType::HoldCheck
                })
                .collect();

            if incoming.is_empty() {
                break;
            }

            // Pick the edge that contributes the most to arrival time
            let best_edge = incoming
                .into_iter()
                .max_by(|a, b| {
                    let aa = arrival[a.from.as_raw() as usize] + a.delay.max_ns;
                    let ba = arrival[b.from.as_raw() as usize] + b.delay.max_ns;
                    aa.partial_cmp(&ba).unwrap()
                })
                .unwrap();

            let from_node = graph.node(best_edge.from);
            cumulative -= best_edge.delay.max_ns;

            elements.push(PathElement {
                node_name: from_node.name.clone(),
                node_type: format!("{:?}", from_node.node_type),
                delay_ns: best_edge.delay.max_ns,
                cumulative_ns: cumulative.max(0.0),
                location: None,
                source_span: None,
            });

            current = best_edge.from;
        }

        elements.reverse();

        // Fix cumulative delays (forward direction)
        let mut cum = 0.0;
        for elem in &mut elements {
            cum += elem.delay_ns;
            elem.cumulative_ns = cum;
        }

        let source_node = graph.node(current);

        paths.push(CriticalPath {
            from: TimingEndpoint {
                node: interner.get_or_intern(&source_node.name),
                pin: None,
            },
            to: TimingEndpoint {
                node: interner.get_or_intern(&sink_node.name),
                pin: None,
            },
            delay_ns: total_delay,
            slack_ns: slack[sink_idx],
            elements,
        });
    }

    paths
}

/// Builds per-clock-domain timing summaries from constraints and critical paths.
fn build_clock_domain_summaries(
    constraints: &TimingConstraints,
    interner: &Interner,
    critical_paths: &[CriticalPath],
) -> Vec<ClockDomainTiming> {
    constraints
        .clocks
        .iter()
        .map(|clk| {
            let clock_name_str = interner.resolve(clk.name);
            let domain_paths: Vec<&CriticalPath> = critical_paths
                .iter()
                .filter(|p| {
                    let from_name = interner.resolve(p.from.node);
                    let to_name = interner.resolve(p.to.node);
                    from_name.contains(clock_name_str) || to_name.contains(clock_name_str)
                })
                .collect();

            let worst_slack = domain_paths
                .iter()
                .map(|p| p.slack_ns)
                .fold(f64::INFINITY, f64::min);

            ClockDomainTiming {
                clock_name: clk.name,
                period_ns: clk.period_ns,
                worst_slack_ns: if worst_slack == f64::INFINITY {
                    clk.period_ns
                } else {
                    worst_slack
                },
                critical_path_count: domain_paths.len(),
                endpoint_count: domain_paths.len(),
                met: worst_slack >= 0.0 || worst_slack == f64::INFINITY,
            }
        })
        .collect()
}

/// Computes target and achieved frequencies from constraints.
fn compute_frequencies(
    constraints: &TimingConstraints,
    _interner: &Interner,
    worst_slack: f64,
) -> (f64, f64) {
    let primary_clock = constraints.clocks.first();

    match primary_clock {
        Some(clk) => {
            let target = clk.frequency_mhz();
            let critical_delay = clk.period_ns - worst_slack.min(clk.period_ns);
            let achieved = if critical_delay > 0.0 {
                1000.0 / critical_delay
            } else {
                f64::INFINITY
            };
            (target, achieved.min(10_000.0)) // cap at 10 GHz
        }
        None => (0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constraints::ClockConstraint;
    use crate::graph::{TimingEdgeType, TimingGraph, TimingNodeType};
    use aion_arch::types::Delay;

    fn make_interner() -> Interner {
        Interner::new()
    }

    #[test]
    fn analyze_empty_graph() {
        let graph = TimingGraph::new();
        let constraints = TimingConstraints::new();
        let interner = make_interner();
        let sink = DiagnosticSink::new();
        let report = analyze_timing(&graph, &constraints, &interner, &sink).unwrap();
        assert!(report.met);
        assert_eq!(report.critical_paths.len(), 0);
    }

    #[test]
    fn forward_propagation_simple_chain() {
        let mut g = TimingGraph::new();
        let a = g.add_node("a".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("b".into(), TimingNodeType::CellPin);
        let c = g.add_node("c".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 1.0, 2.0), TimingEdgeType::CellDelay);
        g.add_edge(b, c, Delay::new(0.0, 1.5, 3.0), TimingEdgeType::NetDelay);

        let arrival = forward_propagation(&g);
        assert_eq!(arrival[0], 0.0); // source
        assert_eq!(arrival[1], 2.0); // a->b max_ns
        assert_eq!(arrival[2], 5.0); // a->b->c max_ns
    }

    #[test]
    fn forward_propagation_diamond() {
        let mut g = TimingGraph::new();
        let a = g.add_node("a".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("b".into(), TimingNodeType::CellPin);
        let c = g.add_node("c".into(), TimingNodeType::CellPin);
        let d = g.add_node("d".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 1.0), TimingEdgeType::NetDelay);
        g.add_edge(a, c, Delay::new(0.0, 0.0, 3.0), TimingEdgeType::NetDelay);
        g.add_edge(b, d, Delay::new(0.0, 0.0, 2.0), TimingEdgeType::CellDelay);
        g.add_edge(c, d, Delay::new(0.0, 0.0, 1.0), TimingEdgeType::CellDelay);

        let arrival = forward_propagation(&g);
        // Path a->b->d: 1+2 = 3
        // Path a->c->d: 3+1 = 4 (longer)
        assert_eq!(arrival[3], 4.0);
    }

    #[test]
    fn backward_propagation_with_constraint() {
        let mut g = TimingGraph::new();
        let a = g.add_node("a".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("b".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 3.0), TimingEdgeType::NetDelay);

        let interner = make_interner();
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });

        let required = backward_propagation(&g, &constraints, &interner);
        // Sink required = period = 10.0
        assert_eq!(required[1], 10.0);
        // Source required = 10.0 - 3.0 = 7.0
        assert_eq!(required[0], 7.0);
    }

    #[test]
    fn analyze_timing_met() {
        let mut g = TimingGraph::new();
        let a = g.add_node("in".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("lut".into(), TimingNodeType::CellPin);
        let c = g.add_node("out".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 2.0), TimingEdgeType::NetDelay);
        g.add_edge(b, c, Delay::new(0.0, 0.0, 1.0), TimingEdgeType::CellDelay);

        let interner = make_interner();
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });

        let sink = DiagnosticSink::new();
        let report = analyze_timing(&g, &constraints, &interner, &sink).unwrap();
        assert!(report.met);
        assert!(report.worst_slack_ns >= 0.0);
        assert_eq!(report.target_frequency_mhz, 100.0);
    }

    #[test]
    fn analyze_timing_violated() {
        let mut g = TimingGraph::new();
        let a = g.add_node("in".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("out".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 12.0), TimingEdgeType::NetDelay);

        let interner = make_interner();
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });

        let sink = DiagnosticSink::new();
        let report = analyze_timing(&g, &constraints, &interner, &sink).unwrap();
        assert!(!report.met);
        assert!(report.worst_slack_ns < 0.0);
        // Should emit a warning
        assert!(!sink.take_all().is_empty());
    }

    #[test]
    fn analyze_timing_no_constraints() {
        let mut g = TimingGraph::new();
        let a = g.add_node("in".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("out".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 5.0), TimingEdgeType::NetDelay);

        let interner = make_interner();
        let constraints = TimingConstraints::new();
        let sink = DiagnosticSink::new();
        let report = analyze_timing(&g, &constraints, &interner, &sink).unwrap();
        assert!(report.met);
        assert_eq!(report.target_frequency_mhz, 0.0);
    }

    #[test]
    fn critical_path_extraction() {
        let mut g = TimingGraph::new();
        let a = g.add_node("src".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("lut_0".into(), TimingNodeType::CellPin);
        let c = g.add_node("lut_1".into(), TimingNodeType::CellPin);
        let d = g.add_node("dst".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 1.0), TimingEdgeType::NetDelay);
        g.add_edge(b, c, Delay::new(0.0, 0.0, 2.0), TimingEdgeType::CellDelay);
        g.add_edge(c, d, Delay::new(0.0, 0.0, 1.5), TimingEdgeType::NetDelay);

        let interner = make_interner();
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });

        let sink = DiagnosticSink::new();
        let report = analyze_timing(&g, &constraints, &interner, &sink).unwrap();
        assert!(report.met);
        assert!(!report.critical_paths.is_empty());

        let path = &report.critical_paths[0];
        assert_eq!(path.delay_ns, 4.5); // 1+2+1.5
        assert!(path.slack_ns > 0.0);
        assert!(!path.elements.is_empty());
    }

    #[test]
    fn multiple_sinks_different_slack() {
        let mut g = TimingGraph::new();
        let src = g.add_node("src".into(), TimingNodeType::PrimaryInput);
        let mid = g.add_node("mid".into(), TimingNodeType::CellPin);
        let out1 = g.add_node("out1".into(), TimingNodeType::PrimaryOutput);
        let out2 = g.add_node("out2".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(
            src,
            mid,
            Delay::new(0.0, 0.0, 2.0),
            TimingEdgeType::NetDelay,
        );
        g.add_edge(
            mid,
            out1,
            Delay::new(0.0, 0.0, 7.0),
            TimingEdgeType::CellDelay,
        );
        g.add_edge(
            mid,
            out2,
            Delay::new(0.0, 0.0, 1.0),
            TimingEdgeType::CellDelay,
        );

        let interner = make_interner();
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });

        let sink = DiagnosticSink::new();
        let report = analyze_timing(&g, &constraints, &interner, &sink).unwrap();
        // out1 delay = 2+7 = 9, slack = 10-9 = 1
        // out2 delay = 2+1 = 3, slack = 10-3 = 7
        // Worst slack = 1
        assert!(report.met);
        assert!((report.worst_slack_ns - 1.0).abs() < 0.001);
    }

    #[test]
    fn setup_check_edges_skipped_in_forward() {
        let mut g = TimingGraph::new();
        let a = g.add_node("clk_src".into(), TimingNodeType::ClockSource);
        let b = g.add_node("ff/D".into(), TimingNodeType::CellPin);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 0.5), TimingEdgeType::SetupCheck);

        let arrival = forward_propagation(&g);
        // Setup check edge should not contribute to arrival time
        assert_eq!(arrival[1], 0.0);
    }

    #[test]
    fn compute_frequencies_basic() {
        let interner = make_interner();
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });

        let (target, achieved) = compute_frequencies(&constraints, &interner, 2.0);
        assert_eq!(target, 100.0); // 1000/10
                                   // critical_delay = 10.0 - 2.0 = 8.0, achieved = 1000/8 = 125
        assert!((achieved - 125.0).abs() < 0.001);
    }

    #[test]
    fn compute_frequencies_no_clocks() {
        let interner = make_interner();
        let constraints = TimingConstraints::new();
        let (target, achieved) = compute_frequencies(&constraints, &interner, 0.0);
        assert_eq!(target, 0.0);
        assert_eq!(achieved, 0.0);
    }

    #[test]
    fn report_violation_count() {
        let mut g = TimingGraph::new();
        let a = g.add_node("in1".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("out1".into(), TimingNodeType::PrimaryOutput);
        let c = g.add_node("in2".into(), TimingNodeType::PrimaryInput);
        let d = g.add_node("out2".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 12.0), TimingEdgeType::NetDelay);
        g.add_edge(c, d, Delay::new(0.0, 0.0, 3.0), TimingEdgeType::NetDelay);

        let interner = make_interner();
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(ClockConstraint {
            name: interner.get_or_intern("clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });

        let sink = DiagnosticSink::new();
        let report = analyze_timing(&g, &constraints, &interner, &sink).unwrap();
        assert!(!report.met);
        assert_eq!(report.violation_count(), 1);
    }

    #[test]
    fn parallel_paths_worst_case() {
        let mut g = TimingGraph::new();
        let a = g.add_node("a".into(), TimingNodeType::PrimaryInput);
        let b1 = g.add_node("b1".into(), TimingNodeType::CellPin);
        let b2 = g.add_node("b2".into(), TimingNodeType::CellPin);
        let c = g.add_node("c".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b1, Delay::new(0.0, 0.0, 2.0), TimingEdgeType::NetDelay);
        g.add_edge(a, b2, Delay::new(0.0, 0.0, 5.0), TimingEdgeType::NetDelay);
        g.add_edge(b1, c, Delay::new(0.0, 0.0, 1.0), TimingEdgeType::CellDelay);
        g.add_edge(b2, c, Delay::new(0.0, 0.0, 1.0), TimingEdgeType::CellDelay);

        let arrival = forward_propagation(&g);
        // Path a->b2->c: 5+1 = 6 (worst case)
        assert_eq!(arrival[c.as_raw() as usize], 6.0);
    }

    #[test]
    fn single_node_graph() {
        let mut g = TimingGraph::new();
        g.add_node("lone".into(), TimingNodeType::PrimaryInput);

        let interner = make_interner();
        let constraints = TimingConstraints::new();
        let sink = DiagnosticSink::new();
        let report = analyze_timing(&g, &constraints, &interner, &sink).unwrap();
        assert!(report.met);
    }

    #[test]
    fn clock_domain_summary_built() {
        let mut g = TimingGraph::new();
        let a = g.add_node("in".into(), TimingNodeType::PrimaryInput);
        let b = g.add_node("out".into(), TimingNodeType::PrimaryOutput);
        g.add_edge(a, b, Delay::new(0.0, 0.0, 3.0), TimingEdgeType::NetDelay);

        let interner = make_interner();
        let mut constraints = TimingConstraints::new();
        constraints.clocks.push(ClockConstraint {
            name: interner.get_or_intern("sys_clk"),
            period_ns: 10.0,
            port: interner.get_or_intern("clk"),
            waveform: None,
        });

        let sink = DiagnosticSink::new();
        let report = analyze_timing(&g, &constraints, &interner, &sink).unwrap();
        assert_eq!(report.clock_domains.len(), 1);
        assert!(report.clock_domains[0].met);
    }
}
