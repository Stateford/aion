//! In-memory value change database for waveform display.
//!
//! The simulation kernel only exposes current signal values. This module
//! captures signal values after each simulation step, building a per-signal
//! change log that supports efficient time-based lookup for waveform rendering.

use aion_common::LogicVec;
use aion_sim::SimSignalId;

/// A single value change event for a signal.
#[derive(Clone, Debug)]
pub struct ValueChange {
    /// Time of the change in femtoseconds.
    pub time_fs: u64,
    /// The new value at this time.
    pub value: LogicVec,
}

/// History of value changes for a single signal.
#[derive(Clone, Debug)]
pub struct SignalHistory {
    /// Signal identifier.
    pub id: SimSignalId,
    /// Signal name (hierarchical).
    pub name: String,
    /// Signal bit width.
    pub width: u32,
    /// Ordered list of value changes (sorted by time).
    pub changes: Vec<ValueChange>,
}

impl SignalHistory {
    /// Creates a new empty signal history.
    pub fn new(id: SimSignalId, name: String, width: u32) -> Self {
        Self {
            id,
            name,
            width,
            changes: Vec::new(),
        }
    }

    /// Records a value change at the given time.
    ///
    /// Only records if the value actually changed from the previous value.
    pub fn record(&mut self, time_fs: u64, value: LogicVec) {
        if let Some(last) = self.changes.last() {
            if last.value == value {
                return; // no change
            }
        }
        self.changes.push(ValueChange { time_fs, value });
    }

    /// Returns the value at a given time, or `None` if no data exists before that time.
    ///
    /// Uses binary search for efficient lookup.
    pub fn value_at(&self, time_fs: u64) -> Option<&LogicVec> {
        if self.changes.is_empty() {
            return None;
        }
        match self.changes.binary_search_by_key(&time_fs, |c| c.time_fs) {
            Ok(idx) => Some(&self.changes[idx].value),
            Err(0) => None, // before any changes
            Err(idx) => Some(&self.changes[idx - 1].value),
        }
    }

    /// Returns all value changes within a time range (inclusive).
    pub fn changes_in_range(&self, start_fs: u64, end_fs: u64) -> &[ValueChange] {
        if self.changes.is_empty() {
            return &[];
        }

        let start_idx = match self.changes.binary_search_by_key(&start_fs, |c| c.time_fs) {
            Ok(idx) => idx,
            Err(idx) => idx,
        };

        let end_idx = match self.changes.binary_search_by_key(&end_fs, |c| c.time_fs) {
            Ok(idx) => idx + 1, // inclusive
            Err(idx) => idx,
        };

        &self.changes[start_idx..end_idx]
    }

    /// Returns the maximum time recorded, or 0 if empty.
    pub fn max_time(&self) -> u64 {
        self.changes.last().map_or(0, |c| c.time_fs)
    }
}

/// In-memory waveform data capturing all signal histories.
#[derive(Clone, Debug)]
pub struct WaveformData {
    /// Per-signal histories, indexed by order of registration.
    pub signals: Vec<SignalHistory>,
}

impl WaveformData {
    /// Creates a new empty waveform data store.
    pub fn new() -> Self {
        Self {
            signals: Vec::new(),
        }
    }

    /// Registers a signal for tracking.
    ///
    /// Returns the index of the signal in the data store.
    pub fn register(&mut self, id: SimSignalId, name: String, width: u32) -> usize {
        let idx = self.signals.len();
        self.signals.push(SignalHistory::new(id, name, width));
        idx
    }

    /// Records a value change for a signal by its data store index.
    pub fn record(&mut self, signal_idx: usize, time_fs: u64, value: LogicVec) {
        if let Some(history) = self.signals.get_mut(signal_idx) {
            history.record(time_fs, value);
        }
    }

    /// Snapshots the current value of all signals from the kernel.
    ///
    /// Calls the provided closure for each registered signal to get its current value.
    pub fn snapshot_all<F>(&mut self, time_fs: u64, mut get_value: F)
    where
        F: FnMut(SimSignalId) -> LogicVec,
    {
        for history in &mut self.signals {
            let value = get_value(history.id);
            history.record(time_fs, value);
        }
    }

    /// Returns the maximum time across all signals.
    pub fn max_time(&self) -> u64 {
        self.signals.iter().map(|s| s.max_time()).max().unwrap_or(0)
    }

    /// Returns the number of registered signals.
    pub fn signal_count(&self) -> usize {
        self.signals.len()
    }
}

impl Default for WaveformData {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::LogicVec;

    fn make_id(raw: u32) -> SimSignalId {
        SimSignalId::from_raw(raw)
    }

    #[test]
    fn signal_history_record_and_lookup() {
        let mut hist = SignalHistory::new(make_id(0), "sig".into(), 1);
        hist.record(0, LogicVec::from_bool(false));
        hist.record(100, LogicVec::from_bool(true));
        hist.record(200, LogicVec::from_bool(false));

        assert_eq!(hist.value_at(0).unwrap(), &LogicVec::from_bool(false));
        assert_eq!(hist.value_at(50).unwrap(), &LogicVec::from_bool(false));
        assert_eq!(hist.value_at(100).unwrap(), &LogicVec::from_bool(true));
        assert_eq!(hist.value_at(150).unwrap(), &LogicVec::from_bool(true));
        assert_eq!(hist.value_at(200).unwrap(), &LogicVec::from_bool(false));
    }

