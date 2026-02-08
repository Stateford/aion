//! Interactive REPL simulation debugger.
//!
//! Provides [`InteractiveSim`] which wraps a [`SimKernel`] with a
//! command-driven interface for stepping through simulations, inspecting
//! signal values, setting breakpoints, and watching signals.
//!
//! # Usage
//!
//! ```ignore
//! use aion_sim::interactive::InteractiveSim;
//!
//! let mut isim = InteractiveSim::new(&design, &interner)?;
//! isim.initialize()?;
//! isim.run_repl(&mut std::io::stdin().lock(), &mut std::io::stdout())?;
//! ```

use std::io::{BufRead, Write};

use aion_common::Interner;
use aion_ir::Design;

use crate::error::SimError;
use crate::kernel::{SimKernel, StepResult};
use crate::time::{FS_PER_MS, FS_PER_NS, FS_PER_PS, FS_PER_US};

/// Femtoseconds per second.
const FS_PER_S: u64 = FS_PER_MS * 1_000;

/// A simulation command parsed from user input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimCommand {
    /// Run for a specified duration in femtoseconds.
    Run {
        /// Duration to run in femtoseconds.
        duration_fs: u64,
    },
    /// Execute a single delta cycle step.
    Step,
    /// Inspect one or more signal values.
    Inspect {
        /// Signal name patterns to inspect.
        signals: Vec<String>,
    },
    /// Set a time breakpoint.
    BreakpointTime {
        /// Breakpoint time in femtoseconds.
        time_fs: u64,
    },
    /// Add a signal to the watch list.
    Watch {
        /// Signal name to watch.
        signal: String,
    },
    /// Remove a signal from the watch list.
    Unwatch {
        /// Signal name to remove.
        signal: String,
    },
    /// Continue execution until next breakpoint or completion.
    Continue,
    /// Display current simulation time.
    Time,
    /// List all signals.
    Signals,
    /// Show simulation status.
    Status,
    /// Display help text.
    Help,
    /// Quit the interactive session.
    Quit,
}

/// A time breakpoint in the simulation.
#[derive(Clone, Debug)]
struct Breakpoint {
    /// Unique breakpoint ID.
    id: u32,
    /// Time in femtoseconds to break at.
    time_fs: u64,
}

/// An entry in the signal watch list.
#[derive(Clone, Debug)]
struct WatchEntry {
    /// Signal name pattern.
    name: String,
}

/// Result of executing a simulation command.
#[derive(Clone, Debug)]
pub enum CommandResult {
    /// Command produced text output.
    Output(String),
    /// Simulation should quit.
    Quit,
    /// Simulation hit a breakpoint.
    BreakpointHit {
        /// The breakpoint ID.
        bp_id: u32,
    },
    /// Simulation finished ($finish or no events).
    Finished,
}

/// Interactive simulation wrapper providing REPL-style debugging.
///
/// Wraps a [`SimKernel`] with breakpoints, watches, command history,
/// and a text-based interface for stepping through simulations.
pub struct InteractiveSim {
    kernel: SimKernel,
    breakpoints: Vec<Breakpoint>,
    next_bp_id: u32,
    watches: Vec<WatchEntry>,
    history: Vec<String>,
    initialized: bool,
}

impl InteractiveSim {
    /// Creates a new interactive simulation from an elaborated design.
    ///
    /// The `interner` is used to resolve interned signal names.
    pub fn new(design: &Design, interner: &Interner) -> Result<Self, SimError> {
        let kernel = SimKernel::new(design, interner)?;
        Ok(Self {
            kernel,
            breakpoints: Vec::new(),
            next_bp_id: 1,
            watches: Vec::new(),
            history: Vec::new(),
            initialized: false,
        })
    }

    /// Initializes the simulation by running initial and combinational processes.
    ///
    /// Must be called exactly once before executing any simulation commands.
    pub fn initialize(&mut self) -> Result<(), SimError> {
        if self.initialized {
            return Ok(());
        }
        self.kernel.initialize()?;
        self.initialized = true;
        Ok(())
    }

