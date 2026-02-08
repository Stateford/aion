//! TUI state management.
//!
//! Contains the viewport (time window and zoom level), input mode,
//! focus tracking, and signal selection state.

use std::collections::HashSet;

/// Which panel currently has keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusedPanel {
    /// The signal list on the left.
    SignalList,
    /// The waveform viewer in the center.
    Waveform,
    /// The command input bar at the bottom.
    CommandInput,
}

/// Current input mode of the TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    /// Normal navigation mode (vim-like keys).
    Normal,
    /// Command entry mode (`:` prefix).
    Command,
}

/// The time viewport controlling which portion of the waveform is visible.
#[derive(Clone, Debug)]
pub struct ViewPort {
    /// Left edge of the viewport in femtoseconds.
    pub start_fs: u64,
    /// Right edge of the viewport in femtoseconds.
    pub end_fs: u64,
    /// Minimum duration the viewport can show (zoom limit) in femtoseconds.
    pub min_span_fs: u64,
}

impl ViewPort {
    /// Creates a new viewport spanning from 0 to the given end time.
    pub fn new(end_fs: u64) -> Self {
        Self {
            start_fs: 0,
            end_fs: end_fs.max(1000),
            min_span_fs: 100,
        }
    }

    /// Returns the duration of the viewport in femtoseconds.
    pub fn span_fs(&self) -> u64 {
        self.end_fs.saturating_sub(self.start_fs)
    }

    /// Converts a time in femtoseconds to a column position within the given width.
    ///
    /// Returns `None` if the time is outside the viewport.
    pub fn time_to_col(&self, time_fs: u64, width: u16) -> Option<u16> {
        if time_fs < self.start_fs || time_fs > self.end_fs || width == 0 {
            return None;
        }
        let span = self.span_fs();
        if span == 0 {
            return Some(0);
        }
        let offset = time_fs - self.start_fs;
        let col = (offset as f64 / span as f64 * (width - 1) as f64) as u16;
        Some(col.min(width - 1))
    }

    /// Converts a column position to a time in femtoseconds.
    pub fn col_to_time(&self, col: u16, width: u16) -> u64 {
        if width <= 1 {
            return self.start_fs;
        }
        let span = self.span_fs();
        let frac = col as f64 / (width - 1) as f64;
        self.start_fs + (frac * span as f64) as u64
    }

    /// Zooms in by halving the visible span, centered on the given time.
    pub fn zoom_in(&mut self, center_fs: u64) {
        let span = self.span_fs();
        let new_span = (span / 2).max(self.min_span_fs);
        self.set_span_around(center_fs, new_span);
    }

    /// Zooms out by doubling the visible span, centered on the given time.
    pub fn zoom_out(&mut self, center_fs: u64) {
        let span = self.span_fs();
        let new_span = span.saturating_mul(2);
        self.set_span_around(center_fs, new_span);
    }

    /// Scrolls the viewport left by one quarter of its span.
    pub fn scroll_left(&mut self) {
        let delta = self.span_fs() / 4;
        if self.start_fs >= delta {
            self.start_fs -= delta;
            self.end_fs -= delta;
        } else {
            let span = self.span_fs();
            self.start_fs = 0;
            self.end_fs = span;
        }
    }

    /// Scrolls the viewport right by one quarter of its span.
    pub fn scroll_right(&mut self) {
        let delta = self.span_fs() / 4;
        self.start_fs = self.start_fs.saturating_add(delta);
        self.end_fs = self.end_fs.saturating_add(delta);
    }

    /// Fits the viewport to show from time 0 to the given end time.
    pub fn fit(&mut self, max_time_fs: u64) {
        self.start_fs = 0;
        self.end_fs = max_time_fs.max(1000);
    }

    /// Sets the viewport span centered on the given time.
    fn set_span_around(&mut self, center_fs: u64, new_span: u64) {
        let half = new_span / 2;
        if center_fs >= half {
            self.start_fs = center_fs - half;
        } else {
            self.start_fs = 0;
        }
        self.end_fs = self.start_fs + new_span;
    }
}

