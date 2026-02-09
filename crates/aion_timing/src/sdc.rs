//! SDC/XDC timing constraint file parser.
//!
//! Parses Synopsys Design Constraints (SDC) and Xilinx Design Constraints (XDC)
//! files into [`TimingConstraints`]. Supports the most commonly used commands:
//!
//! - `create_clock` — define a clock
//! - `set_input_delay` — constrain input port timing
//! - `set_output_delay` — constrain output port timing
//! - `set_false_path` — exclude paths from timing analysis
//! - `set_multicycle_path` — allow multi-cycle paths
//! - `set_max_delay` — constrain maximum path delay
//!
//! The parser is line-based (one command per line, backslash continuation
//! supported) and does not attempt full Tcl interpretation.

use crate::constraints::{
    ClockConstraint, FalsePath, IoDelay, MaxDelayPath, MulticyclePath, TimingConstraints,
};
use aion_common::Interner;
use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink};
use aion_source::Span;

/// Parses an SDC/XDC constraint file into a [`TimingConstraints`] structure.
///
/// Lines starting with `#` are treated as comments. Backslash-newline
/// continuation is supported. Unrecognized commands are reported as warnings
/// and skipped. Parse errors within recognized commands are also reported
/// as warnings.
pub fn parse_sdc(source: &str, interner: &Interner, sink: &DiagnosticSink) -> TimingConstraints {
    let mut constraints = TimingConstraints::new();

    // Join continuation lines (backslash at end of line)
    let joined = join_continuation_lines(source);

    for line in joined.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let tokens: Vec<&str> = tokenize_sdc_line(trimmed);
        if tokens.is_empty() {
            continue;
        }

        match tokens[0] {
            "create_clock" => {
                parse_create_clock(&tokens[1..], interner, sink, &mut constraints);
            }
            "set_input_delay" => {
                parse_set_io_delay(&tokens[1..], interner, sink, &mut constraints, true);
            }
            "set_output_delay" => {
                parse_set_io_delay(&tokens[1..], interner, sink, &mut constraints, false);
            }
            "set_false_path" => {
                parse_set_false_path(&tokens[1..], interner, sink, &mut constraints);
            }
            "set_multicycle_path" => {
                parse_set_multicycle_path(&tokens[1..], interner, sink, &mut constraints);
            }
            "set_max_delay" => {
                parse_set_max_delay(&tokens[1..], interner, sink, &mut constraints);
            }
            cmd => {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::new(Category::Timing, 1),
                    format!("unrecognized SDC command: `{cmd}`"),
                    Span::DUMMY,
                ));
            }
        }
    }

    constraints
}

/// Joins backslash-continuation lines into single logical lines.
fn join_continuation_lines(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut continuation = false;

    for line in source.lines() {
        if continuation {
            result.push(' ');
        }
        let trimmed = line.trim_end();
        if let Some(stripped) = trimmed.strip_suffix('\\') {
            result.push_str(stripped);
            continuation = true;
        } else {
            result.push_str(trimmed);
            result.push('\n');
            continuation = false;
        }
    }

    result
}

/// Tokenizes an SDC line, handling basic quoting with braces and double quotes.
fn tokenize_sdc_line(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut chars = line.char_indices().peekable();

    while let Some(&(start, ch)) = chars.peek() {
        match ch {
            ' ' | '\t' => {
                chars.next();
            }
            '{' => {
                chars.next(); // skip '{'
                let inner_start = chars.peek().map_or(line.len(), |&(i, _)| i);
                let mut end = inner_start;
                for (i, c) in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    end = i + c.len_utf8();
                }
                tokens.push(&line[inner_start..end]);
            }
            '"' => {
                chars.next(); // skip '"'
                let inner_start = chars.peek().map_or(line.len(), |&(i, _)| i);
                let mut end = inner_start;
                for (i, c) in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    end = i + c.len_utf8();
                }
                tokens.push(&line[inner_start..end]);
            }
            '[' => {
                // Skip Tcl command substitution [get_ports ...] — capture whole bracket expr
                let bracket_start = start;
                chars.next(); // skip '['
                let mut depth = 1;
                let mut end = start + 1;
                for (i, c) in chars.by_ref() {
                    end = i + c.len_utf8();
                    if c == '[' {
                        depth += 1;
                    } else if c == ']' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                // Extract inner content, skipping [get_ports ...]
                let inner = &line[bracket_start + 1..end - 1];
                // Try to extract the port name from "get_ports name" pattern
                if let Some(rest) = inner.strip_prefix("get_ports") {
                    let port_name = rest.trim().trim_matches(|c| c == '{' || c == '}');
                    if !port_name.is_empty() {
                        tokens.push(port_name);
                    }
                } else {
                    tokens.push(inner.trim());
                }
            }
            _ => {
                let mut end = start;
                for (i, c) in chars.by_ref() {
                    if c == ' ' || c == '\t' {
                        break;
                    }
                    end = i + c.len_utf8();
                }
                if end == start {
                    // Single char token
                    end = start + ch.len_utf8();
                    chars.next();
                }
                tokens.push(&line[start..end]);
            }
        }
    }

    tokens
}

