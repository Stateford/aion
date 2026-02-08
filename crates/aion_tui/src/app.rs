//! TUI application core.
//!
//! [`TuiApp`] owns the simulation kernel (if in simulation mode), waveform
//! data, and TUI state. It provides the main event loop and coordinates
//! simulation stepping with UI rendering. In viewer mode, the kernel is
//! absent and only pre-loaded waveform data is displayed.

use aion_common::Interner;
use aion_ir::Design;
use aion_sim::interactive::{format_value, SimCommand};
use aion_sim::kernel::StepResult;
use aion_sim::{SimError, SimKernel};

use crate::commands::{parse_tui_command, TuiCommand};
use crate::state::{FocusedPanel, InputMode, TuiState};
use crate::waveform_data::WaveformData;

/// Cached information about a simulation signal.
#[derive(Clone, Debug)]
pub struct SignalInfo {
    /// Signal identifier in the simulation kernel.
    pub id: aion_sim::SimSignalId,
    /// Hierarchical signal name.
    pub name: String,
    /// Bit width of the signal.
    pub width: u32,
}

/// Operating mode for the TUI application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiMode {
    /// Live simulation with a kernel — stepping and running are available.
    Simulation,
    /// Read-only viewer for pre-loaded waveform data — no kernel present.
    Viewer,
}

/// The core TUI application state.
///
/// Owns the simulation kernel (if in simulation mode), waveform history,
/// and UI state. Provides methods for stepping the simulation, handling
/// key events, and executing commands. In viewer mode, simulation commands
/// are unavailable and only waveform navigation is supported.
pub struct TuiApp {
    /// The simulation kernel, present only in simulation mode.
    pub kernel: Option<SimKernel>,
    /// The current operating mode.
    pub mode: TuiMode,
    /// Waveform value change history.
    pub waveform: WaveformData,
    /// UI state (viewport, selection, mode, etc.).
    pub state: TuiState,
    /// Cached signal info: (id, name, width) for each signal.
    pub signal_info: Vec<SignalInfo>,
    /// Whether the simulation is initialized.
    pub initialized: bool,
    /// Whether the application should quit.
    pub should_quit: bool,
}

impl TuiApp {
    /// Creates a new TUI application from an elaborated design.
    ///
    /// The `interner` is used to resolve interned signal names.
    pub fn new(design: &Design, interner: &Interner) -> Result<Self, SimError> {
        let kernel = SimKernel::new(design, interner)?;
        let raw_signals = kernel.all_signals();
        let mut waveform = WaveformData::new();

        let signal_info: Vec<SignalInfo> = raw_signals
            .iter()
            .map(|(id, name, width)| {
                waveform.register(*id, name.to_string(), *width);
                SignalInfo {
                    id: *id,
                    name: name.to_string(),
                    width: *width,
                }
            })
            .collect();

        let state = TuiState::new(signal_info.len());

        Ok(Self {
            kernel: Some(kernel),
            mode: TuiMode::Simulation,
            waveform,
            state,
            signal_info,
            initialized: false,
            should_quit: false,
        })
    }

    /// Creates a TUI application in viewer mode from pre-loaded waveform data.
    ///
    /// No simulation kernel is created. Simulation commands (step, run, etc.)
    /// are unavailable; only waveform navigation and inspection are supported.
    pub fn from_waveform(waveform: WaveformData, signal_info: Vec<SignalInfo>) -> Self {
        let state = TuiState::new(signal_info.len());
        Self {
            kernel: None,
            mode: TuiMode::Viewer,
            waveform,
            state,
            signal_info,
            initialized: true,
            should_quit: false,
        }
    }

    /// Initializes the simulation kernel.
    ///
    /// Must be called once before stepping. Records the initial signal values.
    /// No-op in viewer mode.
    pub fn initialize(&mut self) -> Result<(), SimError> {
        if self.initialized {
            return Ok(());
        }
        if let Some(ref mut kernel) = self.kernel {
            kernel.initialize()?;
        }
        self.initialized = true;
        self.snapshot_signals();

        // Collect initial display output
        if let Some(ref mut kernel) = self.kernel {
            let output = kernel.take_display_output();
            self.state.display_output.extend(output);
        }
        Ok(())
    }

