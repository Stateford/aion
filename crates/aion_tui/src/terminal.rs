//! Terminal setup and teardown for the TUI.
//!
//! Provides helpers to enter and exit raw mode, enable the alternate
//! screen buffer, and install a panic hook that restores the terminal
//! before printing the panic message.

use std::io::{self, Stdout};

use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// A ratatui terminal backed by crossterm on stdout.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Initializes the terminal for TUI rendering.
///
/// Enables raw mode, switches to the alternate screen buffer, and
/// returns a ratatui `Terminal` ready for rendering. The caller must
/// call [`restore_terminal`] on exit (including on panic via the
/// panic hook installed by [`install_panic_hook`]).
pub fn init_terminal() -> io::Result<Tui> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restores the terminal to its original state.
///
/// Leaves the alternate screen buffer and disables raw mode. Safe to
/// call multiple times.
pub fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

/// Installs a panic hook that restores the terminal before printing.
///
/// Without this, a panic in TUI mode would leave the terminal in raw
/// mode with the alternate screen still active, making the error
/// message invisible.
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = restore_terminal();
        original_hook(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_hook_installs_without_error() {
        // Just verify that installing the panic hook doesn't panic.
        // We can't actually test terminal init in CI (no tty).
        install_panic_hook();
    }

    #[test]
    fn restore_terminal_is_idempotent() {
        // restore_terminal should not error even if not in raw mode.
        // In CI there's no terminal, so this may or may not error —
        // the important thing is it doesn't panic.
        let _ = restore_terminal();
        let _ = restore_terminal();
    }
}