/// Full TUI state.
#[derive(Clone, Debug)]
pub struct TuiState {
    /// Current input mode.
    pub mode: InputMode,
    /// Which panel has focus.
    pub focused: FocusedPanel,
    /// Time viewport for waveform display.
    pub viewport: ViewPort,
    /// Index of the currently selected signal in the signal list.
    pub selected_signal: usize,
    /// Total number of signals available.
    pub signal_count: usize,
    /// The cursor time position in femtoseconds.
    pub cursor_fs: u64,
    /// Text currently being typed in command mode.
    pub command_buffer: String,
    /// Whether the simulation is auto-running.
    pub auto_running: bool,
    /// Status message displayed in the status bar.
    pub status_message: String,
    /// Display output from $display calls.
    pub display_output: Vec<String>,
    /// Value display format for the selected signal.
    pub value_format: ValueFormat,
    /// Signals selected for waveform display (indices into all_signals).
    pub waveform_signals: Vec<usize>,
    /// Whether help popup is visible.
    pub show_help: bool,
    /// Set of signal indices whose bus bits are expanded in the waveform.
    pub expanded_signals: HashSet<usize>,
}

/// How signal values are displayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueFormat {
    /// Hexadecimal format.
    Hex,
    /// Binary format.
    Binary,
    /// Decimal format.
    Decimal,
}

impl ValueFormat {
    /// Cycles to the next display format.
    pub fn cycle(self) -> Self {
        match self {
            Self::Hex => Self::Binary,
            Self::Binary => Self::Decimal,
            Self::Decimal => Self::Hex,
        }
    }
}

impl TuiState {
    /// Creates a new TUI state with default settings.
    pub fn new(signal_count: usize) -> Self {
        let waveform_signals: Vec<usize> = (0..signal_count).collect();
        Self {
            mode: InputMode::Normal,
            focused: FocusedPanel::SignalList,
            viewport: ViewPort::new(100_000_000), // 100ns default
            selected_signal: 0,
            signal_count,
            cursor_fs: 0,
            command_buffer: String::new(),
            auto_running: false,
            status_message: String::new(),
            display_output: Vec::new(),
            value_format: ValueFormat::Hex,
            waveform_signals,
            show_help: false,
            expanded_signals: HashSet::new(),
        }
    }

    /// Moves the signal selection up.
    pub fn select_prev_signal(&mut self) {
        if self.selected_signal > 0 {
            self.selected_signal -= 1;
        }
    }

    /// Moves the signal selection down.
    pub fn select_next_signal(&mut self) {
        if self.signal_count > 0 && self.selected_signal < self.signal_count - 1 {
            self.selected_signal += 1;
        }
    }

    /// Toggles the selected signal in/out of the waveform display.
    pub fn toggle_waveform_signal(&mut self) {
        if self.selected_signal >= self.signal_count {
            return;
        }
        let idx = self.selected_signal;
        if let Some(pos) = self.waveform_signals.iter().position(|&s| s == idx) {
            self.waveform_signals.remove(pos);
        } else {
            self.waveform_signals.push(idx);
        }
    }

    /// Moves cursor left by one column increment.
    pub fn cursor_left(&mut self, width: u16) {
        let step = self.viewport.span_fs() / width.max(1) as u64;
        self.cursor_fs = self.cursor_fs.saturating_sub(step.max(1));
    }

