//! Waveform viewer widget.
//!
//! Renders signal waveforms as graphical traces using box-drawing characters.
//! Single-bit signals use 2-row traces with transitions, while multi-bit buses
//! show hex labels between transitions.

use aion_common::Logic;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Widget};

use crate::app::TuiApp;
use crate::state::FocusedPanel;

/// Renders the waveform panel showing signal traces and time ruler.
pub fn render_waveform(app: &TuiApp, area: Rect, buf: &mut Buffer) {
    let is_focused = app.state.focused == FocusedPanel::Waveform;
    let border_style = if is_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Waveform ")
        .borders(Borders::ALL)
        .border_style(border_style);

    Widget::render(block, area, buf);

    // Inner area after borders
    if area.width < 4 || area.height < 3 {
        return;
    }
    let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);

    // Row 0: time ruler
    render_time_ruler(app, inner, buf);

    // Remaining rows: signal waveforms (1-bit signals use 2 rows)
    let mut row = inner.y + 1;
    for &sig_idx in &app.state.waveform_signals {
        if row >= inner.y + inner.height {
            break;
        }

        let width = app.signal_info.get(sig_idx).map_or(1, |info| info.width);

        if width == 1 {
            let rows_available = (inner.y + inner.height).saturating_sub(row);
            if rows_available >= 2 {
                render_1bit_signal(app, sig_idx, inner.x, row, inner.width, buf);
                row += 2;
            } else {
                // Not enough room for 2-row waveform, skip
                break;
            }
        } else {
            render_bus_signal(app, sig_idx, inner.x, row, inner.width, buf);
            row += 1;
        }
    }

    // Cursor line
    if let Some(col) = app
        .state
        .viewport
        .time_to_col(app.state.cursor_fs, inner.width)
    {
        let cursor_x = inner.x + col;
        for y in inner.y..inner.y + inner.height {
            if cursor_x < buf.area().right() && y < buf.area().bottom() {
                buf.get_mut(cursor_x, y)
                    .set_style(Style::default().fg(Color::Magenta));
            }
        }
    }
}

/// Renders the time ruler at the top of the waveform area.
fn render_time_ruler(app: &TuiApp, inner: Rect, buf: &mut Buffer) {
    let width = inner.width as usize;
    if width == 0 {
        return;
    }

    let style = Style::default().fg(Color::DarkGray);
    let tick_style = Style::default().fg(Color::DarkGray);

    // Compute max label width to determine spacing
    let end_label =
        format_time_compact(app.state.viewport.col_to_time(inner.width - 1, inner.width));
    let max_label_width = end_label.len() + 2; // label + 2 chars gap

    // Number of ticks that fit without overlapping
    let num_ticks = if max_label_width == 0 {
        2
    } else {
        (width / max_label_width).clamp(2, 12)
    };

    let mut last_label_end: usize = 0;
    for i in 0..=num_ticks {
        let col = (i * (width - 1)) / num_ticks;
        let time_fs = app.state.viewport.col_to_time(col as u16, inner.width);
        let label = format_time_compact(time_fs);

        // Skip this label if it would overlap with the previous one
        if col < last_label_end && i > 0 {
            // Still draw a tick mark
            let x = inner.x + col as u16;
            if x < buf.area().right() {
                buf.get_mut(x, inner.y).set_char('|').set_style(tick_style);
            }
            continue;
        }

        let x = inner.x + col as u16;
        if x < buf.area().right() {
            for (j, ch) in label.chars().enumerate() {
                let px = x + j as u16;
                if px < inner.x + inner.width {
                    buf.get_mut(px, inner.y).set_char(ch).set_style(style);
                }
            }
            last_label_end = col + label.len() + 1;
        }
    }
}