    /// Advances the simulation to the next meaningful time point and records signal values.
    ///
    /// Uses `run_until()` to process both events and suspended process wakeups,
    /// ensuring delay-based scheduling is honored.
    ///
    /// Returns an error in viewer mode since no simulation kernel is present.
    pub fn step(&mut self) -> Result<StepResult, SimError> {
        let kernel = self.kernel.as_mut().ok_or_else(|| SimError::Other {
            message: "not available in viewer mode".into(),
        })?;

        let result = if let Some(next_t) = kernel.next_event_time_fs() {
            kernel.run_until(next_t)?
        } else {
            StepResult::Done
        };
        self.snapshot_signals();

        // Collect display output
        let output = self.kernel.as_mut().unwrap().take_display_output();
        self.state.display_output.extend(output);

        Ok(result)
    }

    /// Runs the simulation for the given duration in femtoseconds.
    ///
    /// Steps through each event time up to the target, snapshotting signal
    /// values at every event so that the waveform captures all intermediate
    /// transitions (clock toggles, counter increments, etc.).
    ///
    /// Returns an error in viewer mode.
    pub fn run_for(&mut self, duration_fs: u64) -> Result<(), SimError> {
        if self.kernel.is_none() {
            return Err(SimError::Other {
                message: "not available in viewer mode".into(),
            });
        }
        let target_fs = self.kernel.as_ref().unwrap().current_time().fs + duration_fs;

        loop {
            let kernel = self.kernel.as_mut().unwrap();
            if kernel.is_finished() {
                break;
            }
            let next_t = match kernel.next_event_time_fs() {
                Some(t) if t <= target_fs => t,
                _ => break,
            };
            kernel.run_until(next_t)?;
            self.snapshot_signals();
        }

        // Advance time to target even if no more events
        {
            let kernel = self.kernel.as_mut().unwrap();
            if kernel.current_time().fs < target_fs && !kernel.is_finished() {
                kernel.run_until(target_fs)?;
            }
        }
        self.snapshot_signals();

        let kernel = self.kernel.as_mut().unwrap();
        let output = kernel.take_display_output();
        self.state.display_output.extend(output);

        Ok(())
    }

    /// Returns the formatted value of a signal at the current cursor time.
    ///
    /// Looks up the value from the waveform history at `cursor_fs` so the
    /// signal list panel shows the value at the cursor, not the final sim
    /// time. Falls back to the kernel's current value when no waveform data
    /// exists yet (simulation mode only).
    pub fn signal_value_str(&self, signal_idx: usize) -> String {
        // Try waveform history at cursor time first
        if let Some(history) = self.waveform.signals.get(signal_idx) {
            if let Some(val) = history.value_at(self.state.cursor_fs) {
                return format_value(val);
            }
        }
        // Fall back to current kernel value (simulation mode only)
        if let Some(ref kernel) = self.kernel {
            if let Some(info) = self.signal_info.get(signal_idx) {
                let val = kernel.signal_value(info.id);
                return format_value(val);
            }
        }
        "?".into()
    }

    /// Returns the current simulation time as a formatted string.
    ///
    /// In viewer mode, shows the cursor time or waveform max time.
    pub fn time_str(&self) -> String {
        if let Some(ref kernel) = self.kernel {
            format!("{}", kernel.current_time())
        } else {
            format!("{}", aion_sim::SimTime::from_fs(self.state.cursor_fs))
        }
    }

    /// Returns whether the simulation is finished.
    ///
    /// In viewer mode, always returns `true`.
    pub fn is_finished(&self) -> bool {
        self.kernel.as_ref().is_none_or(|k| k.is_finished())
    }

    /// Returns whether the simulation has pending events.
    ///
    /// In viewer mode, always returns `false`.
    pub fn has_pending_events(&self) -> bool {
        self.kernel.as_ref().is_some_and(|k| k.has_pending_events())
    }