    /// Moves cursor right by one column increment.
    pub fn cursor_right(&mut self, width: u16) {
        let step = self.viewport.span_fs() / width.max(1) as u64;
        self.cursor_fs = self.cursor_fs.saturating_add(step.max(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_new_defaults() {
        let vp = ViewPort::new(1_000_000);
        assert_eq!(vp.start_fs, 0);
        assert_eq!(vp.end_fs, 1_000_000);
    }

    #[test]
    fn viewport_span() {
        let vp = ViewPort::new(5000);
        assert_eq!(vp.span_fs(), 5000);
    }

    #[test]
    fn viewport_time_to_col() {
        let vp = ViewPort {
            start_fs: 0,
            end_fs: 100,
            min_span_fs: 10,
        };
        assert_eq!(vp.time_to_col(0, 101), Some(0));
        assert_eq!(vp.time_to_col(50, 101), Some(50));
        assert_eq!(vp.time_to_col(100, 101), Some(100));
    }

    #[test]
    fn viewport_time_to_col_outside() {
        let vp = ViewPort {
            start_fs: 0,
            end_fs: 100,
            min_span_fs: 10,
        };
        assert_eq!(vp.time_to_col(200, 101), None);
    }

    #[test]
    fn viewport_col_to_time() {
        let vp = ViewPort {
            start_fs: 0,
            end_fs: 100,
            min_span_fs: 10,
        };
        assert_eq!(vp.col_to_time(0, 101), 0);
        assert_eq!(vp.col_to_time(100, 101), 100);
    }

    #[test]
    fn viewport_zoom_in() {
        let mut vp = ViewPort::new(1000);
        vp.zoom_in(500);
        assert_eq!(vp.span_fs(), 500);
        // Should be centered around 500
        assert_eq!(vp.start_fs, 250);
        assert_eq!(vp.end_fs, 750);
    }

    #[test]
    fn viewport_zoom_out() {
        let mut vp = ViewPort::new(1000);
        vp.zoom_out(500);
        assert_eq!(vp.span_fs(), 2000);
    }

    #[test]
    fn viewport_zoom_in_min_span() {
        let mut vp = ViewPort {
            start_fs: 0,
            end_fs: 100,
            min_span_fs: 100,
        };
        vp.zoom_in(50);
        // Should not go below min_span_fs
        assert_eq!(vp.span_fs(), 100);
    }

    #[test]
    fn viewport_scroll_left() {
        let mut vp = ViewPort::new(1000);
        vp.start_fs = 500;
        vp.end_fs = 1500;
        vp.scroll_left();
        assert_eq!(vp.start_fs, 250);
        assert_eq!(vp.end_fs, 1250);
    }

    #[test]
    fn viewport_scroll_left_clamp() {
        let mut vp = ViewPort::new(1000);
        vp.scroll_left();
        assert_eq!(vp.start_fs, 0);
    }

    #[test]
    fn viewport_scroll_right() {
        let mut vp = ViewPort::new(1000);
        vp.scroll_right();
        assert_eq!(vp.start_fs, 250);
        assert_eq!(vp.end_fs, 1250);
    }

    #[test]
    fn viewport_fit() {
        let mut vp = ViewPort::new(1000);
        vp.start_fs = 500;
        vp.end_fs = 1500;
        vp.fit(2000);
        assert_eq!(vp.start_fs, 0);
        assert_eq!(vp.end_fs, 2000);
    }

    #[test]
    fn state_new_defaults() {
        let state = TuiState::new(5);
        assert_eq!(state.mode, InputMode::Normal);
        assert_eq!(state.focused, FocusedPanel::SignalList);
        assert_eq!(state.selected_signal, 0);
        assert_eq!(state.signal_count, 5);
        assert_eq!(state.waveform_signals.len(), 5);
    }

    #[test]
    fn state_select_prev_next() {
        let mut state = TuiState::new(5);
        state.select_next_signal();
        assert_eq!(state.selected_signal, 1);
        state.select_next_signal();
        assert_eq!(state.selected_signal, 2);
        state.select_prev_signal();
        assert_eq!(state.selected_signal, 1);
    }

    #[test]
    fn state_select_bounds() {
        let mut state = TuiState::new(3);
        state.select_prev_signal(); // already at 0
        assert_eq!(state.selected_signal, 0);
        state.selected_signal = 2;
        state.select_next_signal(); // already at end
        assert_eq!(state.selected_signal, 2);
    }

    #[test]
    fn state_toggle_waveform_signal() {
        let mut state = TuiState::new(3);
        assert_eq!(state.waveform_signals.len(), 3);
        state.selected_signal = 1;
        state.toggle_waveform_signal(); // remove
        assert_eq!(state.waveform_signals.len(), 2);
        assert!(!state.waveform_signals.contains(&1));
        state.toggle_waveform_signal(); // add back
        assert_eq!(state.waveform_signals.len(), 3);
    }

    #[test]
    fn state_cursor_movement() {
        let mut state = TuiState::new(1);
        state.viewport = ViewPort::new(1000);
        state.cursor_fs = 500;
        state.cursor_left(100);
        assert!(state.cursor_fs < 500);
        let pos = state.cursor_fs;
        state.cursor_right(100);
        assert!(state.cursor_fs > pos);
    }

    #[test]
    fn value_format_cycle() {
        assert_eq!(ValueFormat::Hex.cycle(), ValueFormat::Binary);
        assert_eq!(ValueFormat::Binary.cycle(), ValueFormat::Decimal);
        assert_eq!(ValueFormat::Decimal.cycle(), ValueFormat::Hex);
    }

    #[test]
    fn state_expanded_signals_default_empty() {
        let state = TuiState::new(3);
        assert!(state.expanded_signals.is_empty());
    }

    #[test]
    fn input_mode_eq() {
        assert_eq!(InputMode::Normal, InputMode::Normal);
        assert_ne!(InputMode::Normal, InputMode::Command);
    }

    #[test]
    fn focused_panel_eq() {
        assert_eq!(FocusedPanel::Waveform, FocusedPanel::Waveform);
        assert_ne!(FocusedPanel::Waveform, FocusedPanel::SignalList);
    }
}
