//! Status bar widget.
//!
//! Renders a single-line status bar at the bottom showing the current
//! simulation time, input mode, signal count, and status message.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::TuiApp;
use crate::state::InputMode;

/// Renders the status bar into the given area.
pub fn render_status_bar(app: &TuiApp, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }

    let mode_str = match app.state.mode {
        InputMode::Normal => "NORMAL",
        InputMode::Command => "COMMAND",
    };

    let mode_style = match app.state.mode {
        InputMode::Normal => Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        InputMode::Command => Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    };

    let time_str = app.time_str();
    let signal_count = app.signal_info.len();
    let finished = if app.kernel.is_finished() {
        " [DONE]"
    } else {
        ""
    };
    let auto = if app.state.auto_running { " [RUN]" } else { "" };

    let status_msg = if app.state.status_message.is_empty() {
        String::new()
    } else {
        format!(" | {}", app.state.status_message)
    };

    let line = Line::from(vec![
        Span::styled(format!(" {mode_str} "), mode_style),
        Span::styled(format!(" T={time_str}"), Style::default().fg(Color::White)),
        Span::styled(
            format!(" | {signal_count} signals"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{finished}{auto}"),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(status_msg, Style::default().fg(Color::Cyan)),
    ]);

    // Fill the entire line with background color
    let bg_style = Style::default().bg(Color::DarkGray);
    for x in area.x..area.x + area.width {
        if x < buf.area().right() {
            buf.get_mut(x, area.y).set_style(bg_style);
        }
    }

    Widget::render(line, area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, Interner};
    use aion_ir::arena::Arena;
    use aion_ir::{
        Design, Module, ModuleId, Signal, SignalId, SignalKind, SourceMap, Type, TypeDb,
    };
    use aion_source::Span as IrSpan;

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
            span: IrSpan::DUMMY,
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
            span: IrSpan::DUMMY,
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
    fn render_status_bar_normal_mode() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_status_bar(&app, area, &mut buf);
        // Check that NORMAL appears somewhere in the buffer
        let content: String = (0..80)
            .map(|x| buf.get(x, 0u16).symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("NORMAL"));
    }

    #[test]
    fn render_status_bar_command_mode() {
        let design = make_test_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.state.mode = InputMode::Command;
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_status_bar(&app, area, &mut buf);
        let content: String = (0..80)
            .map(|x| buf.get(x, 0u16).symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("COMMAND"));
    }

    #[test]
    fn render_status_bar_zero_height() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let area = Rect::new(0, 0, 80, 0);
        let mut buf = Buffer::empty(area);
        render_status_bar(&app, area, &mut buf);
        // Should not panic
    }

    #[test]
    fn render_status_bar_with_message() {
        let design = make_test_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.state.status_message = "Test message".to_string();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_status_bar(&app, area, &mut buf);
    }
}