/// Parses `create_clock -period <val> -name <name> [-waveform {rise fall}] [port]`.
fn parse_create_clock(
    args: &[&str],
    interner: &Interner,
    sink: &DiagnosticSink,
    constraints: &mut TimingConstraints,
) {
    let mut period: Option<f64> = None;
    let mut name: Option<&str> = None;
    let mut waveform: Option<(f64, f64)> = None;
    let mut port: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-period" => {
                i += 1;
                if i < args.len() {
                    period = args[i].parse().ok();
                }
            }
            "-name" => {
                i += 1;
                if i < args.len() {
                    name = Some(args[i]);
                }
            }
            "-waveform" => {
                i += 1;
                if i < args.len() {
                    let parts: Vec<&str> = args[i].split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let (Ok(r), Ok(f)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
                            waveform = Some((r, f));
                        }
                    }
                }
            }
            s if !s.starts_with('-') => {
                port = Some(s);
            }
            _ => {}
        }
        i += 1;
    }

    let Some(period_val) = period else {
        sink.emit(Diagnostic::warning(
            DiagnosticCode::new(Category::Timing, 2),
            "create_clock: missing -period".to_string(),
            Span::DUMMY,
        ));
        return;
    };

    let clock_name = name.or(port).unwrap_or("default_clock");
    let port_name = port.or(name).unwrap_or("clk");

    constraints.clocks.push(ClockConstraint {
        name: interner.get_or_intern(clock_name),
        period_ns: period_val,
        port: interner.get_or_intern(port_name),
        waveform,
    });
}

/// Parses `set_input_delay`/`set_output_delay -clock <clk> <delay> [port]`.
fn parse_set_io_delay(
    args: &[&str],
    interner: &Interner,
    sink: &DiagnosticSink,
    constraints: &mut TimingConstraints,
    is_input: bool,
) {
    let mut clock: Option<&str> = None;
    let mut delay: Option<f64> = None;
    let mut port: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-clock" => {
                i += 1;
                if i < args.len() {
                    clock = Some(args[i]);
                }
            }
            s if !s.starts_with('-') => {
                if delay.is_none() {
                    if let Ok(v) = s.parse::<f64>() {
                        delay = Some(v);
                    } else {
                        port = Some(s);
                    }
                } else {
                    port = Some(s);
                }
            }
            _ => {}
        }
        i += 1;
    }

    let (Some(clock_name), Some(delay_val)) = (clock, delay) else {
        let cmd = if is_input {
            "set_input_delay"
        } else {
            "set_output_delay"
        };
        sink.emit(Diagnostic::warning(
            DiagnosticCode::new(Category::Timing, 3),
            format!("{cmd}: missing -clock or delay value"),
            Span::DUMMY,
        ));
        return;
    };

    let port_name = port.unwrap_or("*");
    let io_delay = IoDelay {
        port: interner.get_or_intern(port_name),
        clock: interner.get_or_intern(clock_name),
        delay_ns: delay_val,
    };

    if is_input {
        constraints.input_delays.push(io_delay);
    } else {
        constraints.output_delays.push(io_delay);
    }
}

/// Parses `set_false_path -from <from> -to <to>`.
fn parse_set_false_path(
    args: &[&str],
    interner: &Interner,
    _sink: &DiagnosticSink,
    constraints: &mut TimingConstraints,
) {
    let mut from = Vec::new();
    let mut to = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-from" => {
                i += 1;
                if i < args.len() {
                    from.push(interner.get_or_intern(args[i]));
                }
            }
            "-to" => {
                i += 1;
                if i < args.len() {
                    to.push(interner.get_or_intern(args[i]));
                }
            }
            _ => {}
        }
        i += 1;
    }

    constraints.false_paths.push(FalsePath { from, to });
}