/// Renders a 1-bit signal waveform using 2-row box-drawing traces.
///
/// The top row shows the high-level line, the bottom row shows the low-level
/// line. Transitions are drawn with corner characters connecting the two rows.
/// ```text
///   ───┐   ┌───       (top row: high level)
///      └───┘           (bottom row: low level)
/// ```
fn render_1bit_signal(
    app: &TuiApp,
    sig_idx: usize,
    x_start: u16,
    row: u16,
    width: u16,
    buf: &mut Buffer,
) {
    let history = match app.waveform.signals.get(sig_idx) {
        Some(h) => h,
        None => return,
    };

    let top_row = row;
    let bot_row = row + 1;

    let style_signal = Style::default().fg(Color::Green);
    let style_xz = Style::default().fg(Color::Red);
    let style_dim = Style::default().fg(Color::DarkGray);

    for col in 0..width {
        let time_fs = app.state.viewport.col_to_time(col, width);
        let x = x_start + col;
        if x >= buf.area().right() {
            break;
        }

        let val = history.value_at(time_fs);

        // Detect transition from previous column
        let prev_val = if col > 0 {
            let prev_time = app.state.viewport.col_to_time(col - 1, width);
            history.value_at(prev_time)
        } else {
            None
        };

        let is_transition = match (&prev_val, &val) {
            (Some(pv), Some(cv)) => pv != cv,
            _ => false,
        };

        let prev_bit = prev_val.as_ref().map(|v| v.get(0));
        let cur_bit = val.as_ref().map(|v| v.get(0));

        if top_row < buf.area().bottom() && bot_row < buf.area().bottom() {
            match cur_bit {
                Some(Logic::One) if is_transition => {
                    // Rising edge: └ on bottom, ┌ on top (but we draw at transition col)
                    // Previous was low → this is high
                    buf.get_mut(x, top_row)
                        .set_char('\u{250C}') // ┌
                        .set_style(style_signal);
                    buf.get_mut(x, bot_row)
                        .set_char('\u{2518}') // ┘
                        .set_style(style_signal);
                }
                Some(Logic::Zero) if is_transition => {
                    // Falling edge: ┐ on top, └ on bottom
                    buf.get_mut(x, top_row)
                        .set_char('\u{2510}') // ┐
                        .set_style(style_signal);
                    buf.get_mut(x, bot_row)
                        .set_char('\u{2514}') // └
                        .set_style(style_signal);
                }
                Some(Logic::One) => {
                    buf.get_mut(x, top_row)
                        .set_char('\u{2500}') // ─
                        .set_style(style_signal);
                    buf.get_mut(x, bot_row).set_char(' ').set_style(style_dim);
                }
                Some(Logic::Zero) => {
                    buf.get_mut(x, top_row).set_char(' ').set_style(style_dim);
                    buf.get_mut(x, bot_row)
                        .set_char('\u{2500}') // ─
                        .set_style(style_signal);
                }
                Some(Logic::X) => {
                    buf.get_mut(x, top_row).set_char('X').set_style(style_xz);
                    buf.get_mut(x, bot_row).set_char('X').set_style(style_xz);
                }
                Some(Logic::Z) => {
                    buf.get_mut(x, top_row).set_char('Z').set_style(style_xz);
                    buf.get_mut(x, bot_row).set_char('Z').set_style(style_xz);
                }
                None => {
                    // Handle X→value or Z→value transitions
                    if is_transition && matches!(prev_bit, Some(Logic::X) | Some(Logic::Z)) {
                        buf.get_mut(x, top_row)
                            .set_char('\u{2502}') // │
                            .set_style(style_signal);
                        buf.get_mut(x, bot_row)
                            .set_char('\u{2502}') // │
                            .set_style(style_signal);
                    } else {
                        buf.get_mut(x, top_row)
                            .set_char('\u{00B7}') // ·
                            .set_style(style_dim);
                        buf.get_mut(x, bot_row)
                            .set_char('\u{00B7}') // ·
                            .set_style(style_dim);
                    }
                }
            }
        }
    }
}

/// Renders a multi-bit bus signal with hex labels between transitions.
fn render_bus_signal(
    app: &TuiApp,
    sig_idx: usize,
    x_start: u16,
    row: u16,
    width: u16,
    buf: &mut Buffer,
) {
    let history = match app.waveform.signals.get(sig_idx) {
        Some(h) => h,
        None => return,
    };

    let style_bus = Style::default().fg(Color::Cyan);
    let style_transition = Style::default().fg(Color::Yellow);

    // Find transitions visible in viewport
    let changes = history.changes_in_range(app.state.viewport.start_fs, app.state.viewport.end_fs);

    // Draw bus bars
    for col in 0..width {
        let x = x_start + col;
        if x >= buf.area().right() || row >= buf.area().bottom() {
            break;
        }
        buf.get_mut(x, row)
            .set_char('\u{2550}') // ═
            .set_style(style_bus);
    }

    // Draw transition markers and labels
    let mut last_label_end: u16 = 0;
    for change in changes {
        if let Some(col) = app.state.viewport.time_to_col(change.time_fs, width) {
            let x = x_start + col;
            if x < buf.area().right() && row < buf.area().bottom() {
                buf.get_mut(x, row)
                    .set_char('\u{256B}') // ╫
                    .set_style(style_transition);
            }

            // Label the value after transition
            let label = format_bus_value(&change.value);
            let label_start = col + 1;
            if label_start > last_label_end {
                for (j, ch) in label.chars().enumerate() {
                    let px = x_start + label_start + j as u16;
                    if px < x_start + width && px < buf.area().right() {
                        buf.get_mut(px, row).set_char(ch).set_style(style_bus);
                    }
                }
                last_label_end = label_start + label.len() as u16;
            }
        }
    }

    // Label initial value if no transition at start (offset by 1 for readability)
    if changes.is_empty() || changes[0].time_fs > app.state.viewport.start_fs {
        if let Some(val) = history.value_at(app.state.viewport.start_fs) {
            let label = format_bus_value(val);
            let label_offset: u16 = 1;
            for (j, ch) in label.chars().enumerate() {
                let px = x_start + label_offset + j as u16;
                if px < x_start + width && px < buf.area().right() && row < buf.area().bottom() {
                    buf.get_mut(px, row).set_char(ch).set_style(style_bus);
                }
            }
        }
    }
}