    /// Executes a single simulation command and returns the result.
    pub fn execute(&mut self, cmd: &SimCommand) -> Result<CommandResult, SimError> {
        match cmd {
            SimCommand::Run { duration_fs } => self.cmd_run(*duration_fs),
            SimCommand::Step => self.cmd_step(),
            SimCommand::Inspect { signals } => self.cmd_inspect(signals),
            SimCommand::BreakpointTime { time_fs } => self.cmd_breakpoint_time(*time_fs),
            SimCommand::Watch { signal } => self.cmd_watch(signal),
            SimCommand::Unwatch { signal } => self.cmd_unwatch(signal),
            SimCommand::Continue => self.cmd_continue(),
            SimCommand::Time => self.cmd_time(),
            SimCommand::Signals => self.cmd_signals(),
            SimCommand::Status => self.cmd_status(),
            SimCommand::Help => Ok(CommandResult::Output(help_text())),
            SimCommand::Quit => Ok(CommandResult::Quit),
        }
    }

    /// Runs the REPL loop, reading commands from `input` and writing to `output`.
    pub fn run_repl<R: BufRead, W: Write>(
        &mut self,
        input: &mut R,
        output: &mut W,
    ) -> Result<(), SimError> {
        if !self.initialized {
            self.initialize()?;
        }

        // Print initial display output
        let initial_output = self.kernel.take_display_output();
        for line in &initial_output {
            let _ = writeln!(output, "{line}");
        }

        writeln!(output, "Aion Interactive Simulator")?;
        writeln!(output, "Type 'help' for available commands.")?;
        writeln!(
            output,
            "Simulation initialized at {}",
            self.kernel.current_time()
        )?;
        writeln!(output)?;

        let mut line = String::new();
        loop {
            write!(output, "aion> ")?;
            output.flush()?;

            line.clear();
            let bytes_read = input.read_line(&mut line)?;
            if bytes_read == 0 {
                // EOF
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            self.history.push(trimmed.to_string());

            match parse_command(trimmed) {
                Ok(cmd) => match self.execute(&cmd)? {
                    CommandResult::Output(text) => {
                        writeln!(output, "{text}")?;
                        // Print any display output generated
                        for msg in self.kernel.take_display_output() {
                            writeln!(output, "[display] {msg}")?;
                        }
                        // Print watched signals
                        self.print_watches(output)?;
                    }
                    CommandResult::Quit => {
                        writeln!(output, "Goodbye.")?;
                        break;
                    }
                    CommandResult::BreakpointHit { bp_id } => {
                        writeln!(
                            output,
                            "Breakpoint #{bp_id} hit at {}",
                            self.kernel.current_time()
                        )?;
                        self.print_watches(output)?;
                    }
                    CommandResult::Finished => {
                        writeln!(
                            output,
                            "Simulation finished at {}",
                            self.kernel.current_time()
                        )?;
                        for msg in self.kernel.take_display_output() {
                            writeln!(output, "[display] {msg}")?;
                        }
                        for fail in self.kernel.take_assertion_failures() {
                            writeln!(output, "[FAIL] {fail}")?;
                        }
                    }
                },
                Err(err) => {
                    writeln!(output, "Error: {err}")?;
                }
            }
        }

        Ok(())
    }

    /// Prints watched signal values.
    fn print_watches<W: Write>(&self, output: &mut W) -> Result<(), SimError> {
        if self.watches.is_empty() {
            return Ok(());
        }
        for watch in &self.watches {
            if let Some(id) = self.kernel.find_signal(&watch.name) {
                let val = self.kernel.signal_value(id);
                writeln!(output, "  [watch] {} = {}", watch.name, format_value(val))?;
            }
        }
        Ok(())
    }

    fn cmd_run(&mut self, duration_fs: u64) -> Result<CommandResult, SimError> {
        let target_fs = self.kernel.current_time().fs + duration_fs;

        loop {
            if self.kernel.is_finished() || !self.kernel.has_pending_events() {
                return Ok(CommandResult::Finished);
            }

            // Check breakpoints before stepping
            if let Some(bp) = self.check_breakpoints(target_fs) {
                return Ok(CommandResult::BreakpointHit { bp_id: bp });
            }

            if self.kernel.current_time().fs >= target_fs {
                break;
            }

            let result = self.kernel.step_delta()?;
            if result == StepResult::Done {
                return Ok(CommandResult::Finished);
            }
        }

        Ok(CommandResult::Output(format!(
            "Ran to {}",
            self.kernel.current_time()
        )))
    }

    fn cmd_step(&mut self) -> Result<CommandResult, SimError> {
        let result = self.kernel.step_delta()?;
        match result {
            StepResult::Continued => Ok(CommandResult::Output(format!(
                "Stepped to {}",
                self.kernel.current_time()
            ))),
            StepResult::Done => Ok(CommandResult::Finished),
        }
    }

    fn cmd_inspect(&self, signals: &[String]) -> Result<CommandResult, SimError> {
        let mut output = String::new();
        for name in signals {
            match self.kernel.find_signal(name) {
                Some(id) => {
                    let val = self.kernel.signal_value(id);
                    output.push_str(&format!("{name} = {}\n", format_value(val)));
                }
                None => {
                    // Try partial match
                    let all = self.kernel.all_signals();
                    let matches: Vec<_> = all
                        .iter()
                        .filter(|(_, n, _)| n.contains(name.as_str()))
                        .collect();
                    if matches.is_empty() {
                        output.push_str(&format!("Signal not found: {name}\n"));
                    } else {
                        for (id, matched_name, _) in matches {
                            let val = self.kernel.signal_value(*id);
                            output.push_str(&format!("{matched_name} = {}\n", format_value(val)));
                        }
                    }
                }
            }
        }
        if output.ends_with('\n') {
            output.truncate(output.len() - 1);
        }
        Ok(CommandResult::Output(output))
    }

    fn cmd_breakpoint_time(&mut self, time_fs: u64) -> Result<CommandResult, SimError> {
        let id = self.next_bp_id;
        self.next_bp_id += 1;
        self.breakpoints.push(Breakpoint { id, time_fs });
        Ok(CommandResult::Output(format!(
            "Breakpoint #{id} set at {}",
            format_time_fs(time_fs)
        )))
    }

    fn cmd_watch(&mut self, signal: &str) -> Result<CommandResult, SimError> {
        if self.kernel.find_signal(signal).is_none() {
            return Ok(CommandResult::Output(format!(
                "Warning: signal '{signal}' not found (will watch if it appears)"
            )));
        }
        self.watches.push(WatchEntry {
            name: signal.to_string(),
        });
        Ok(CommandResult::Output(format!("Watching '{signal}'")))
    }

    fn cmd_unwatch(&mut self, signal: &str) -> Result<CommandResult, SimError> {
        let before = self.watches.len();
        self.watches.retain(|w| w.name != signal);
        if self.watches.len() < before {
            Ok(CommandResult::Output(format!("Unwatched '{signal}'")))
        } else {
            Ok(CommandResult::Output(format!(
                "Signal '{signal}' was not being watched"
            )))
        }
    }

    fn cmd_continue(&mut self) -> Result<CommandResult, SimError> {
        loop {
            if self.kernel.is_finished() || !self.kernel.has_pending_events() {
                return Ok(CommandResult::Finished);
            }

            if let Some(bp) = self.check_breakpoints(u64::MAX) {
                return Ok(CommandResult::BreakpointHit { bp_id: bp });
            }

            let result = self.kernel.step_delta()?;
            if result == StepResult::Done {
                return Ok(CommandResult::Finished);
            }
        }
    }

    fn cmd_time(&self) -> Result<CommandResult, SimError> {
        Ok(CommandResult::Output(format!(
            "Current time: {}",
            self.kernel.current_time()
        )))
    }

    fn cmd_signals(&self) -> Result<CommandResult, SimError> {
        let signals = self.kernel.all_signals();
        if signals.is_empty() {
            return Ok(CommandResult::Output("No signals".to_string()));
        }
        let mut output = String::new();
        output.push_str(&format!("{} signal(s):\n", signals.len()));
        for (_, name, width) in &signals {
            output.push_str(&format!("  {name} [{width} bit]\n"));
        }
        if output.ends_with('\n') {
            output.truncate(output.len() - 1);
        }
        Ok(CommandResult::Output(output))
    }

    fn cmd_status(&self) -> Result<CommandResult, SimError> {
        let mut output = String::new();
        output.push_str(&format!("Time: {}\n", self.kernel.current_time()));
        output.push_str(&format!("Signals: {}\n", self.kernel.signal_count()));
        output.push_str(&format!("Processes: {}\n", self.kernel.process_count()));
        output.push_str(&format!(
            "Pending events: {}\n",
            if self.kernel.has_pending_events() {
                "yes"
            } else {
                "no"
            }
        ));
        output.push_str(&format!(
            "Finished: {}\n",
            if self.kernel.is_finished() {
                "yes"
            } else {
                "no"
            }
        ));
        output.push_str(&format!("Breakpoints: {}\n", self.breakpoints.len()));
        output.push_str(&format!("Watches: {}", self.watches.len()));
        Ok(CommandResult::Output(output))
    }

    /// Checks if any breakpoint is at or before the current time.
    fn check_breakpoints(&self, _target_fs: u64) -> Option<u32> {
        let current_fs = self.kernel.current_time().fs;
        for bp in &self.breakpoints {
            if bp.time_fs <= current_fs {
                return Some(bp.id);
            }
        }
        None
    }
}

/// Parses a command string into a `SimCommand`.
///
/// Supports both full command names and single-character shortcuts:
/// `r`=run, `s`=step, `i`=inspect, `c`=continue, `t`=time,
/// `q`=quit, `h`=help, `bp`=breakpoint, `w`=watch, `sig`=signals.
pub fn parse_command(input: &str) -> Result<SimCommand, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty command".to_string());
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let cmd = parts[0].to_lowercase();
    let args = &parts[1..];