/// Parses `set_multicycle_path -setup <N> -from <from> -to <to>`.
fn parse_set_multicycle_path(
    args: &[&str],
    interner: &Interner,
    _sink: &DiagnosticSink,
    constraints: &mut TimingConstraints,
) {
    let mut cycles: u32 = 2; // default
    let mut from = Vec::new();
    let mut to = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-setup" => {
                i += 1;
                if i < args.len() {
                    cycles = args[i].parse().unwrap_or(2);
                }
            }
            "-from" => {
                i += 1;
                if i < args.len() {
                    from.push(interner.get_or_intern(args[i]));
                }
            }
            "-to" => {
                i += 1;
                if i < args.len() {
                    to.push(interner.get_or_intern(args[i]));
                }
            }
            _ => {}
        }
        i += 1;
    }

    constraints
        .multicycle_paths
        .push(MulticyclePath { from, to, cycles });
}

/// Parses `set_max_delay <delay> -from <from> -to <to>`.
fn parse_set_max_delay(
    args: &[&str],
    interner: &Interner,
    _sink: &DiagnosticSink,
    constraints: &mut TimingConstraints,
) {
    let mut delay: Option<f64> = None;
    let mut from = Vec::new();
    let mut to = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-from" => {
                i += 1;
                if i < args.len() {
                    from.push(interner.get_or_intern(args[i]));
                }
            }
            "-to" => {
                i += 1;
                if i < args.len() {
                    to.push(interner.get_or_intern(args[i]));
                }
            }
            s if !s.starts_with('-') => {
                if delay.is_none() {
                    delay = s.parse().ok();
                }
            }
            _ => {}
        }
        i += 1;
    }

    let delay_ns = delay.unwrap_or(0.0);
    constraints
        .max_delay_paths
        .push(MaxDelayPath { from, to, delay_ns });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> (TimingConstraints, Vec<Diagnostic>) {
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let tc = parse_sdc(source, &interner, &sink);
        let diags = sink.take_all();
        (tc, diags)
    }

    #[test]
    fn empty_file() {
        let (tc, diags) = parse("");
        assert_eq!(tc.clock_count(), 0);
        assert!(diags.is_empty());
    }

    #[test]
    fn comments_only() {
        let (tc, diags) = parse("# This is a comment\n# Another comment\n");
        assert_eq!(tc.clock_count(), 0);
        assert!(diags.is_empty());
    }

    #[test]
    fn create_clock_simple() {
        let (tc, diags) = parse("create_clock -period 10.0 -name sys_clk clk_port");
        assert_eq!(tc.clock_count(), 1);
        assert_eq!(tc.clocks[0].period_ns, 10.0);
        assert!(diags.is_empty());
    }

    #[test]
    fn create_clock_with_waveform() {
        let (tc, _) = parse("create_clock -period 10.0 -name clk -waveform {0.0 5.0} port");
        assert_eq!(tc.clock_count(), 1);
        assert_eq!(tc.clocks[0].waveform, Some((0.0, 5.0)));
    }

    #[test]
    fn create_clock_missing_period() {
        let (tc, diags) = parse("create_clock -name clk clk_port");
        assert_eq!(tc.clock_count(), 0);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("missing -period"));
    }

    #[test]
    fn create_clock_with_get_ports() {
        let (tc, _) = parse("create_clock -period 8.0 -name fast_clk [get_ports clk_in]");
        assert_eq!(tc.clock_count(), 1);
        assert_eq!(tc.clocks[0].period_ns, 8.0);
    }

    #[test]
    fn set_input_delay() {
        let (tc, diags) = parse("set_input_delay -clock clk 2.0 data_in");
        assert_eq!(tc.input_delays.len(), 1);
        assert_eq!(tc.input_delays[0].delay_ns, 2.0);
        assert!(diags.is_empty());
    }

    #[test]
    fn set_output_delay() {
        let (tc, diags) = parse("set_output_delay -clock clk 1.5 data_out");
        assert_eq!(tc.output_delays.len(), 1);
        assert_eq!(tc.output_delays[0].delay_ns, 1.5);
        assert!(diags.is_empty());
    }

    #[test]
    fn set_io_delay_missing_clock() {
        let (_, diags) = parse("set_input_delay 2.0 data_in");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("missing -clock"));
    }

    #[test]
    fn set_false_path() {
        let (tc, diags) = parse("set_false_path -from clk_a -to clk_b");
        assert_eq!(tc.false_paths.len(), 1);
        assert_eq!(tc.false_paths[0].from.len(), 1);
        assert_eq!(tc.false_paths[0].to.len(), 1);
        assert!(diags.is_empty());
    }

    #[test]
    fn set_multicycle_path() {
        let (tc, _) = parse("set_multicycle_path -setup 3 -from reg_a -to reg_b");
        assert_eq!(tc.multicycle_paths.len(), 1);
        assert_eq!(tc.multicycle_paths[0].cycles, 3);
    }

    #[test]
    fn set_max_delay() {
        let (tc, _) = parse("set_max_delay 15.0 -from src -to dst");
        assert_eq!(tc.max_delay_paths.len(), 1);
        assert_eq!(tc.max_delay_paths[0].delay_ns, 15.0);
    }

    #[test]
    fn unrecognized_command() {
        let (_, diags) = parse("set_driving_cell -lib_cell BUF data_in");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unrecognized SDC command"));
    }

    #[test]
    fn multiple_commands() {
        let sdc = r#"
# Clock definitions
create_clock -period 10.0 -name clk clk_port
create_clock -period 5.0 -name fast_clk fast_port

# I/O delays
set_input_delay -clock clk 2.0 data_in
set_output_delay -clock clk 1.0 data_out

# Exceptions
set_false_path -from clk -to fast_clk
set_multicycle_path -setup 2 -from slow_reg -to fast_reg
set_max_delay 20.0 -from async_in -to sync_out
"#;
        let (tc, diags) = parse(sdc);
        assert_eq!(tc.clock_count(), 2);
        assert_eq!(tc.input_delays.len(), 1);
        assert_eq!(tc.output_delays.len(), 1);
        assert_eq!(tc.false_paths.len(), 1);
        assert_eq!(tc.multicycle_paths.len(), 1);
        assert_eq!(tc.max_delay_paths.len(), 1);
        assert!(diags.is_empty());
    }

    #[test]
    fn continuation_lines() {
        let sdc = "create_clock \\\n  -period 10.0 \\\n  -name clk \\\n  clk_port";
        let (tc, _) = parse(sdc);
        assert_eq!(tc.clock_count(), 1);
        assert_eq!(tc.clocks[0].period_ns, 10.0);
    }

    #[test]
    fn join_continuation_lines_basic() {
        let input = "line1 \\\nline2\nline3";
        let joined = join_continuation_lines(input);
        assert!(joined.contains("line1  line2"));
        assert!(joined.contains("line3"));
    }

    #[test]
    fn tokenize_braces() {
        let tokens = tokenize_sdc_line("create_clock -waveform {0.0 5.0} clk");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0], "create_clock");
        assert_eq!(tokens[1], "-waveform");
        assert_eq!(tokens[2], "0.0 5.0");
        assert_eq!(tokens[3], "clk");
    }

    #[test]
    fn tokenize_quotes() {
        let tokens = tokenize_sdc_line("set_max_delay -from \"reg_a\" -to \"reg_b\" 5.0");
        assert!(tokens.contains(&"reg_a"));
        assert!(tokens.contains(&"reg_b"));
    }

    #[test]
    fn tokenize_get_ports() {
        let tokens = tokenize_sdc_line("create_clock -period 10.0 [get_ports clk]");
        assert!(tokens.contains(&"clk"));
    }

    #[test]
    fn empty_false_path() {
        let (tc, _) = parse("set_false_path");
        assert_eq!(tc.false_paths.len(), 1);
        assert!(tc.false_paths[0].from.is_empty());
        assert!(tc.false_paths[0].to.is_empty());
    }

    #[test]
    fn multicycle_default_cycles() {
        let (tc, _) = parse("set_multicycle_path -from a -to b");
        assert_eq!(tc.multicycle_paths[0].cycles, 2);
    }

    #[test]
    fn max_delay_no_value() {
        let (tc, _) = parse("set_max_delay -from a -to b");
        assert_eq!(tc.max_delay_paths[0].delay_ns, 0.0);
    }

    #[test]
    fn whitespace_handling() {
        let (tc, _) = parse("  create_clock  -period  10.0  -name  clk  port  ");
        assert_eq!(tc.clock_count(), 1);
    }

    #[test]
    fn mixed_comments_and_commands() {
        let sdc = r#"
# Header comment
create_clock -period 10.0 -name clk port
# Another comment
set_input_delay -clock clk 2.0 data
# Trailing comment
"#;
        let (tc, _) = parse(sdc);
        assert_eq!(tc.clock_count(), 1);
        assert_eq!(tc.input_delays.len(), 1);
    }
}
