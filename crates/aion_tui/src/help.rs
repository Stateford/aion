//! Single source of truth for the TUI's key bindings and commands.
//!
//! Both the `?` help popup ([`crate::render`]) and the `help` command text
//! ([`crate::app`]) are generated from the tables in this module, so they
//! cannot drift apart from each other. Tests verify the tables against the
//! key handler and the command parsers.

/// One help entry: the key or command spelling(s) and what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HelpEntry {
    /// Key(s) or command spelling(s) as shown to the user, e.g. `"run (r) <dur>"`.
    pub usage: &'static str,
    /// One-line description of the effect.
    pub description: &'static str,
}

const fn e(usage: &'static str, description: &'static str) -> HelpEntry {
    HelpEntry { usage, description }
}

/// Keys handled in normal mode by `TuiApp::handle_normal_key`.
pub const KEY_BINDINGS: &[HelpEntry] = &[
    e("j/k  ↑/↓", "Select signal"),
    e("h/l  ←/→", "Scroll waveform"),
    e("+ or =", "Zoom in"),
    e("-", "Zoom out"),
    e("f", "Fit viewport to time range"),
    e("Space", "Step simulation"),
    e("Enter", "Toggle signal in waveform"),
    e("e", "Expand/collapse bus bits"),
    e("Tab", "Switch panel focus"),
    e("d", "Cycle value format (hex/bin/dec)"),
    e("?", "Toggle help (j/k scroll, Esc close)"),
    e(":", "Enter command mode"),
    e("q", "Quit"),
];

/// Commands accepted by `parse_tui_command` and the simulation `parse_command`.
pub const COMMANDS: &[HelpEntry] = &[
    e("run (r) <dur>", "Run for duration, e.g. 'run 100ns'"),
    e("step (s)", "Single delta step"),
    e("continue (c)", "Run to completion"),
    e("goto (g) <time>", "Jump cursor to time"),
    e("inspect (i) <sig..>", "Show current signal value(s)"),
    e("breakpoint (bp) <t>", "Break at time, '@100ns' or '100ns'"),
    e("watch (w) <sig>", "Break when signal changes"),
    e("unwatch <sig>", "Remove a signal watch"),
    e("time (t)", "Show current simulation time"),
    e("signals (sig)", "List signals"),
    e("status", "Show simulation status"),
    e("zoomin (zi)", "Zoom in"),
    e("zoomout (zo)", "Zoom out"),
    e("zoomfit (zf), fit", "Fit viewport to time range"),
    e("add <sig>", "Add signal to waveform"),
    e("remove (rm) <sig>", "Remove signal from waveform"),
    e("format (fmt)", "Cycle value format"),
    e("help (h)", "Show this help"),
    e("quit (q)", "Exit"),
];

fn push_section(out: &mut String, title: &str, entries: &[HelpEntry]) {
    out.push_str(title);
    out.push_str(":\n");
    for entry in entries {
        out.push_str(&format!("  {:<21} {}\n", entry.usage, entry.description));
    }
}

/// Renders the full help text (keys, then commands) from the shared tables.
pub fn help_body() -> String {
    let mut out = String::new();
    push_section(&mut out, "Keys", KEY_BINDINGS);
    out.push('\n');
    push_section(&mut out, "Commands", COMMANDS);
    out.truncate(out.trim_end().len());
    out
}

/// Number of lines in [`help_body`].
pub fn help_line_count() -> usize {
    help_body().lines().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::parse_tui_command;

    /// Every first-position alias in a usage string (`name (alias), other <arg>`).
    fn spellings(usage: &str) -> Vec<String> {
        usage
            .split(',')
            .flat_map(|part| {
                let mut v = Vec::new();
                for tok in part.split_whitespace() {
                    if tok.starts_with('<') {
                        break;
                    }
                    v.push(tok.trim_matches(|c| c == '(' || c == ')').to_string());
                }
                v
            })
            .collect()
    }

    #[test]
    fn every_listed_command_spelling_parses() {
        for entry in COMMANDS {
            for name in spellings(entry.usage) {
                let input = format!("{name} 1ns");
                match parse_tui_command(&input) {
                    Ok(_) => {}
                    Err(msg) => assert!(
                        !msg.starts_with("unknown command"),
                        "help lists '{name}' but the parser rejects it: {msg}"
                    ),
                }
            }
        }
    }

    #[test]
    fn every_accepted_command_is_documented() {
        let body = help_body();
        let accepted = [
            "run",
            "r",
            "step",
            "s",
            "continue",
            "c",
            "goto",
            "g",
            "inspect",
            "i",
            "breakpoint",
            "bp",
            "watch",
            "w",
            "unwatch",
            "time",
            "t",
            "signals",
            "sig",
            "status",
            "help",
            "h",
            "zoomin",
            "zi",
            "zoomout",
            "zo",
            "zoomfit",
            "zf",
            "fit",
            "add",
            "remove",
            "rm",
            "format",
            "fmt",
            "quit",
            "q",
        ];
        let documented: Vec<String> = COMMANDS.iter().flat_map(|c| spellings(c.usage)).collect();
        for name in accepted {
            assert!(documented.iter().any(|d| d == name), "undocumented: {name}");
        }
        assert!(body.contains("Commands:"));
    }

    #[test]
    fn every_handled_key_is_documented() {
        let keys: String = KEY_BINDINGS.iter().map(|k| k.usage).collect();
        for key in [
            "j", "k", "h", "l", "+", "=", "-", "f", "e", "d", "?", ":", "q",
        ] {
            assert!(keys.contains(key), "undocumented key: {key}");
        }
        for key in ["Space", "Enter", "Tab", "↑", "↓", "←", "→"] {
            assert!(keys.contains(key), "undocumented key: {key}");
        }
    }

    #[test]
    fn zoom_aliases_not_misleading() {
        let body = help_body();
        assert!(!body.contains("zoomin/zo"));
        assert!(body.contains("zoomout (zo)"));
        assert!(body.contains("zoomin (zi)"));
    }

    #[test]
    fn line_count_matches_body() {
        assert_eq!(help_line_count(), help_body().lines().count());
        assert!(help_line_count() > KEY_BINDINGS.len() + COMMANDS.len());
    }
}