    match cmd.as_str() {
        "run" | "r" => {
            if args.is_empty() {
                return Err("run requires a duration (e.g., 'run 100ns')".to_string());
            }
            let duration_fs =
                parse_sim_duration(args[0]).map_err(|e| format!("invalid duration: {e}"))?;
            Ok(SimCommand::Run { duration_fs })
        }
        "step" | "s" => Ok(SimCommand::Step),
        "inspect" | "i" => {
            if args.is_empty() {
                return Err("inspect requires signal name(s)".to_string());
            }
            Ok(SimCommand::Inspect {
                signals: args.iter().map(|s| s.to_string()).collect(),
            })
        }
        "breakpoint" | "bp" => {
            if args.is_empty() {
                return Err(
                    "breakpoint requires a time (e.g., 'bp @100ns' or 'bp 100ns')".to_string(),
                );
            }
            let time_str = args[0].strip_prefix('@').unwrap_or(args[0]);
            let time_fs = parse_sim_duration(time_str)
                .map_err(|e| format!("invalid breakpoint time: {e}"))?;
            Ok(SimCommand::BreakpointTime { time_fs })
        }
        "watch" | "w" => {
            if args.is_empty() {
                return Err("watch requires a signal name".to_string());
            }
            Ok(SimCommand::Watch {
                signal: args[0].to_string(),
            })
        }
        "unwatch" => {
            if args.is_empty() {
                return Err("unwatch requires a signal name".to_string());
            }
            Ok(SimCommand::Unwatch {
                signal: args[0].to_string(),
            })
        }
        "continue" | "c" => Ok(SimCommand::Continue),
        "time" | "t" => Ok(SimCommand::Time),
        "signals" | "sig" => Ok(SimCommand::Signals),
        "status" => Ok(SimCommand::Status),
        "help" | "h" => Ok(SimCommand::Help),
        "quit" | "q" => Ok(SimCommand::Quit),
        _ => Err(format!("unknown command: '{}'", parts[0])),
    }
}

