//! Top-level rendering logic.
//!
//! Assembles the TUI layout by splitting the terminal into panels and
//! delegating rendering to individual widget modules.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Frame;

use crate::app::TuiApp;
use crate::widgets::{command_input, signal_list, status_bar, waveform};

/// Renders the complete TUI layout into the given frame.
///
/// Layout:
/// ```text
/// ┌─────────────┬──────────────────────┐
/// │ Signal List  │    Waveform          │
/// │ (30%)        │    (70%)             │
/// │              │                      │
/// ├──────────────┴──────────────────────┤
/// │ Status Bar                          │
/// ├─────────────────────────────────────┤
/// │ Command Input                       │
/// └─────────────────────────────────────┘
/// ```
pub fn render(app: &TuiApp, frame: &mut Frame) {
    let size = frame.size();

    // Main vertical split: content area + status bar + command input
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // main content
            Constraint::Length(1), // status bar
            Constraint::Length(1), // command input
        ])
        .split(size);

    // Horizontal split: signal list + waveform
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30), // signal list
            Constraint::Percentage(70), // waveform
        ])
        .split(vertical[0]);

    // Render each panel
    signal_list::render_signal_list(app, horizontal[0], frame.buffer_mut());
    waveform::render_waveform(app, horizontal[1], frame.buffer_mut());
    status_bar::render_status_bar(app, vertical[1], frame.buffer_mut());
    command_input::render_command_input(app, vertical[2], frame.buffer_mut());

    // Help popup (if visible)
    if app.state.show_help {
        render_help_popup(frame);
    }
}

/// Renders a centered help popup.
fn render_help_popup(frame: &mut Frame) {
    use ratatui::style::{Color, Style};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

    let help_text = "\
Navigation:
  j/k ↑/↓  Select signal       h/l ←/→  Scroll waveform
  +/-       Zoom in/out         f         Fit viewport
  Space     Step simulation     Enter     Toggle signal
  Tab       Switch panel        d         Cycle format
  ?         Toggle help         :         Command mode
  q         Quit

Commands:
  run <dur>    Run for duration    step (s)    Single step
  continue     Run to end          goto <t>    Jump cursor
  add <sig>    Add to waveform     rm <sig>    Remove
  zoomin/zo    Zoom in/out         fit         Fit viewport
  fmt          Cycle format        quit (q)    Exit

Press ? to close";

    let area = frame.size();
    let popup_width = 60u16.min(area.width.saturating_sub(4));
    let popup_height = 18u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = ratatui::layout::Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let popup = Paragraph::new(help_text)
        .block(
            Block::default()
                .title(" Help ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false });

    frame.render_widget(popup, popup_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, Interner};
    use aion_ir::arena::Arena;
    use aion_ir::{
        Design, Module, ModuleId, Signal, SignalId, SignalKind, SourceMap, Type, TypeDb,
    };
    use aion_source::Span;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn make_test_interner() -> Interner {
        let interner = Interner::new();
        interner.get_or_intern("__dummy__");
        interner.get_or_intern("top");
        interner.get_or_intern("clk");
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
    fn render_full_layout() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(&app, f)).unwrap();
    }

    #[test]
    fn render_with_help_popup() {
        let design = make_test_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.state.show_help = true;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(&app, f)).unwrap();
    }

    #[test]
    fn render_small_terminal() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(&app, f)).unwrap();
    }

    #[test]
    fn render_command_mode_layout() {
        let design = make_test_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.state.mode = crate::state::InputMode::Command;
        app.state.command_buffer = "run 100ns".to_string();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(&app, f)).unwrap();
    }
}