    /// Executes a TUI command.
    pub fn execute_command(&mut self, input: &str) -> Result<String, String> {
        let cmd = parse_tui_command(input)?;
        match cmd {
            TuiCommand::Sim(sim_cmd) => self.execute_sim_command(&sim_cmd),
            TuiCommand::ZoomIn => {
                self.state.viewport.zoom_in(self.state.cursor_fs);
                Ok("Zoomed in".into())
            }
            TuiCommand::ZoomOut => {
                self.state.viewport.zoom_out(self.state.cursor_fs);
                Ok("Zoomed out".into())
            }
            TuiCommand::ZoomFit => {
                let max_t = self.waveform.max_time();
                let kernel_t = self.kernel.as_ref().map_or(0, |k| k.current_time().fs);
                self.state.viewport.fit(max_t.max(kernel_t));
                Ok("Fit to simulation range".into())
            }
            TuiCommand::Goto { time_fs } => {
                self.state.cursor_fs = time_fs;
                Ok(format!("Cursor at {}", aion_sim::SimTime::from_fs(time_fs)))
            }
            TuiCommand::AddSignal { name } => self.add_signal_to_waveform(&name),
            TuiCommand::RemoveSignal { name } => self.remove_signal_from_waveform(&name),
            TuiCommand::CycleFormat => {
                self.state.value_format = self.state.value_format.cycle();
                Ok(format!("Format: {:?}", self.state.value_format))
            }
            TuiCommand::ToggleHelp => {
                self.state.show_help = !self.state.show_help;
                Ok(String::new())
            }
        }
    }

