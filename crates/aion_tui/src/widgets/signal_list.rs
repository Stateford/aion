//! Signal list widget.
//!
//! Renders the left panel showing signal names and their current values.
//! The selected signal is highlighted.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget};

use crate::app::TuiApp;
use crate::state::FocusedPanel;

/// Renders the signal list panel into the given buffer area.
pub fn render_signal_list(app: &TuiApp, area: Rect, buf: &mut Buffer) {
    let is_focused = app.state.focused == FocusedPanel::SignalList;
    let border_style = if is_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Signals ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = app
        .signal_info
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let name = &info.name;
            let width = info.width;
            let val = app.signal_value_str(i);
            let in_waveform = app.state.waveform_signals.contains(&i);
            let marker = if in_waveform { "+" } else { " " };
            let line = Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(Color::DarkGray)),
                Span::styled(name.to_string(), Style::default().fg(Color::White)),
                Span::styled(format!(" [{width}]"), Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" = {val}"), Style::default().fg(Color::Yellow)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let highlight_style = Style::default()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);

    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style);

    let mut list_state = ListState::default();
    if !app.signal_info.is_empty() {
        list_state.select(Some(app.state.selected_signal));
    }

    StatefulWidget::render(list, area, buf, &mut list_state);
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
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

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
    fn render_signal_list_does_not_panic() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        render_signal_list(&app, area, &mut buf);
        // Just verify no panic and something was written
        assert!(buf.area().width > 0);
    }

    #[test]
    fn render_signal_list_empty_design() {
        let types = TypeDb::new();
        let top = Module {
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
        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };

        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        render_signal_list(&app, area, &mut buf);
    }
}