/// Parses a duration string into femtoseconds.
///
/// Supports units: `fs`, `ps`, `ns`, `us`, `ms`, `s`.
/// Returns the duration value in femtoseconds.
pub fn parse_sim_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".to_string());
    }

    let digit_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digit_end == 0 {
        return Err(format!("no numeric value in '{s}'"));
    }

    let number: u64 = s[..digit_end]
        .parse()
        .map_err(|_| format!("invalid number in '{s}'"))?;

    let unit = s[digit_end..].trim();
    let multiplier = match unit {
        "fs" => 1,
        "ps" => FS_PER_PS,
        "ns" => FS_PER_NS,
        "us" => FS_PER_US,
        "ms" => FS_PER_MS,
        "s" => FS_PER_S,
        "" => return Err(format!("missing unit in '{s}'")),
        _ => return Err(format!("unknown unit '{unit}'")),
    };

    Ok(number * multiplier)
}

/// Formats a `LogicVec` for display.
///
/// Single-bit values render as `0`, `1`, `x`, or `z`.
/// Multi-bit values without X/Z render as hex (e.g., `8'hff`).
/// Multi-bit values with X/Z render as binary (e.g., `4'bz0x1`).
pub fn format_value(val: &aion_common::LogicVec) -> String {
    use aion_common::Logic;
    let w = val.width();
    if w == 1 {
        match val.get(0) {
            Logic::Zero => "0".into(),
            Logic::One => "1".into(),
            Logic::X => "x".into(),
            Logic::Z => "z".into(),
        }
    } else {
        // Show hex if no X/Z, otherwise binary
        let has_xz = (0..w).any(|i| matches!(val.get(i), Logic::X | Logic::Z));
        if has_xz {
            let mut s = String::with_capacity(w as usize);
            for i in (0..w).rev() {
                s.push(match val.get(i) {
                    Logic::Zero => '0',
                    Logic::One => '1',
                    Logic::X => 'x',
                    Logic::Z => 'z',
                });
            }
            format!("{w}'b{s}")
        } else {
            match val.to_u64() {
                Some(v) => format!("{w}'h{v:x}"),
                None => format!("{w}'b..."),
            }
        }
    }
}

