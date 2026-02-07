//! Source mapping from IR entities back to their original source locations.
//!
//! The [`SourceMap`] records the source [`Span`] for every IR entity,
//! enabling precise error messages and diagnostics even after elaboration.

use crate::ids::{CellId, ModuleId, ProcessId, SignalId};
use aion_source::Span;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maps IR entity IDs back to their original source spans.
///
/// Every module, signal, cell, and process can be traced back to the
/// exact source location where it was declared or inferred.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceMap {
    /// Module ID → source span.
    module_spans: HashMap<ModuleId, Span>,
    /// (Module, Signal) → source span.
    signal_spans: HashMap<(ModuleId, SignalId), Span>,
    /// (Module, Cell) → source span.
    cell_spans: HashMap<(ModuleId, CellId), Span>,
    /// (Module, Process) → source span.
    process_spans: HashMap<(ModuleId, ProcessId), Span>,
}

impl SourceMap {
    /// Creates a new, empty source map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the source span for a module.
    pub fn insert_module(&mut self, id: ModuleId, span: Span) {
        self.module_spans.insert(id, span);
    }

    /// Records the source span for a signal within a module.
    pub fn insert_signal(&mut self, module: ModuleId, signal: SignalId, span: Span) {
        self.signal_spans.insert((module, signal), span);
    }

    /// Records the source span for a cell within a module.
    pub fn insert_cell(&mut self, module: ModuleId, cell: CellId, span: Span) {
        self.cell_spans.insert((module, cell), span);
    }

    /// Records the source span for a process within a module.
    pub fn insert_process(&mut self, module: ModuleId, process: ProcessId, span: Span) {
        self.process_spans.insert((module, process), span);
    }

    /// Looks up the source span for a module.
    pub fn get_module(&self, id: ModuleId) -> Option<Span> {
        self.module_spans.get(&id).copied()
    }

    /// Looks up the source span for a signal within a module.
    pub fn get_signal(&self, module: ModuleId, signal: SignalId) -> Option<Span> {
        self.signal_spans.get(&(module, signal)).copied()
    }

    /// Looks up the source span for a cell within a module.
    pub fn get_cell(&self, module: ModuleId, cell: CellId) -> Option<Span> {
        self.cell_spans.get(&(module, cell)).copied()
    }

    /// Looks up the source span for a process within a module.
    pub fn get_process(&self, module: ModuleId, process: ProcessId) -> Option<Span> {
        self.process_spans.get(&(module, process)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_source::FileId;

    fn test_span(start: u32, end: u32) -> Span {
        Span::new(FileId::from_raw(0), start, end)
    }

    #[test]
    fn insert_and_get_module() {
        let mut map = SourceMap::new();
        let mid = ModuleId::from_raw(0);
        let span = test_span(10, 20);
        map.insert_module(mid, span);
        assert_eq!(map.get_module(mid), Some(span));
    }

    #[test]
    fn missing_module_returns_none() {
        let map = SourceMap::new();
        assert_eq!(map.get_module(ModuleId::from_raw(99)), None);
    }

    #[test]
    fn insert_and_get_signal() {
        let mut map = SourceMap::new();
        let mid = ModuleId::from_raw(0);
        let sid = SignalId::from_raw(5);
        let span = test_span(30, 40);
        map.insert_signal(mid, sid, span);
        assert_eq!(map.get_signal(mid, sid), Some(span));
    }

    #[test]
    fn insert_and_get_cell() {
        let mut map = SourceMap::new();
        let mid = ModuleId::from_raw(0);
        let cid = CellId::from_raw(3);
        let span = test_span(50, 60);
        map.insert_cell(mid, cid, span);
        assert_eq!(map.get_cell(mid, cid), Some(span));
    }

    #[test]
    fn insert_and_get_process() {
        let mut map = SourceMap::new();
        let mid = ModuleId::from_raw(0);
        let pid = ProcessId::from_raw(7);
        let span = test_span(70, 80);
        map.insert_process(mid, pid, span);
        assert_eq!(map.get_process(mid, pid), Some(span));
    }

    #[test]
    fn signals_scoped_by_module() {
        let mut map = SourceMap::new();
        let m0 = ModuleId::from_raw(0);
        let m1 = ModuleId::from_raw(1);
        let sid = SignalId::from_raw(0);
        let span0 = test_span(0, 10);
        let span1 = test_span(100, 110);
        map.insert_signal(m0, sid, span0);
        map.insert_signal(m1, sid, span1);
        assert_eq!(map.get_signal(m0, sid), Some(span0));
        assert_eq!(map.get_signal(m1, sid), Some(span1));
    }
}