    #[test]
    fn signal_history_value_at_before_any() {
        let hist = SignalHistory::new(make_id(0), "sig".into(), 1);
        assert!(hist.value_at(0).is_none());
    }

    #[test]
    fn signal_history_dedup() {
        let mut hist = SignalHistory::new(make_id(0), "sig".into(), 1);
        hist.record(0, LogicVec::from_bool(false));
        hist.record(100, LogicVec::from_bool(false)); // same value, should be deduped
        assert_eq!(hist.changes.len(), 1);
    }

    #[test]
    fn signal_history_changes_in_range() {
        let mut hist = SignalHistory::new(make_id(0), "sig".into(), 1);
        hist.record(0, LogicVec::from_bool(false));
        hist.record(100, LogicVec::from_bool(true));
        hist.record(200, LogicVec::from_bool(false));
        hist.record(300, LogicVec::from_bool(true));

        let range = hist.changes_in_range(100, 200);
        assert_eq!(range.len(), 2);
        assert_eq!(range[0].time_fs, 100);
        assert_eq!(range[1].time_fs, 200);
    }

    #[test]
    fn signal_history_changes_in_range_empty() {
        let hist = SignalHistory::new(make_id(0), "sig".into(), 1);
        assert!(hist.changes_in_range(0, 100).is_empty());
    }

    #[test]
    fn signal_history_max_time() {
        let mut hist = SignalHistory::new(make_id(0), "sig".into(), 1);
        assert_eq!(hist.max_time(), 0);
        hist.record(0, LogicVec::from_bool(false));
        hist.record(500, LogicVec::from_bool(true));
        assert_eq!(hist.max_time(), 500);
    }

    #[test]
    fn waveform_data_register_and_record() {
        let mut data = WaveformData::new();
        let idx = data.register(make_id(0), "clk".into(), 1);
        assert_eq!(idx, 0);
        data.record(idx, 0, LogicVec::from_bool(false));
        data.record(idx, 100, LogicVec::from_bool(true));

        assert_eq!(data.signals[0].changes.len(), 2);
    }

    #[test]
    fn waveform_data_snapshot_all() {
        let mut data = WaveformData::new();
        let id0 = make_id(0);
        let id1 = make_id(1);
        data.register(id0, "a".into(), 1);
        data.register(id1, "b".into(), 1);

        data.snapshot_all(0, |id| {
            if id == id0 {
                LogicVec::from_bool(false)
            } else {
                LogicVec::from_bool(true)
            }
        });

        assert_eq!(data.signals[0].changes.len(), 1);
        assert_eq!(data.signals[1].changes.len(), 1);
    }

    #[test]
    fn waveform_data_max_time() {
        let mut data = WaveformData::new();
        data.register(make_id(0), "a".into(), 1);
        data.register(make_id(1), "b".into(), 1);
        data.record(0, 100, LogicVec::from_bool(true));
        data.record(1, 200, LogicVec::from_bool(true));
        assert_eq!(data.max_time(), 200);
    }

    #[test]
    fn waveform_data_default() {
        let data = WaveformData::default();
        assert_eq!(data.signal_count(), 0);
    }

    #[test]
    fn waveform_data_signal_count() {
        let mut data = WaveformData::new();
        data.register(make_id(0), "a".into(), 1);
        data.register(make_id(1), "b".into(), 8);
        assert_eq!(data.signal_count(), 2);
    }

    #[test]
    fn signal_history_multi_bit() {
        let mut hist = SignalHistory::new(make_id(0), "bus".into(), 8);
        hist.record(0, LogicVec::from_u64(0x00, 8));
        hist.record(100, LogicVec::from_u64(0xFF, 8));
        hist.record(200, LogicVec::from_u64(0x42, 8));

        assert_eq!(hist.value_at(0).unwrap().to_u64(), Some(0x00));
        assert_eq!(hist.value_at(150).unwrap().to_u64(), Some(0xFF));
        assert_eq!(hist.value_at(200).unwrap().to_u64(), Some(0x42));
    }

    #[test]
    fn waveform_data_record_out_of_bounds() {
        let mut data = WaveformData::new();
        // Recording to a non-existent signal index should not panic
        data.record(999, 0, LogicVec::from_bool(false));
    }

    #[test]
    fn signal_history_value_at_exact_boundary() {
        let mut hist = SignalHistory::new(make_id(0), "sig".into(), 1);
        hist.record(100, LogicVec::from_bool(true));
        // Before first change
        assert!(hist.value_at(50).is_none());
        // Exactly at change
        assert_eq!(hist.value_at(100).unwrap(), &LogicVec::from_bool(true));
        // After change
        assert_eq!(hist.value_at(200).unwrap(), &LogicVec::from_bool(true));
    }
}