/// Formats a femtosecond timestamp for display.
fn format_time_fs(fs: u64) -> String {
    crate::time::SimTime::from_fs(fs).to_string()
}

/// Returns the help text for the interactive session.
fn help_text() -> String {
    "\
Commands:
  run <duration>     (r)   Run for duration (e.g., 'run 100ns')
  step               (s)   Execute one delta cycle
  inspect <signal>   (i)   Show signal value(s)
  breakpoint <time>  (bp)  Set time breakpoint (e.g., 'bp @100ns')
  watch <signal>     (w)   Add signal to watch list
  unwatch <signal>          Remove signal from watch list
  continue           (c)   Run until breakpoint or done
  time               (t)   Show current simulation time
  signals            (sig) List all signals
  status                   Show simulation status
  help               (h)   Show this help
  quit               (q)   Exit interactive mode

Duration units: fs, ps, ns, us, ms, s"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, Interner, LogicVec};
    use aion_ir::arena::Arena;
    use aion_ir::{
        Assignment, Design, Expr, Module, ModuleId, Process, ProcessId, ProcessKind, Sensitivity,
        Signal, SignalId, SignalKind, SignalRef, SourceMap, Statement, Type, TypeDb,
    };
    use aion_source::Span;

    /// Creates a test interner pre-populated with names matching `Ident::from_raw()` indices.
    fn make_test_interner() -> Interner {
        let interner = Interner::new();
        interner.get_or_intern("__dummy__"); // 0
        interner.get_or_intern("top"); // 1
        interner.get_or_intern("clk"); // 2
        interner.get_or_intern("out"); // 3
        interner
    }

    fn make_type_db() -> TypeDb {
        let mut types = TypeDb::new();
        types.intern(Type::Bit);
        types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        types.intern(Type::BitVec {
            width: 4,
            signed: false,
        });
        types
    }

    fn empty_module(id: u32, name: Ident) -> Module {
        Module {
            id: ModuleId::from_raw(id),
            name,
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(b"test"),
        }
    }

    fn make_simple_design() -> Design {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));

        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        top.assignments.push(Assignment {
            target: SignalRef::Signal(SignalId::from_raw(1)),
            value: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
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

    fn make_finish_design() -> Design {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        top.processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Finish { span: Span::DUMMY },
            sensitivity: Sensitivity::All,
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

    // -- Command parsing tests --

    #[test]
    fn parse_run_command() {
        let cmd = parse_command("run 100ns").unwrap();
        assert_eq!(
            cmd,
            SimCommand::Run {
                duration_fs: 100 * FS_PER_NS
            }
        );
    }

    #[test]
    fn parse_run_shortcut() {
        let cmd = parse_command("r 50us").unwrap();
        assert_eq!(
            cmd,
            SimCommand::Run {
                duration_fs: 50 * FS_PER_US
            }
        );
    }

    #[test]
    fn parse_step_command() {
        assert_eq!(parse_command("step").unwrap(), SimCommand::Step);
        assert_eq!(parse_command("s").unwrap(), SimCommand::Step);
    }

    #[test]
    fn parse_inspect_command() {
        let cmd = parse_command("inspect top.sig0").unwrap();
        assert_eq!(
            cmd,
            SimCommand::Inspect {
                signals: vec!["top.sig0".to_string()]
            }
        );
    }

    #[test]
    fn parse_inspect_shortcut() {
        let cmd = parse_command("i top.sig0 top.sig1").unwrap();
        assert_eq!(
            cmd,
            SimCommand::Inspect {
                signals: vec!["top.sig0".to_string(), "top.sig1".to_string()]
            }
        );
    }

    #[test]
    fn parse_breakpoint_command() {
        let cmd = parse_command("breakpoint @100ns").unwrap();
        assert_eq!(
            cmd,
            SimCommand::BreakpointTime {
                time_fs: 100 * FS_PER_NS
            }
        );
    }

    #[test]
    fn parse_breakpoint_no_at() {
        let cmd = parse_command("bp 50ns").unwrap();
        assert_eq!(
            cmd,
            SimCommand::BreakpointTime {
                time_fs: 50 * FS_PER_NS
            }
        );
    }

    #[test]
    fn parse_watch_command() {
        let cmd = parse_command("watch top.clk").unwrap();
        assert_eq!(
            cmd,
            SimCommand::Watch {
                signal: "top.clk".to_string()
            }
        );
    }

    #[test]
    fn parse_unwatch_command() {
        let cmd = parse_command("unwatch top.clk").unwrap();
        assert_eq!(
            cmd,
            SimCommand::Unwatch {
                signal: "top.clk".to_string()
            }
        );
    }

    #[test]
    fn parse_continue_command() {
        assert_eq!(parse_command("continue").unwrap(), SimCommand::Continue);
        assert_eq!(parse_command("c").unwrap(), SimCommand::Continue);
    }

    #[test]
    fn parse_time_command() {
        assert_eq!(parse_command("time").unwrap(), SimCommand::Time);
        assert_eq!(parse_command("t").unwrap(), SimCommand::Time);
    }

    #[test]
    fn parse_signals_command() {
        assert_eq!(parse_command("signals").unwrap(), SimCommand::Signals);
        assert_eq!(parse_command("sig").unwrap(), SimCommand::Signals);
    }

    #[test]
    fn parse_status_command() {
        assert_eq!(parse_command("status").unwrap(), SimCommand::Status);
    }

    #[test]
    fn parse_help_command() {
        assert_eq!(parse_command("help").unwrap(), SimCommand::Help);
        assert_eq!(parse_command("h").unwrap(), SimCommand::Help);
    }

    #[test]
    fn parse_quit_command() {
        assert_eq!(parse_command("quit").unwrap(), SimCommand::Quit);
        assert_eq!(parse_command("q").unwrap(), SimCommand::Quit);
    }

    #[test]
    fn parse_unknown_command() {
        let result = parse_command("foobar");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown command"));
    }

    #[test]
    fn parse_empty_command() {
        assert!(parse_command("").is_err());
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(parse_command("STEP").unwrap(), SimCommand::Step);
        assert_eq!(
            parse_command("Run 10ns").unwrap(),
            SimCommand::Run {
                duration_fs: 10 * FS_PER_NS
            }
        );
    }

    #[test]
    fn parse_missing_args_run() {
        assert!(parse_command("run").is_err());
    }

    #[test]
    fn parse_missing_args_inspect() {
        assert!(parse_command("inspect").is_err());
    }

    // -- Duration parsing tests --

    #[test]
    fn duration_nanoseconds() {
        assert_eq!(parse_sim_duration("100ns").unwrap(), 100 * FS_PER_NS);
    }

    #[test]
    fn duration_microseconds() {
        assert_eq!(parse_sim_duration("5us").unwrap(), 5 * FS_PER_US);
    }

    #[test]
    fn duration_picoseconds() {
        assert_eq!(parse_sim_duration("250ps").unwrap(), 250 * FS_PER_PS);
    }

    #[test]
    fn duration_femtoseconds() {
        assert_eq!(parse_sim_duration("42fs").unwrap(), 42);
    }

    #[test]
    fn duration_milliseconds() {
        assert_eq!(parse_sim_duration("10ms").unwrap(), 10 * FS_PER_MS);
    }

    #[test]
    fn duration_seconds() {
        assert_eq!(parse_sim_duration("1s").unwrap(), FS_PER_S);
    }

    // -- InteractiveSim construction tests --

    #[test]
    fn interactive_from_simple_design() {
        let design = make_simple_design();
        let isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        assert!(!isim.initialized);
        assert!(isim.breakpoints.is_empty());
        assert!(isim.watches.is_empty());
    }

    #[test]
    fn interactive_initialize() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        assert!(isim.initialized);
        // Double-init should be ok
        isim.initialize().unwrap();
    }

    #[test]
    fn interactive_empty_design_error() {
        let types = TypeDb::new();
        let design = Design {
            modules: Arena::new(),
            top: ModuleId::from_raw(0),
            types,
            source_map: SourceMap::new(),
        };
        assert!(InteractiveSim::new(&design, &make_test_interner()).is_err());
    }

    // -- Command execution tests --

    #[test]
    fn execute_time_command() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        let result = isim.execute(&SimCommand::Time).unwrap();
        match result {
            CommandResult::Output(s) => assert!(s.contains("Current time")),
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn execute_step_command() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        // After init, combinational propagation may have queued events
        let result = isim.execute(&SimCommand::Step).unwrap();
        // Either stepped forward or finished (no more events)
        assert!(matches!(
            result,
            CommandResult::Output(_) | CommandResult::Finished
        ));
    }

    #[test]
    fn execute_inspect_known_signal() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        let result = isim
            .execute(&SimCommand::Inspect {
                signals: vec!["top.clk".to_string()],
            })
            .unwrap();
        match result {
            CommandResult::Output(s) => assert!(s.contains("top.clk")),
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn execute_inspect_unknown_signal() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        let result = isim
            .execute(&SimCommand::Inspect {
                signals: vec!["nonexistent".to_string()],
            })
            .unwrap();
        match result {
            CommandResult::Output(s) => assert!(s.contains("not found")),
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn execute_signals_list() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        let result = isim.execute(&SimCommand::Signals).unwrap();
        match result {
            CommandResult::Output(s) => {
                assert!(s.contains("signal(s)"));
                assert!(s.contains("top.clk"));
            }
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn execute_breakpoint_add() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        let result = isim
            .execute(&SimCommand::BreakpointTime { time_fs: 100 })
            .unwrap();
        match result {
            CommandResult::Output(s) => assert!(s.contains("Breakpoint #1")),
            _ => panic!("expected Output"),
        }
        assert_eq!(isim.breakpoints.len(), 1);
    }

    #[test]
    fn execute_watch_add() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        let result = isim
            .execute(&SimCommand::Watch {
                signal: "top.clk".to_string(),
            })
            .unwrap();
        match result {
            CommandResult::Output(s) => assert!(s.contains("Watching")),
            _ => panic!("expected Output"),
        }
        assert_eq!(isim.watches.len(), 1);
    }

    #[test]
    fn execute_help_text() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        let result = isim.execute(&SimCommand::Help).unwrap();
        match result {
            CommandResult::Output(s) => {
                assert!(s.contains("run"));
                assert!(s.contains("step"));
                assert!(s.contains("inspect"));
            }
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn execute_quit_command() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        let result = isim.execute(&SimCommand::Quit).unwrap();
        assert!(matches!(result, CommandResult::Quit));
    }

    #[test]
    fn execute_status_command() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        let result = isim.execute(&SimCommand::Status).unwrap();
        match result {
            CommandResult::Output(s) => {
                assert!(s.contains("Time:"));
                assert!(s.contains("Signals:"));
                assert!(s.contains("Processes:"));
            }
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn execute_on_finish_design() {
        let design = make_finish_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        isim.initialize().unwrap();
        // Should be finished since initial block called $finish
        assert!(isim.kernel.is_finished());
    }

    // -- REPL integration tests --

    #[test]
    fn repl_quit_exits() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        let input = b"quit\n";
        let mut output = Vec::new();
        isim.run_repl(&mut &input[..], &mut output).unwrap();
        let out_str = String::from_utf8(output).unwrap();
        assert!(out_str.contains("Goodbye"));
    }

    #[test]
    fn repl_multiple_commands() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        let input = b"time\nstatus\nquit\n";
        let mut output = Vec::new();
        isim.run_repl(&mut &input[..], &mut output).unwrap();
        let out_str = String::from_utf8(output).unwrap();
        assert!(out_str.contains("Current time"));
        assert!(out_str.contains("Signals:"));
        assert!(out_str.contains("Goodbye"));
    }

    #[test]
    fn repl_unknown_command_recovers() {
        let design = make_simple_design();
        let mut isim = InteractiveSim::new(&design, &make_test_interner()).unwrap();
        let input = b"badcmd\ntime\nquit\n";
        let mut output = Vec::new();
        isim.run_repl(&mut &input[..], &mut output).unwrap();
        let out_str = String::from_utf8(output).unwrap();
        assert!(out_str.contains("Error: unknown command"));
        assert!(out_str.contains("Current time"));
        assert!(out_str.contains("Goodbye"));
    }

    // -- Format helpers tests --

    #[test]
    fn format_value_single_bit_values() {
        assert_eq!(format_value(&LogicVec::from_bool(false)), "0");
        assert_eq!(format_value(&LogicVec::from_bool(true)), "1");
    }

    #[test]
    fn format_value_multi_bit_hex() {
        let v = LogicVec::from_u64(0xFF, 8);
        assert_eq!(format_value(&v), "8'hff");
    }

    #[test]
    fn format_value_multi_bit_with_xz() {
        let mut v = LogicVec::new(4);
        v.set(0, aion_common::Logic::One);
        v.set(1, aion_common::Logic::X);
        v.set(2, aion_common::Logic::Zero);
        v.set(3, aion_common::Logic::Z);
        assert_eq!(format_value(&v), "4'bz0x1");
    }
}
