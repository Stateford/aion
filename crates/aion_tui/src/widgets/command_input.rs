//! Command input widget.
//!
//! Renders the command bar at the bottom of the TUI. In normal mode it
//! shows key hints; in command mode it shows the `:` prompt with the
//! typed command.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::TuiApp;
use crate::state::InputMode;

/// Renders the command input bar.
pub fn render_command_input(app: &TuiApp, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }

    let line = match app.state.mode {
        InputMode::Normal => Line::from(vec![
            Span::styled(
                " q",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":quit ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Space",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":step ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "j/k",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":nav ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "+/-",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":zoom ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                ":",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("cmd ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "?",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":help", Style::default().fg(Color::DarkGray)),
        ]),
        InputMode::Command => Line::from(vec![
            Span::styled(
                ":",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&app.state.command_buffer, Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::White)),
        ]),
    };

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
    fn render_command_input_normal() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_command_input(&app, area, &mut buf);
        let content: String = (0..80)
            .map(|x| buf.get(x, 0u16).symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("quit"));
    }

    #[test]
    fn render_command_input_command_mode() {
        let design = make_test_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.state.mode = InputMode::Command;
        app.state.command_buffer = "run 10ns".to_string();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_command_input(&app, area, &mut buf);
        let content: String = (0..80)
            .map(|x| buf.get(x, 0u16).symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("run 10ns"));
    }

    #[test]
    fn render_command_input_zero_height() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let area = Rect::new(0, 0, 80, 0);
        let mut buf = Buffer::empty(area);
        render_command_input(&app, area, &mut buf);
    }
}
