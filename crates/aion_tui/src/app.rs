//! TUI application core.
//!
//! [`TuiApp`] owns the simulation kernel, waveform data, and TUI state.
//! It provides the main event loop and coordinates simulation stepping
//! with UI rendering.

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

/// The core TUI application state.
///
/// Owns the simulation kernel, waveform history, and UI state. Provides
/// methods for stepping the simulation, handling key events, and executing
/// commands.
pub struct TuiApp {
    /// The simulation kernel.
    pub kernel: SimKernel,
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
            kernel,
            waveform,
            state,
            signal_info,
            initialized: false,
            should_quit: false,
        })
    }

    /// Initializes the simulation kernel.
    ///
    /// Must be called once before stepping. Records the initial signal values.
    pub fn initialize(&mut self) -> Result<(), SimError> {
        if self.initialized {
            return Ok(());
        }
        self.kernel.initialize()?;
        self.initialized = true;
        self.snapshot_signals();

        // Collect initial display output
        let output = self.kernel.take_display_output();
        self.state.display_output.extend(output);

        Ok(())
    }

    /// Advances the simulation to the next meaningful time point and records signal values.
    ///
    /// Uses `run_until()` to process both events and suspended process wakeups,
    /// ensuring delay-based scheduling is honored.
    pub fn step(&mut self) -> Result<StepResult, SimError> {
        let result = if let Some(next_t) = self.kernel.next_event_time_fs() {
            self.kernel.run_until(next_t)?
        } else {
            StepResult::Done
        };
        self.snapshot_signals();

        // Collect display output
        let output = self.kernel.take_display_output();
        self.state.display_output.extend(output);

        Ok(result)
    }

    /// Runs the simulation for the given duration in femtoseconds.
    ///
    /// Steps through each event time up to the target, snapshotting signal
    /// values at every event so that the waveform captures all intermediate
    /// transitions (clock toggles, counter increments, etc.).
    pub fn run_for(&mut self, duration_fs: u64) -> Result<(), SimError> {
        let target_fs = self.kernel.current_time().fs + duration_fs;

        while !self.kernel.is_finished() {
            let next_t = match self.kernel.next_event_time_fs() {
                Some(t) if t <= target_fs => t,
                _ => break,
            };
            self.kernel.run_until(next_t)?;
            self.snapshot_signals();
        }

        // Advance time to target even if no more events
        if self.kernel.current_time().fs < target_fs && !self.kernel.is_finished() {
            self.kernel.run_until(target_fs)?;
            self.snapshot_signals();
        }

        let output = self.kernel.take_display_output();
        self.state.display_output.extend(output);

        Ok(())
    }

    /// Returns the formatted value of a signal at the current cursor time.
    ///
    /// Looks up the value from the waveform history at `cursor_fs` so the
    /// signal list panel shows the value at the cursor, not the final sim
    /// time. Falls back to the kernel's current value when no waveform data
    /// exists yet.
    pub fn signal_value_str(&self, signal_idx: usize) -> String {
        // Try waveform history at cursor time first
        if let Some(history) = self.waveform.signals.get(signal_idx) {
            if let Some(val) = history.value_at(self.state.cursor_fs) {
                return format_value(val);
            }
        }
        // Fall back to current kernel value
        if let Some(info) = self.signal_info.get(signal_idx) {
            let val = self.kernel.signal_value(info.id);
            format_value(val)
        } else {
            "?".into()
        }
    }

    /// Returns the current simulation time as a formatted string.
    pub fn time_str(&self) -> String {
        format!("{}", self.kernel.current_time())
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
                self.state
                    .viewport
                    .fit(max_t.max(self.kernel.current_time().fs));
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
                let mut output = String::new();
                for name in signals {
                    match self.kernel.find_signal(name) {
                        Some(id) => {
                            let val = self.kernel.signal_value(id);
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
            SimCommand::Status => Ok(format!(
                "Time: {}\nSignals: {}\nFinished: {}",
                self.time_str(),
                self.signal_info.len(),
                self.kernel.is_finished()
            )),
            SimCommand::Continue => {
                while !self.kernel.is_finished() && self.kernel.has_pending_events() {
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

    /// Snapshots current signal values into waveform data.
    fn snapshot_signals(&mut self) {
        let time_fs = self.kernel.current_time().fs;
        for (i, info) in self.signal_info.iter().enumerate() {
            let val = self.kernel.signal_value(info.id).clone();
            self.waveform.record(i, time_fs, val);
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
                let _ = self.step();
            }
            KeyCode::Char(':') => {
                self.state.mode = InputMode::Command;
                self.state.command_buffer.clear();
            }
            KeyCode::Char('?') => {
                self.state.show_help = !self.state.show_help;
            }
            KeyCode::Char('d') => {
                self.state.value_format = self.state.value_format.cycle();
            }
            KeyCode::Char('f') => {
                let max_t = self.waveform.max_time();
                self.state
                    .viewport
                    .fit(max_t.max(self.kernel.current_time().fs));
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
    use aion_common::{ContentHash, Ident, Interner};
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
}
