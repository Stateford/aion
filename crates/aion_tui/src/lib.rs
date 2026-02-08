//! Terminal-based waveform viewer and interactive simulator.
//!
//! Provides a ratatui-based TUI for the Aion HDL simulator, replacing the
//! line-based REPL with a graphical interface featuring waveform traces,
//! a signal list, time cursor navigation, and a command bar.
//!
//! # Usage
//!
//! ```ignore
//! use aion_tui::run_tui;
//!
//! let design = aion_elaborate::elaborate(&parsed, &config, &source_db, &interner, &sink)?;
//! run_tui(&design)?;
//! ```
//!
//! # Layout
//!
//! The TUI is divided into four panels:
//!
//! - **Signal List** (left) — lists all signals with current values
//! - **Waveform** (right) — graphical waveform traces with time ruler
//! - **Status Bar** — simulation time, mode, signal count
//! - **Command Input** — key hints or command prompt

#![warn(missing_docs)]

pub mod app;
pub mod commands;
pub mod event;
pub mod render;
pub mod state;
pub mod terminal;
pub mod waveform_data;
pub mod widgets;

use std::time::Duration;

use aion_common::Interner;
use aion_ir::Design;
use aion_sim::SimError;

use app::{SignalInfo, TuiApp, TuiMode};
use event::{poll_event, TuiEvent};
use state::InputMode;
use terminal::{init_terminal, install_panic_hook, restore_terminal};
use waveform_data::WaveformData;

/// Runs the TUI interactive simulator on an elaborated design.
///
/// Sets up the terminal, creates a `TuiApp` wrapping a `SimKernel`, and
/// runs the main event loop. Restores the terminal on exit (including
/// on panic). The `interner` is used to resolve interned signal names.
///
/// # Errors
///
/// Returns `SimError` if simulation kernel creation or stepping fails.
pub fn run_tui(design: &Design, interner: &Interner) -> Result<(), SimError> {
    install_panic_hook();

    let mut terminal = init_terminal().map_err(|e| {
        SimError::WaveformIo(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))
    })?;

    let mut app = TuiApp::new(design, interner)?;
    app.initialize()?;

    run_tui_loop(&mut app, &mut terminal)?;

    restore_terminal().map_err(|e| {
        SimError::WaveformIo(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))
    })?;

    Ok(())
}

/// Runs the TUI waveform viewer on pre-loaded waveform data.
///
/// Sets up the terminal, creates a `TuiApp` in viewer mode (no kernel),
/// and runs the main event loop. Simulation commands are unavailable;
/// only waveform navigation and inspection are supported.
///
/// # Errors
///
/// Returns `SimError` on I/O errors during terminal setup or rendering.
pub fn run_tui_viewer(
    waveform: WaveformData,
    signal_info: Vec<SignalInfo>,
) -> Result<(), SimError> {
    install_panic_hook();

    let mut terminal = init_terminal().map_err(|e| {
        SimError::WaveformIo(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))
    })?;

    let mut app = TuiApp::from_waveform(waveform, signal_info);

    run_tui_loop(&mut app, &mut terminal)?;

    restore_terminal().map_err(|e| {
        SimError::WaveformIo(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))
    })?;

    Ok(())
}

/// Shared event loop for both simulation and viewer modes.
fn run_tui_loop(
    app: &mut TuiApp,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
) -> Result<(), SimError> {
    let tick_rate = Duration::from_millis(50);

    loop {
        // Render
        terminal
            .draw(|frame| render::render(app, frame))
            .map_err(|e| {
                SimError::WaveformIo(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                ))
            })?;

        // Handle events
        match poll_event(tick_rate) {
            Ok(TuiEvent::Key(key)) => match app.state.mode {
                InputMode::Normal => app.handle_normal_key(key.code),
                InputMode::Command => app.handle_command_key(key.code),
            },
            Ok(TuiEvent::Tick) => {
                // Auto-run: step simulation if running (simulation mode only)
                if app.mode == TuiMode::Simulation
                    && app.state.auto_running
                    && !app.is_finished()
                    && app.has_pending_events()
                {
                    let _ = app.step();
                }
            }
            Ok(TuiEvent::Resize(_, _)) => {
                // Handled automatically by ratatui
            }
            Ok(TuiEvent::Mouse(_)) => {
                // Mouse support placeholder
            }
            Err(_) => {
                // I/O error — quit gracefully
                break;
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, Interner, LogicVec};
    use aion_ir::arena::Arena;
    use aion_ir::{
        Design, Module, ModuleId, Signal, SignalId, SignalKind, SourceMap, Type, TypeDb,
    };
    use aion_source::Span;
    use crossterm::event::KeyCode;

    fn make_test_interner() -> Interner {
        let interner = Interner::new();
        interner.get_or_intern("__dummy__"); // 0
        interner.get_or_intern("top"); // 1
        interner.get_or_intern("clk"); // 2
        interner
    }

    fn make_test_design() -> Design {
        let mut types = TypeDb::new();
        types.intern(Type::Bit);
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
    fn app_can_be_constructed() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        assert_eq!(app.signal_info.len(), 1);
    }

    #[test]
    fn app_initialize_and_step() {
        let design = make_test_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();
        let _ = app.step();
    }

    #[test]
    fn app_key_handling_does_not_panic() {
        let design = make_test_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();

        // Test various key codes
        app.handle_normal_key(KeyCode::Char('j'));
        app.handle_normal_key(KeyCode::Char('k'));
        app.handle_normal_key(KeyCode::Char('+'));
        app.handle_normal_key(KeyCode::Char('-'));
        app.handle_normal_key(KeyCode::Char(' '));
        app.handle_normal_key(KeyCode::Char('f'));
        app.handle_normal_key(KeyCode::Char('d'));
        app.handle_normal_key(KeyCode::Char('?'));
        app.handle_normal_key(KeyCode::Tab);
        app.handle_normal_key(KeyCode::Enter);
    }

    #[test]
    fn viewer_app_can_be_constructed() {
        let mut waveform = WaveformData::new();
        let id = aion_sim::SimSignalId::from_raw(0);
        waveform.register(id, "top.clk".into(), 1);
        waveform.record(0, 0, LogicVec::from_bool(false));

        let signal_info = vec![SignalInfo {
            id,
            name: "top.clk".into(),
            width: 1,
        }];

        let app = TuiApp::from_waveform(waveform, signal_info);
        assert_eq!(app.mode, TuiMode::Viewer);
        assert!(app.initialized);
        assert!(app.kernel.is_none());
    }

    #[test]
    fn viewer_key_handling_does_not_panic() {
        let mut waveform = WaveformData::new();
        let id = aion_sim::SimSignalId::from_raw(0);
        waveform.register(id, "top.clk".into(), 1);
        waveform.record(0, 0, LogicVec::from_bool(false));

        let signal_info = vec![SignalInfo {
            id,
            name: "top.clk".into(),
            width: 1,
        }];

        let mut app = TuiApp::from_waveform(waveform, signal_info);
        app.handle_normal_key(KeyCode::Char('j'));
        app.handle_normal_key(KeyCode::Char('k'));
        app.handle_normal_key(KeyCode::Char(' ')); // Should not crash in viewer mode
        app.handle_normal_key(KeyCode::Char('f'));
        app.handle_normal_key(KeyCode::Char('?'));
    }
}
