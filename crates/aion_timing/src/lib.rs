//! Static timing analysis and constraint management for the Aion FPGA toolchain.
//!
//! This crate provides SDC/XDC constraint parsing, a device-independent timing
//! graph representation, and a static timing analysis (STA) engine. It computes
//! arrival times, required times, and slack at every endpoint, then extracts
//! critical paths for timing closure.
//!
//! # Usage
//!
//! ```ignore
//! use aion_timing::{parse_sdc, analyze_timing, TimingConstraints, TimingGraph};
//!
//! // Parse constraints
//! let constraints = parse_sdc(sdc_source, &interner, &sink);
//!
//! // Build timing graph (from PnR netlist via timing bridge)
//! let graph = build_timing_graph(&netlist, &arch);
//!
//! // Run STA
//! let report = analyze_timing(&graph, &constraints, &interner, &sink)?;
//! println!("Met: {}, worst slack: {:.3} ns", report.met, report.worst_slack_ns);
//! ```
//!
//! # Architecture
//!
//! - [`constraints`] — timing constraint types (clocks, I/O delays, exceptions)
//! - [`sdc`] — SDC/XDC file parser
//! - [`graph`] — device-independent timing graph (nodes + delay edges)
//! - [`sta`] — STA algorithm (forward/backward propagation, slack, critical paths)
//! - [`report`] — timing report types (critical paths, per-domain summaries)

#![warn(missing_docs)]

pub mod constraints;
pub mod graph;
pub mod ids;
pub mod report;
pub mod sdc;
pub mod sta;

pub use constraints::{
    ClockConstraint, FalsePath, IoDelay, MaxDelayPath, MulticyclePath, TimingConstraints,
};
pub use graph::{TimingEdge, TimingEdgeType, TimingGraph, TimingNode, TimingNodeType};
pub use ids::{TimingEdgeId, TimingNodeId};
pub use report::{ClockDomainTiming, CriticalPath, PathElement, TimingEndpoint, TimingReport};
pub use sdc::parse_sdc;
pub use sta::analyze_timing;

#[cfg(test)]
mod tests {
    use super::*;
    use aion_arch::types::Delay;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;

    #[test]
    fn full_pipeline_parse_and_analyze() {
        let sdc_source = r#"
create_clock -period 10.0 -name sys_clk clk
set_input_delay -clock sys_clk 2.0 data_in
set_output_delay -clock sys_clk 1.0 data_out
"#;
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let constraints = parse_sdc(sdc_source, &interner, &sink);
        assert_eq!(constraints.clock_count(), 1);

        // Build a simple timing graph
        let mut graph = TimingGraph::new();
        let inp = graph.add_node("data_in".into(), TimingNodeType::PrimaryInput);
        let lut = graph.add_node("lut_0".into(), TimingNodeType::CellPin);
        let out = graph.add_node("data_out".into(), TimingNodeType::PrimaryOutput);
        graph.add_edge(
            inp,
            lut,
            Delay::new(0.0, 0.0, 2.0),
            TimingEdgeType::NetDelay,
        );
        graph.add_edge(
            lut,
            out,
            Delay::new(0.0, 0.0, 1.5),
            TimingEdgeType::CellDelay,
        );

        let report = analyze_timing(&graph, &constraints, &interner, &sink).unwrap();
        assert!(report.met);
        assert!(!report.critical_paths.is_empty());
    }

    #[test]
    fn full_pipeline_timing_violation() {
        let sdc_source = "create_clock -period 5.0 -name fast_clk clk";
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let constraints = parse_sdc(sdc_source, &interner, &sink);

        let mut graph = TimingGraph::new();
        let inp = graph.add_node("in".into(), TimingNodeType::PrimaryInput);
        let out = graph.add_node("out".into(), TimingNodeType::PrimaryOutput);
        graph.add_edge(
            inp,
            out,
            Delay::new(0.0, 0.0, 8.0),
            TimingEdgeType::NetDelay,
        );

        let report = analyze_timing(&graph, &constraints, &interner, &sink).unwrap();
        assert!(!report.met);
        assert!(report.worst_slack_ns < 0.0);
    }

    #[test]
    fn reexports_available() {
        // Verify all public types are accessible
        let _ = TimingConstraints::new();
        let _ = TimingGraph::new();
        let _ = TimingReport::empty();
        let _ = TimingNodeId::from_raw(0);
        let _ = TimingEdgeId::from_raw(0);
    }
}