    /// Executes a simulation command and returns a status message.
    fn execute_sim_command(&mut self, cmd: &SimCommand) -> Result<String, String> {
        if self.mode == TuiMode::Viewer {
            return match cmd {
                SimCommand::Quit => {
                    self.should_quit = true;
                    Ok("Quitting".into())
                }
                SimCommand::Help => Ok(help_text()),
                SimCommand::Signals => {
                    let mut s = format!("{} signal(s):\n", self.signal_info.len());
                    for info in &self.signal_info {
                        s.push_str(&format!("  {} [{} bit]\n", info.name, info.width));
                    }
                    Ok(s.trim_end().to_string())
                }
                SimCommand::Time => Ok(format!("Time: {}", self.time_str())),
                SimCommand::Inspect { signals } => {
                    let mut output = String::new();
                    for name in signals {
                        let mut found = false;
                        for (i, info) in self.signal_info.iter().enumerate() {
                            if info.name == *name || info.name.ends_with(&format!(".{name}")) {
                                let val_str = self.signal_value_str(i);
                                output.push_str(&format!("{} = {val_str}\n", info.name));
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            output.push_str(&format!("Signal not found: {name}\n"));
                        }
                    }
                    Ok(output.trim_end().to_string())
                }
                _ => Err("not available in viewer mode".into()),
            };
        }

        match cmd {
            SimCommand::Run { duration_fs } => {
                self.run_for(*duration_fs).map_err(|e| e.to_string())?;
                Ok(format!("Ran to {}", self.time_str()))
            }
            SimCommand::Step => {
                let result = self.step().map_err(|e| e.to_string())?;
                match result {
                    StepResult::Continued => Ok(format!("Stepped to {}", self.time_str())),
                    StepResult::Done => Ok("Simulation finished".into()),
                }
            }
            SimCommand::Inspect { signals } => {
                let kernel = self.kernel.as_ref().unwrap();
                let mut output = String::new();
                for name in signals {
                    match kernel.find_signal(name) {
                        Some(id) => {
                            let val = kernel.signal_value(id);
                            output.push_str(&format!("{name} = {}\n", format_value(val)));
                        }
                        None => {
                            output.push_str(&format!("Signal not found: {name}\n"));
                        }
                    }
                }
                Ok(output.trim_end().to_string())
            }
            SimCommand::Time => Ok(format!("Time: {}", self.time_str())),
            SimCommand::Signals => {
                let mut s = format!("{} signal(s):\n", self.signal_info.len());
                for info in &self.signal_info {
                    s.push_str(&format!("  {} [{} bit]\n", info.name, info.width));
                }
                Ok(s.trim_end().to_string())
            }
            SimCommand::Quit => {
                self.should_quit = true;
                Ok("Quitting".into())
            }
            SimCommand::Help => Ok(help_text()),
            SimCommand::Status => {
                let finished = self.is_finished();
                Ok(format!(
                    "Time: {}\nSignals: {}\nFinished: {finished}",
                    self.time_str(),
                    self.signal_info.len(),
                ))
            }
            SimCommand::Continue => {
                while !self.is_finished() && self.has_pending_events() {
                    let result = self.step().map_err(|e| e.to_string())?;
                    if result == StepResult::Done {
                        break;
                    }
                }
                Ok(format!("Continued to {}", self.time_str()))
            }
            _ => Ok("Command not yet supported in TUI".into()),
        }
    }

    /// Toggles bus expansion for the currently selected signal.
    ///
    /// Only multi-bit signals can be expanded. When expanded, individual
    /// bit traces are shown below the bus in the waveform panel.
    pub fn toggle_expand(&mut self) {
        let idx = self.state.selected_signal;
        if let Some(info) = self.signal_info.get(idx) {
            if info.width > 1 && !self.state.expanded_signals.remove(&idx) {
                self.state.expanded_signals.insert(idx);
            }
        }
    }

    /// Returns the formatted value of a single bit at the current cursor time.
    pub fn bit_value_str(&self, signal_idx: usize, bit: u32) -> String {
        if let Some(history) = self.waveform.signals.get(signal_idx) {
            if let Some(logic) = history.bit_value_at(self.state.cursor_fs, bit) {
                return match logic {
                    aion_common::Logic::Zero => "0",
                    aion_common::Logic::One => "1",
                    aion_common::Logic::X => "x",
                    aion_common::Logic::Z => "z",
                }
                .into();
            }
        }
        "?".into()
    }

    /// Snapshots current signal values into waveform data.
    fn snapshot_signals(&mut self) {
        if let Some(ref kernel) = self.kernel {
            let time_fs = kernel.current_time().fs;
            for (i, info) in self.signal_info.iter().enumerate() {
                let val = kernel.signal_value(info.id).clone();
                self.waveform.record(i, time_fs, val);
            }
        }
    }

    /// Adds a signal to the waveform display by name pattern.
    fn add_signal_to_waveform(&mut self, name: &str) -> Result<String, String> {
        let mut added = 0;
        for (i, info) in self.signal_info.iter().enumerate() {
            if info.name.contains(name) && !self.state.waveform_signals.contains(&i) {
                self.state.waveform_signals.push(i);
                added += 1;
            }
        }
        if added > 0 {
            Ok(format!("Added {added} signal(s)"))
        } else {
            Err(format!("No matching signal found for '{name}'"))
        }
    }

    /// Removes a signal from the waveform display by name pattern.
    fn remove_signal_from_waveform(&mut self, name: &str) -> Result<String, String> {
        let before = self.state.waveform_signals.len();
        self.state.waveform_signals.retain(|&i| {
            self.signal_info
                .get(i)
                .is_none_or(|info| !info.name.contains(name))
        });
        let removed = before - self.state.waveform_signals.len();
        if removed > 0 {
            Ok(format!("Removed {removed} signal(s)"))
        } else {
            Err(format!("No matching signal in waveform for '{name}'"))
        }
    }

    /// Handles a key event in normal mode.
    pub fn handle_normal_key(&mut self, key: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.state.select_next_signal(),
            KeyCode::Char('k') | KeyCode::Up => self.state.select_prev_signal(),
            KeyCode::Char('h') | KeyCode::Left => self.state.viewport.scroll_left(),
            KeyCode::Char('l') | KeyCode::Right => self.state.viewport.scroll_right(),
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.state.viewport.zoom_in(self.state.cursor_fs);
            }
            KeyCode::Char('-') => {
                self.state.viewport.zoom_out(self.state.cursor_fs);
            }
            KeyCode::Char(' ') => {
                if self.mode == TuiMode::Simulation {
                    let _ = self.step();
                } else {
                    self.state.status_message = "Viewer mode: simulation not available".into();
                }
            }
            KeyCode::Char(':') => {
                self.state.mode = InputMode::Command;
                self.state.command_buffer.clear();
            }
            KeyCode::Char('?') => {
                self.state.show_help = !self.state.show_help;
            }
            KeyCode::Char('e') => {
                self.toggle_expand();
            }
            KeyCode::Char('d') => {
                self.state.value_format = self.state.value_format.cycle();
            }
            KeyCode::Char('f') => {
                let max_t = self.waveform.max_time();
                let kernel_t = self.kernel.as_ref().map_or(0, |k| k.current_time().fs);
                self.state.viewport.fit(max_t.max(kernel_t));
            }
            KeyCode::Enter => {
                self.state.toggle_waveform_signal();
            }
            KeyCode::Tab => {
                self.state.focused = match self.state.focused {
                    FocusedPanel::SignalList => FocusedPanel::Waveform,
                    FocusedPanel::Waveform => FocusedPanel::CommandInput,
                    FocusedPanel::CommandInput => FocusedPanel::SignalList,
                };
            }
            _ => {}
        }
    }

    /// Handles a key event in command mode.
    pub fn handle_command_key(&mut self, key: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match key {
            KeyCode::Esc => {
                self.state.mode = InputMode::Normal;
                self.state.command_buffer.clear();
            }
            KeyCode::Enter => {
                let cmd = self.state.command_buffer.clone();
                self.state.mode = InputMode::Normal;
                self.state.command_buffer.clear();
                if !cmd.is_empty() {
                    match self.execute_command(&cmd) {
                        Ok(msg) => {
                            if !msg.is_empty() {
                                self.state.status_message = msg;
                            }
                        }
                        Err(err) => {
                            self.state.status_message = format!("Error: {err}");
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                self.state.command_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.state.command_buffer.push(c);
            }
            _ => {}
        }
    }
}

/// Returns the help text for the TUI.
fn help_text() -> String {
    "\
Navigation:
  j/k  or ↑/↓   Select signal
  h/l  or ←/→   Scroll waveform
  +/-            Zoom in/out
  f              Fit to time range
  Space          Step simulation
  Enter          Toggle signal in waveform
  e              Expand/collapse bus bits
  Tab            Switch panel focus
  d              Cycle value format (hex/bin/dec)
  ?              Toggle help
  :              Enter command mode
  q              Quit

Commands:
  run <dur>      Run for duration (e.g., 'run 100ns')
  step (s)       Single delta step
  continue (c)   Run to completion
  goto <time>    Jump cursor to time
  zoomin/zo      Zoom in/out
  fit            Fit viewport
  add <signal>   Add signal to waveform
  rm <signal>    Remove signal from waveform
  fmt            Cycle value format
  quit (q)       Exit"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, Interner, LogicVec};
    use aion_ir::arena::Arena;
    use aion_ir::{
        Assignment, Design, Expr, Module, ModuleId, Signal, SignalId, SignalKind, SignalRef,
        SourceMap, Type, TypeDb,
    };
    use aion_source::Span;

    fn make_test_interner() -> Interner {
        let interner = Interner::new();
        interner.get_or_intern("__dummy__"); // 0
        interner.get_or_intern("top"); // 1
        interner.get_or_intern("clk"); // 2
        interner.get_or_intern("out"); // 3
        interner
    }

    fn make_type_db() -> TypeDb {
        let mut types = TypeDb::new();
        types.intern(Type::Bit);
        types
    }

    fn make_simple_design() -> Design {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = Module {
            id: ModuleId::from_raw(0),
            name: Ident::from_raw(1),
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(b"test"),
        };

        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        top.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(1)),
            value: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);

        Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        }
    }

    #[test]
    fn app_construction() {
        let design = make_simple_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        assert!(!app.initialized);
        assert!(!app.should_quit);
        assert_eq!(app.signal_info.len(), 2);
        assert_eq!(app.waveform.signal_count(), 2);
        assert_eq!(app.mode, TuiMode::Simulation);
        assert!(app.kernel.is_some());
    }

    #[test]
    fn app_initialize() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();
        assert!(app.initialized);
        // Double init is ok
        app.initialize().unwrap();
    }

    #[test]
    fn app_step() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();
        let result = app.step();
        // Either continued or done (no more events)
        assert!(result.is_ok());
    }

    #[test]
    fn app_signal_value_str() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();
        let val = app.signal_value_str(0);
        // Should be "0", "1", "x", or "z"
        assert!(!val.is_empty());
    }

    #[test]
    fn app_signal_value_str_out_of_bounds() {
        let design = make_simple_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        assert_eq!(app.signal_value_str(999), "?");
    }

    #[test]
    fn app_time_str() {
        let design = make_simple_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let t = app.time_str();
        assert!(t.contains("0"));
    }

    #[test]
    fn app_execute_command_step() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();
        let result = app.execute_command("step");
        assert!(result.is_ok());
    }

    #[test]
    fn app_execute_command_time() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();
        let result = app.execute_command("time").unwrap();
        assert!(result.contains("Time:"));
    }

    #[test]
    fn app_execute_command_quit() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let _ = app.execute_command("quit");
        assert!(app.should_quit);
    }

    #[test]
    fn app_execute_command_zoom() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let span_before = app.state.viewport.span_fs();
        let _ = app.execute_command("zoomin");
        assert!(app.state.viewport.span_fs() < span_before);
    }

    #[test]
    fn app_handle_normal_key_quit() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.handle_normal_key(crossterm::event::KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn app_handle_normal_key_nav() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.handle_normal_key(crossterm::event::KeyCode::Char('j'));
        assert_eq!(app.state.selected_signal, 1);
        app.handle_normal_key(crossterm::event::KeyCode::Char('k'));
        assert_eq!(app.state.selected_signal, 0);
    }

    #[test]
    fn app_handle_command_mode() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.handle_normal_key(crossterm::event::KeyCode::Char(':'));
        assert_eq!(app.state.mode, InputMode::Command);
        app.handle_command_key(crossterm::event::KeyCode::Char('t'));
        app.handle_command_key(crossterm::event::KeyCode::Char('i'));
        app.handle_command_key(crossterm::event::KeyCode::Char('m'));
        app.handle_command_key(crossterm::event::KeyCode::Char('e'));
        assert_eq!(app.state.command_buffer, "time");
        app.handle_command_key(crossterm::event::KeyCode::Esc);
        assert_eq!(app.state.mode, InputMode::Normal);
        assert!(app.state.command_buffer.is_empty());
    }

    #[test]
    fn app_handle_command_enter() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();
        app.state.mode = InputMode::Command;
        app.state.command_buffer = "time".to_string();
        app.handle_command_key(crossterm::event::KeyCode::Enter);
        assert_eq!(app.state.mode, InputMode::Normal);
        assert!(app.state.status_message.contains("Time:"));
    }

    #[test]
    fn app_handle_command_backspace() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.state.mode = InputMode::Command;
        app.state.command_buffer = "abc".to_string();
        app.handle_command_key(crossterm::event::KeyCode::Backspace);
        assert_eq!(app.state.command_buffer, "ab");
    }

    #[test]
    fn app_handle_tab_focus() {
        let design = make_simple_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        assert_eq!(app.state.focused, FocusedPanel::SignalList);
        app.handle_normal_key(crossterm::event::KeyCode::Tab);
        assert_eq!(app.state.focused, FocusedPanel::Waveform);
        app.handle_normal_key(crossterm::event::KeyCode::Tab);
        assert_eq!(app.state.focused, FocusedPanel::CommandInput);
        app.handle_normal_key(crossterm::event::KeyCode::Tab);
        assert_eq!(app.state.focused, FocusedPanel::SignalList);
    }

    fn make_bus_interner() -> Interner {
        let interner = Interner::new();
        interner.get_or_intern("__dummy__"); // 0
        interner.get_or_intern("top"); // 1
        interner.get_or_intern("clk"); // 2
        interner.get_or_intern("count"); // 3
        interner
    }

    fn make_bus_design() -> Design {
        let mut types = TypeDb::new();
        types.intern(Type::Bit); // 0
        types.intern(Type::BitVec {
            width: 8,
            signed: false,
        }); // 1
        let bit_ty = aion_ir::TypeId::from_raw(0);
        let bus_ty = aion_ir::TypeId::from_raw(1);

        let mut top = Module {
            id: ModuleId::from_raw(0),
            name: Ident::from_raw(1),
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(b"bus_test"),
        };

        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2), // clk
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3), // count
            ty: bus_ty,
            kind: SignalKind::Reg,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);

        Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        }
    }

    #[test]
    fn toggle_expand_bus_signal() {
        let design = make_bus_design();
        let mut app = TuiApp::new(&design, &make_bus_interner()).unwrap();

        // Signal 1 is 8-bit bus, signal 0 is 1-bit
        assert!(app.state.expanded_signals.is_empty());

        // Select bus signal and expand
        app.state.selected_signal = 1;
        app.toggle_expand();
        assert!(app.state.expanded_signals.contains(&1));

        // Toggle again to collapse
        app.toggle_expand();
        assert!(!app.state.expanded_signals.contains(&1));
    }

    #[test]
    fn toggle_expand_1bit_noop() {
        let design = make_bus_design();
        let mut app = TuiApp::new(&design, &make_bus_interner()).unwrap();

        // Select 1-bit signal, expand should do nothing
        app.state.selected_signal = 0;
        app.toggle_expand();
        assert!(app.state.expanded_signals.is_empty());
    }

    #[test]
    fn expand_key_binding() {
        let design = make_bus_design();
        let mut app = TuiApp::new(&design, &make_bus_interner()).unwrap();

        app.state.selected_signal = 1;
        app.handle_normal_key(crossterm::event::KeyCode::Char('e'));
        assert!(app.state.expanded_signals.contains(&1));

        app.handle_normal_key(crossterm::event::KeyCode::Char('e'));
        assert!(!app.state.expanded_signals.contains(&1));
    }

    #[test]
    fn bit_value_str_with_data() {
        let design = make_bus_design();
        let mut app = TuiApp::new(&design, &make_bus_interner()).unwrap();
        app.initialize().unwrap();

        // Record a bus value: 0x05 = 0000_0101
        app.waveform
            .record(1, 0, aion_common::LogicVec::from_u64(0x05, 8));
        app.state.cursor_fs = 0;

        assert_eq!(app.bit_value_str(1, 0), "1");
        assert_eq!(app.bit_value_str(1, 1), "0");
        assert_eq!(app.bit_value_str(1, 2), "1");
        assert_eq!(app.bit_value_str(1, 7), "0");
    }

    #[test]
    fn bit_value_str_no_data() {
        let design = make_bus_design();
        let app = TuiApp::new(&design, &make_bus_interner()).unwrap();
        assert_eq!(app.bit_value_str(1, 0), "?");
    }

    #[test]
    fn bit_value_str_out_of_bounds() {
        let design = make_bus_design();
        let app = TuiApp::new(&design, &make_bus_interner()).unwrap();
        assert_eq!(app.bit_value_str(999, 0), "?");
    }

    // -- Viewer mode tests --

    fn make_viewer_app() -> TuiApp {
        let mut waveform = WaveformData::new();
        let id0 = aion_sim::SimSignalId::from_raw(0);
        let id1 = aion_sim::SimSignalId::from_raw(1);
        waveform.register(id0, "top.clk".into(), 1);
        waveform.register(id1, "top.data".into(), 4);
        waveform.record(0, 0, LogicVec::from_bool(false));
        waveform.record(0, 100, LogicVec::from_bool(true));
        waveform.record(1, 0, LogicVec::from_u64(0, 4));
        waveform.record(1, 100, LogicVec::from_u64(5, 4));

        let signal_info = vec![
            SignalInfo {
                id: id0,
                name: "top.clk".into(),
                width: 1,
            },
            SignalInfo {
                id: id1,
                name: "top.data".into(),
                width: 4,
            },
        ];

        TuiApp::from_waveform(waveform, signal_info)
    }

    #[test]
    fn viewer_mode_construction() {
        let app = make_viewer_app();
        assert_eq!(app.mode, TuiMode::Viewer);
        assert!(app.kernel.is_none());
        assert!(app.initialized);
        assert_eq!(app.signal_info.len(), 2);
    }

    #[test]
    fn viewer_mode_no_step() {
        let mut app = make_viewer_app();
        let result = app.step();
        assert!(result.is_err());
    }

    #[test]
    fn viewer_mode_signal_value_at_cursor() {
        let mut app = make_viewer_app();
        app.state.cursor_fs = 0;
        // clk = 0 at time 0
        let val = app.signal_value_str(0);
        assert_eq!(val, "0");

        app.state.cursor_fs = 100;
        // clk = 1 at time 100
        let val = app.signal_value_str(0);
        assert_eq!(val, "1");
    }

    #[test]
    fn viewer_mode_key_handling_space() {
        let mut app = make_viewer_app();
        app.handle_normal_key(crossterm::event::KeyCode::Char(' '));
        // Should show viewer mode message, not crash
        assert!(app.state.status_message.contains("Viewer mode"));
    }

    #[test]
    fn viewer_mode_zoom_fit() {
        let mut app = make_viewer_app();
        app.handle_normal_key(crossterm::event::KeyCode::Char('f'));
        // Should not panic (no kernel to call current_time on)
    }

    #[test]
    fn viewer_mode_sim_command_rejected() {
        let mut app = make_viewer_app();
        let result = app.execute_command("run 100ns");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not available"));
    }

    #[test]
    fn viewer_mode_quit_works() {
        let mut app = make_viewer_app();
        let _ = app.execute_command("quit");
        assert!(app.should_quit);
    }

    #[test]
    fn viewer_mode_time_str() {
        let mut app = make_viewer_app();
        app.state.cursor_fs = 100;
        let t = app.time_str();
        // Should use cursor time in viewer mode
        assert!(t.contains("100"));
    }
}
