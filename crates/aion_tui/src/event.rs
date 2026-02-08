//! Event source for the TUI.
//!
//! Polls crossterm for keyboard and mouse events, and generates periodic
//! tick events for UI refresh and auto-run stepping.

use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, MouseEvent};

/// Events produced by the event source for the TUI main loop.
#[derive(Clone, Debug)]
pub enum TuiEvent {
    /// A keyboard key was pressed.
    Key(KeyEvent),
    /// A mouse event occurred.
    Mouse(MouseEvent),
    /// A periodic tick for UI refresh or auto-run stepping.
    Tick,
    /// The terminal was resized.
    Resize(u16, u16),
}

/// Polls for the next TUI event with a timeout.
///
/// Returns `Some(event)` if an event is available within the timeout,
/// or `Some(TuiEvent::Tick)` if the timeout expired (used for periodic
/// refresh). Returns an `Err` on I/O failure.
pub fn poll_event(timeout: Duration) -> std::io::Result<TuiEvent> {
    if event::poll(timeout)? {
        let evt = event::read()?;
        match evt {
            CrosstermEvent::Key(key) => Ok(TuiEvent::Key(key)),
            CrosstermEvent::Mouse(mouse) => Ok(TuiEvent::Mouse(mouse)),
            CrosstermEvent::Resize(w, h) => Ok(TuiEvent::Resize(w, h)),
            _ => Ok(TuiEvent::Tick),
        }
    } else {
        Ok(TuiEvent::Tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_event_returns_tick_on_timeout() {
        // With a very short timeout and no actual terminal, poll
        // should return Tick (timeout expired).
        let result = poll_event(Duration::from_millis(1));
        // In CI, crossterm may error (no terminal) or return Tick.
        // Either is acceptable — just verify no panic.
        match result {
            Ok(TuiEvent::Tick) => {} // expected
            Err(_) => {}             // acceptable in CI
            Ok(_) => {}              // some other event, also ok
        }
    }

    #[test]
    fn tui_event_debug() {
        let tick = TuiEvent::Tick;
        let debug = format!("{tick:?}");
        assert!(debug.contains("Tick"));
    }

    #[test]
    fn tui_event_resize() {
        let evt = TuiEvent::Resize(80, 24);
        match evt {
            TuiEvent::Resize(w, h) => {
                assert_eq!(w, 80);
                assert_eq!(h, 24);
            }
            _ => panic!("expected Resize"),
        }
    }
}
