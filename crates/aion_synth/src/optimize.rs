//! Optimization pass runner and pass trait.
//!
//! Provides the [`OptPass`] trait for implementing optimization passes and
//! the [`run_passes`] function that orchestrates them in the correct order.

use crate::netlist::Netlist;
use aion_diagnostics::DiagnosticSink;

/// Trait for a single optimization pass.
///
/// Each pass inspects and modifies the netlist, returning `true` if
/// any changes were made (which may enable further optimization).
pub(crate) trait OptPass {
    /// Runs the pass on the netlist, returning `true` if it made changes.
    fn run(&self, netlist: &mut Netlist, sink: &DiagnosticSink) -> bool;
}

/// Runs all optimization passes on the netlist in the standard order.
///
/// The order is: constant propagation, DCE, CSE, then final DCE cleanup.
/// Each pass is run once (future: iterate until fixpoint for aggressive optimization).
pub(crate) fn run_passes(netlist: &mut Netlist, sink: &DiagnosticSink) {
    let passes: Vec<Box<dyn OptPass>> = vec![
        Box::new(crate::const_prop::ConstPropPass),
        Box::new(crate::dce::DcePass),
        Box::new(crate::cse::CsePass),
        Box::new(crate::dce::DcePass), // Final cleanup
    ];

    for pass in &passes {
        pass.run(netlist, sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use aion_common::Interner;
    use aion_ir::{Arena, Module, TypeDb};
    use aion_source::Span;

    fn empty_netlist(interner: &Interner) -> Netlist<'_> {
        let types = TypeDb::new();
        let mod_name = interner.get_or_intern("test");
        let module = Module {
            id: aion_ir::ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![],
            clock_domains: vec![],
            content_hash: aion_common::ContentHash::from_bytes(b"empty"),
        };
        Netlist::from_module(&module, &types, interner)
    }

    #[test]
    fn run_passes_on_empty_netlist() {
        let interner = Interner::new();
        let mut netlist = empty_netlist(&interner);
        let sink = DiagnosticSink::new();
        run_passes(&mut netlist, &sink);
        assert_eq!(netlist.live_cell_count(), 0);
    }

    #[test]
    fn pass_runner_executes_all() {
        let interner = Interner::new();
        let mut netlist = empty_netlist(&interner);
        let sink = DiagnosticSink::new();
        // Just verifies all passes execute without panicking on empty netlist
        run_passes(&mut netlist, &sink);
    }
}