/// Formats a bus value compactly for waveform labels.
///
/// Uses short hex format (e.g. `0a`, `ff`) instead of the full
/// Verilog-style `8'hff` to save horizontal space in the waveform.
/// Values containing X or Z bits use binary notation.
fn format_bus_value(val: &aion_common::LogicVec) -> String {
    use aion_common::Logic;

    // Check for X/Z bits
    let has_xz = (0..val.width()).any(|i| matches!(val.get(i), Logic::X | Logic::Z));
    if has_xz {
        // Show binary for X/Z values
        let mut s = String::with_capacity(val.width() as usize);
        for i in (0..val.width()).rev() {
            s.push(match val.get(i) {
                Logic::Zero => '0',
                Logic::One => '1',
                Logic::X => 'x',
                Logic::Z => 'z',
            });
        }
        return s;
    }

    match val.to_u64() {
        Some(v) => format!("{v:x}"),
        None => "?".into(),
    }
}

/// Formats a time in femtoseconds compactly for the ruler.
fn format_time_compact(fs: u64) -> String {
    if fs == 0 {
        return "0".into();
    }
    if fs >= 1_000_000_000_000 {
        format!("{}ms", fs / 1_000_000_000_000)
    } else if fs >= 1_000_000_000 {
        format!("{}us", fs / 1_000_000_000)
    } else if fs >= 1_000_000 {
        format!("{}ns", fs / 1_000_000)
    } else if fs >= 1_000 {
        format!("{}ps", fs / 1_000)
    } else {
        format!("{fs}fs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, Interner, LogicVec};
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
    fn render_waveform_does_not_panic() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render_waveform(&app, area, &mut buf);
    }

    #[test]
    fn render_waveform_small_area() {
        let design = make_test_design();
        let app = TuiApp::new(&design, &make_test_interner()).unwrap();
        let area = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::empty(area);
        render_waveform(&app, area, &mut buf);
    }

    #[test]
    fn format_time_compact_units() {
        assert_eq!(format_time_compact(0), "0");
        assert_eq!(format_time_compact(100), "100fs");
        assert_eq!(format_time_compact(5_000), "5ps");
        assert_eq!(format_time_compact(100_000_000), "100ns");
        assert_eq!(format_time_compact(5_000_000_000), "5us");
        assert_eq!(format_time_compact(2_000_000_000_000), "2ms");
    }

    #[test]
    fn format_bus_value_hex() {
        let v = LogicVec::from_u64(0xFF, 8);
        assert_eq!(format_bus_value(&v), "ff");
    }

    #[test]
    fn format_bus_value_single_bit() {
        assert_eq!(format_bus_value(&LogicVec::from_bool(true)), "1");
    }

    #[test]
    fn format_bus_value_xz() {
        use aion_common::Logic;
        let mut v = LogicVec::new(4);
        v.set(0, Logic::One);
        v.set(1, Logic::X);
        v.set(2, Logic::Zero);
        v.set(3, Logic::Z);
        assert_eq!(format_bus_value(&v), "z0x1");
    }

    #[test]
    fn render_1bit_signal_with_data() {
        let design = make_test_design();
        let mut app = TuiApp::new(&design, &make_test_interner()).unwrap();
        app.initialize().unwrap();
        app.waveform.record(0, 0, LogicVec::from_bool(false));
        app.waveform
            .record(0, 50_000_000, LogicVec::from_bool(true));
        app.state.viewport.fit(100_000_000);

        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render_waveform(&app, area, &mut buf);
    }
}
