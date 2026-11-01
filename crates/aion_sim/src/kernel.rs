//! Simulation kernel with event queue, hierarchy flattening, and delta-cycle loop.
//!
//! [`SimKernel`] is the core simulation engine. It flattens the module hierarchy
//! at construction time, then runs an event-driven simulation loop with delta
//! cycles, multi-driver resolution, edge detection, and sensitivity-based
//! process wakeup.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use aion_common::{Interner, Logic, LogicVec};
use aion_ir::arena::Arena;
use aion_ir::{
    AssignKind, CellKind, ConstValue, Design, Edge, Expr, ModuleId, Process, ProcessKind,
    Sensitivity, SignalId, SignalKind, SignalRef, Statement, TypeDb,
};

use crate::error::SimError;
use crate::evaluator::{apply_write, exec_statement, EvalContext, ExecResult, PendingUpdate};
use crate::time::SimTime;
use crate::value::{DriveStrength, SimSignalId, SimSignalState};
use crate::waveform::WaveformRecorder;

/// An event scheduled in the simulation event queue.
#[derive(Debug, Clone)]
struct SimEvent {
    /// When this event should be applied.
    time: SimTime,
    /// The target signal.
    signal: SimSignalId,
    /// The new value.
    value: LogicVec,
    /// The drive strength.
    _strength: DriveStrength,
}

impl PartialEq for SimEvent {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}

impl Eq for SimEvent {}

impl PartialOrd for SimEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SimEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time.cmp(&other.time)
    }
}

/// A simulation process with its pre-computed metadata.
#[derive(Debug, Clone)]
struct SimProcess {
    /// Index into the kernel's process list (for identification).
    _index: usize,
    /// The process kind (Combinational, Sequential, Initial).
    kind: ProcessKind,
    /// Mapping from the process's module IR `SignalId` to flat `SimSignalId`.
    signal_map: HashMap<SignalId, SimSignalId>,
    /// The process body statement tree.
    body: Statement,
    /// The sensitivity specification.
    sensitivity: Sensitivity,
    /// The set of signals this process reads (for `Sensitivity::All`).
    read_signals: HashSet<SimSignalId>,
    /// Whether a change on a signal in the sensitivity list may start the
    /// body again (see [`is_retriggerable`]).
    retriggerable: bool,
}

/// Returns whether a process may be (re)started by its sensitivity list.
///
/// An `initial` block runs once at time 0 and afterwards only resumes from
/// its own continuations. A process whose body is a top-level `forever` loop
/// (how the elaborator lowers an `always` block without an event control,
/// e.g. `always #5 clk = ~clk;`) never completes, so it is likewise started
/// once and then driven only by its own delays. Neither belongs in the
/// sensitivity map.
fn is_retriggerable(kind: ProcessKind, body: &Statement) -> bool {
    kind != ProcessKind::Initial && !matches!(body, Statement::Forever { .. })
}

/// Where [`SimKernel::run_process`] starts executing a process.
enum ProcessEntry {
    /// From the top of the process body (time-0 start or sensitivity trigger).
    Body,
    /// From the continuation saved when the process last suspended on a delay.
    Resume(Statement),
}

/// The result of a completed simulation run.
#[derive(Debug, Clone)]
pub struct SimResult {
    /// The final simulation time when the run ended.
    pub final_time: SimTime,
    /// Whether the simulation was terminated by `$finish`.
    pub finished_by_user: bool,
    /// The total number of delta cycles executed.
    pub total_deltas: u64,
    /// All `$display` output collected during the run.
    pub display_output: Vec<String>,
    /// Assertion failure messages collected during the run.
    pub assertion_failures: Vec<String>,
}

/// The result of a single delta-cycle step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// Simulation can continue.
    Continued,
    /// Simulation is done (no more events or $finish).
    Done,
}

/// A process whose execution is suspended pending a delay.
#[derive(Debug, Clone)]
struct SuspendedProcess {
    /// The index of the process in the kernel's process list.
    process_idx: usize,
    /// The remaining statements to execute when the process wakes.
    continuation: Statement,
}

/// The simulation kernel: flattened hierarchy, event queue, and execution engine.
///
/// Construct via [`SimKernel::new`] from an elaborated [`Design`], then call
/// [`run`](SimKernel::run) or [`run_to_completion`](SimKernel::run_to_completion).
pub struct SimKernel {
    /// Current simulation time.
    current_time: SimTime,
    /// Min-heap event queue (earliest events first).
    event_queue: BinaryHeap<Reverse<SimEvent>>,
    /// All flattened simulation signals.
    signals: Arena<SimSignalId, SimSignalState>,
    /// All simulation processes.
    processes: Vec<SimProcess>,
    /// Optional waveform recorder.
    recorder: Option<Box<dyn WaveformRecorder>>,
    /// The type database (cloned from the design).
    types: TypeDb,
    /// Whether `$finish` has been called.
    finished: bool,
    /// Collected `$display` output.
    display_output: Vec<String>,
    /// Collected assertion failure messages.
    assertion_failures: Vec<String>,
    /// Optional time limit in femtoseconds.
    time_limit: Option<u64>,
    /// Mapping from SimSignalId to the processes sensitive to it.
    sensitivity_map: HashMap<SimSignalId, Vec<usize>>,
    /// Maximum delta cycles per time step (default 10,000).
    max_delta_per_step: u32,
    /// Total delta cycles executed.
    total_deltas: u64,
    /// Processes suspended by delay statements, sorted by wake time.
    ///
    /// Holds at most one entry per process: a suspended process is not
    /// re-triggered by its sensitivity list until it resumes and completes.
    suspended_processes: Vec<(SimTime, SuspendedProcess)>,
    /// Non-blocking assignment (NBA) region: updates from `<=` assignments
    /// made at the current time, in execution order.
    ///
    /// They are applied by [`apply_nba_region`](Self::apply_nba_region) only
    /// once no active work (signal events or due wakeups) remains at the
    /// current timestamp, matching the IEEE 1364/1800 region order.
    nba_queue: Vec<PendingUpdate>,
    /// Flat signals that back VHDL process-local variables
    /// ([`SignalKind::Variable`]); never part of an implicit sensitivity set.
    variables: HashSet<SimSignalId>,

    /// Whether the time-0 start (initial blocks plus the initial
    /// combinational pass) has run; shared by `initialize()`, `run()`,
    /// `run_to_completion()` and `run_until()` so it happens exactly once.
    initialized: bool,
    /// Whether the attached waveform recorder has received its signal
    /// declarations and initial value dump.
    waveform_started: bool,
}

impl SimKernel {
    /// Creates a new simulation kernel from an elaborated design.
    ///
    /// Flattens the module hierarchy, allocating flat signal IDs and creating
    /// simulation processes with their signal mappings. The `interner` is used
    /// to resolve interned signal and instance names into human-readable strings.
    pub fn new(design: &Design, interner: &Interner) -> Result<Self, SimError> {
        let top_id = design.top;
        if design.modules.is_empty() {
            return Err(SimError::NoTopModule);
        }

        let mut kernel = Self {
            current_time: SimTime::zero(),
            event_queue: BinaryHeap::new(),
            signals: Arena::new(),
            processes: Vec::new(),
            recorder: None,
            types: design.types.clone(),
            finished: false,
            display_output: Vec::new(),
            assertion_failures: Vec::new(),
            time_limit: None,
            sensitivity_map: HashMap::new(),
            max_delta_per_step: 10_000,
            total_deltas: 0,
            suspended_processes: Vec::new(),
            nba_queue: Vec::new(),
            variables: HashSet::new(),

            initialized: false,
            waveform_started: false,
        };

        // Flatten the hierarchy starting at top
        let mut signal_map = HashMap::new();
        kernel.flatten_module(design, top_id, "top", &mut signal_map, interner)?;

        // Build sensitivity map
        kernel.build_sensitivity_map();

        Ok(kernel)
    }

    /// Sets the time limit for the simulation.
    pub fn set_time_limit(&mut self, limit_fs: u64) {
        self.time_limit = Some(limit_fs);
    }

    /// Sets the maximum number of delta cycles per time step.
    pub fn set_max_delta(&mut self, max: u32) {
        self.max_delta_per_step = max;
    }

    /// Attaches a waveform recorder to the kernel.
    ///
    /// The recorder receives its signal declarations and a dump of the
    /// current signal values when the simulation next starts or resumes, so
    /// it may be attached before or after [`initialize`](Self::initialize).
    pub fn set_recorder(&mut self, recorder: Box<dyn WaveformRecorder>) {
        self.recorder = Some(recorder);
        self.waveform_started = false;
    }

    /// Returns the current simulation time.
    pub fn current_time(&self) -> SimTime {
        self.current_time
    }

    /// Returns the value of a signal by its flat ID.
    pub fn signal_value(&self, id: SimSignalId) -> &LogicVec {
        &self.signals.get(id).value
    }

    /// Finds a signal by hierarchical name, returning its flat ID.
    pub fn find_signal(&self, name: &str) -> Option<SimSignalId> {
        self.signals
            .iter()
            .find(|(_, s)| s.name == name)
            .map(|(id, _)| id)
    }

    /// Returns the number of flattened signals.
    pub fn signal_count(&self) -> usize {
        self.signals.len()
    }

    /// Returns the number of simulation processes.
    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    /// Runs the simulation for the given duration in femtoseconds.
    ///
    /// Returns [`SimError::DeltaCycleLimit`] if more than the configured
    /// maximum number of scheduler iterations (delta cycles or zero-delay
    /// wakeups) occur at a single timestamp, e.g. a combinational loop or an
    /// `initial forever #0;` process.
    pub fn run(&mut self, duration_fs: u64) -> Result<SimResult, SimError> {
        let end_time = self.current_time.fs + duration_fs;
        self.time_limit = Some(end_time);
        self.run_simulation()
    }

    /// Runs the simulation to completion (until event queue empties or `$finish`).
    ///
    /// Like [`run`](Self::run), returns [`SimError::DeltaCycleLimit`] when a
    /// single timestamp exceeds the per-step delta/zero-delay-wakeup budget.
    pub fn run_to_completion(&mut self) -> Result<SimResult, SimError> {
        self.run_simulation()
    }

    /// Executes a single delta-cycle step.
    ///
    /// If no signal events remain at the current time but non-blocking
    /// updates are waiting, the NBA region is applied first, so this step
    /// processes those updates.
    pub fn step_delta(&mut self) -> Result<StepResult, SimError> {
        if self
            .event_queue
            .peek()
            .is_none_or(|e| e.0.time.fs != self.current_time.fs)
        {
            self.flush_nba_queue();
        }

        if self.event_queue.is_empty() {
            return Ok(StepResult::Done);
        }

        // Get the next event time
        let next_time = self.event_queue.peek().unwrap().0.time;

        // Check time limit
        if let Some(limit) = self.time_limit {
            if next_time.fs > limit {
                return Ok(StepResult::Done);
            }
        }

        self.current_time = next_time;

        // Dequeue all events at current time
        let mut events = Vec::new();
        while let Some(Reverse(evt)) = self.event_queue.peek() {
            if evt.time == self.current_time {
                events.push(self.event_queue.pop().unwrap().0);
            } else {
                break;
            }
        }

        // Apply events to signals
        let mut changed_signals = HashSet::new();
        for evt in &events {
            let sig = self.signals.get_mut(evt.signal);
            sig.previous_value = sig.value.clone();

            // Apply the new value
            let mut new_val = sig.value.clone();
            for i in 0..new_val.width().min(evt.value.width()) {
                new_val.set(i, evt.value.get(i));
            }

            if new_val != sig.value {
                sig.value = new_val;
                changed_signals.insert(evt.signal);
            }
        }

        // Record waveform changes
        if self.recorder.is_some() {
            for &sig_id in &changed_signals {
                let sig = self.signals.get(sig_id);
                let value = sig.value.clone();
                let time_fs = self.current_time.fs;
                if let Some(rec) = &mut self.recorder {
                    rec.record_change(time_fs, sig_id, &value)?;
                }
            }
        }

        // Find and execute sensitive processes
        let mut all_pending = Vec::new();
        let processes_to_run = self.find_sensitive_processes(&changed_signals);

        for proc_idx in processes_to_run {
            if self.run_process(proc_idx, ProcessEntry::Body, &mut all_pending)? {
                return Ok(StepResult::Done);
            }
        }

        // Schedule pending updates (merging slice updates to same signal)
        let next_delta = self.current_time.next_delta();
        self.merge_and_schedule(all_pending, next_delta);

        self.total_deltas += 1;

        Ok(StepResult::Continued)
    }

    /// Flattens a module into the kernel's flat signal/process space.
    fn flatten_module(
        &mut self,
        design: &Design,
        module_id: ModuleId,
        prefix: &str,
        parent_signal_map: &mut HashMap<SignalId, SimSignalId>,
        interner: &Interner,
    ) -> Result<HashMap<SignalId, SimSignalId>, SimError> {
        let module = design.modules.get(module_id);
        let mut signal_map = HashMap::new();

        // Allocate signals
        for (sig_id, signal) in module.signals.iter() {
            // If this signal is already mapped by parent (port binding), use that
            if let Some(&existing) = parent_signal_map.get(&sig_id) {
                signal_map.insert(sig_id, existing);
                continue;
            }

            let name = format!("{prefix}.{}", interner.resolve(signal.name));
            let width = self.types.bit_width(signal.ty).unwrap_or(1);

            let init_value = match &signal.init {
                // Fit the declared value to the signal's width.
                Some(ConstValue::Logic(lv)) => apply_write(&LogicVec::new(width), lv, None),
                Some(ConstValue::Int(v)) => LogicVec::from_u64(*v as u64, width),
                Some(ConstValue::Bool(b)) => LogicVec::from_bool(*b),
                _ => match signal.kind {
                    SignalKind::Wire | SignalKind::Port => LogicVec::new(width),
                    SignalKind::Reg | SignalKind::Latch | SignalKind::Variable => {
                        let mut v = LogicVec::new(width);
                        for i in 0..width {
                            v.set(i, Logic::X);
                        }
                        v
                    }
                    SignalKind::Const => LogicVec::new(width),
                },
            };

            let sim_id = self
                .signals
                .alloc(SimSignalState::new(name, width, init_value));
            signal_map.insert(sig_id, sim_id);
            if signal.kind == SignalKind::Variable {
                self.variables.insert(sim_id);
            }
        }

        // Create processes
        for (_, process) in module.processes.iter() {
            self.create_sim_process(process, &signal_map);
        }

        // Create implicit processes for concurrent assignments
        for assignment in &module.assignments {
            let proc_body = Statement::Assign {
                target: assignment.target.clone(),
                value: assignment.value.clone(),
                // Continuous assignments update in the active region, like
                // a blocking assignment in a single-statement process.
                kind: AssignKind::Blocking,
                span: assignment.span,
            };
            let read_sigs = collect_expr_read_signals(&assignment.value, &signal_map);
            let proc = SimProcess {
                _index: self.processes.len(),
                kind: ProcessKind::Combinational,
                signal_map: signal_map.clone(),
                body: proc_body,
                sensitivity: Sensitivity::All,
                read_signals: read_sigs,
                retriggerable: true,
            };
            self.processes.push(proc);
        }

        // Recurse into instances
        for (_, cell) in module.cells.iter() {
            if let CellKind::Instance {
                module: child_mod, ..
            } = &cell.kind
            {
                let child_module = design.modules.get(*child_mod);
                let child_prefix = format!("{prefix}.{}", interner.resolve(cell.name));

                // Build parent-to-child signal binding
                let mut child_port_map = HashMap::new();
                for conn in &cell.connections {
                    // Find the child port with matching name
                    if let Some(child_port) =
                        child_module.ports.iter().find(|p| p.name == conn.port_name)
                    {
                        // Resolve the parent's signal ref to a SimSignalId
                        if let Some(parent_sim_id) =
                            resolve_signal_ref_to_sim_id(&conn.signal, &signal_map)
                        {
                            child_port_map.insert(child_port.signal, parent_sim_id);
                        }
                    }
                }

                self.flatten_module(
                    design,
                    *child_mod,
                    &child_prefix,
                    &mut child_port_map,
                    interner,
                )?;
            }
        }

        Ok(signal_map)
    }

    /// Creates a SimProcess from an IR Process.
    fn create_sim_process(
        &mut self,
        process: &Process,
        signal_map: &HashMap<SignalId, SimSignalId>,
    ) {
        let read_sigs = collect_stmt_read_signals(&process.body, signal_map);
        let proc = SimProcess {
            _index: self.processes.len(),
            kind: process.kind,
            signal_map: signal_map.clone(),
            body: process.body.clone(),
            sensitivity: process.sensitivity.clone(),
            read_signals: read_sigs,
            retriggerable: is_retriggerable(process.kind, &process.body),
        };
        self.processes.push(proc);
    }

    /// Builds the sensitivity map: signal → list of process indices.
    ///
    /// Initial blocks and looping processes are left out (see
    /// [`is_retriggerable`]): they run once at time 0 and then resume only
    /// from their own continuations, whatever their IR sensitivity says.
    fn build_sensitivity_map(&mut self) {
        self.sensitivity_map.clear();
        for (idx, proc) in self.processes.iter().enumerate() {
            if !proc.retriggerable {
                continue;
            }
            match &proc.sensitivity {
                Sensitivity::All => {
                    // Sensitive to all read signals, but not to VHDL
                    // variables: `process(all)` covers signals only, and a
                    // variable written by `:=` must not wake its process.
                    for &sig in &proc.read_signals {
                        if !self.variables.contains(&sig) {
                            self.sensitivity_map.entry(sig).or_default().push(idx);
                        }
                    }
                }
                Sensitivity::EdgeList(edges) => {
                    for edge_sens in edges {
                        if let Some(&sim_id) = proc.signal_map.get(&edge_sens.signal) {
                            self.sensitivity_map.entry(sim_id).or_default().push(idx);
                        }
                    }
                }
                Sensitivity::SignalList(sigs) => {
                    for sig_id in sigs {
                        if let Some(&sim_id) = proc.signal_map.get(sig_id) {
                            self.sensitivity_map.entry(sim_id).or_default().push(idx);
                        }
                    }
                }
            }
        }
    }

    /// Finds processes to execute based on which signals changed.
    ///
    /// A process that is suspended mid-body on a delay is skipped: it is not
    /// waiting on its sensitivity list, so the event is simply missed (as in
    /// IEEE 1800 event scheduling) instead of starting a second copy.
    fn find_sensitive_processes(&self, changed: &HashSet<SimSignalId>) -> Vec<usize> {
        let mut to_run = HashSet::new();
        let suspended: HashSet<usize> = self
            .suspended_processes
            .iter()
            .map(|(_, sp)| sp.process_idx)
            .collect();

        for &sig_id in changed {
            if let Some(procs) = self.sensitivity_map.get(&sig_id) {
                for &proc_idx in procs {
                    if suspended.contains(&proc_idx) {
                        continue;
                    }
                    let proc = &self.processes[proc_idx];

                    // For edge-sensitive processes, check edge conditions
                    if let Sensitivity::EdgeList(edges) = &proc.sensitivity {
                        let should_wake = edges.iter().any(|es| {
                            if let Some(&sim_id) = proc.signal_map.get(&es.signal) {
                                if changed.contains(&sim_id) {
                                    let sig = self.signals.get(sim_id);
                                    check_edge(&sig.previous_value, &sig.value, es.edge)
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        });
                        if should_wake {
                            to_run.insert(proc_idx);
                        }
                    } else {
                        to_run.insert(proc_idx);
                    }
                }
            }
        }

        let mut result: Vec<usize> = to_run.into_iter().collect();
        result.sort_unstable();
        result
    }

    /// Core simulation loop.
    /// Registers all flattened signals with the waveform recorder.
    ///
    /// Groups signals by their hierarchical scope prefix (e.g. `"top"`,
    /// `"top.child"`) and calls `begin_scope`/`register_signal`/`end_scope`
    /// on the recorder to produce the correct scope/variable hierarchy.
    fn register_waveform_signals(&mut self) -> Result<(), SimError> {
        let rec = match &mut self.recorder {
            Some(r) => r,
            None => return Ok(()),
        };

        // Collect signal info to avoid borrow conflict with recorder
        let mut sig_info: Vec<(SimSignalId, String, u32)> = Vec::new();
        for (id, state) in self.signals.iter() {
            sig_info.push((id, state.name.clone(), state.width));
        }

        // Sort by name so scopes group together
        sig_info.sort_by(|a, b| a.1.cmp(&b.1));

        // Track open scopes to emit begin_scope/end_scope correctly
        let mut open_scopes: Vec<String> = Vec::new();

        for (id, full_name, width) in &sig_info {
            // Split "top.child.sig0" into scope=["top","child"] leaf="sig0"
            let parts: Vec<&str> = full_name.split('.').collect();
            let (scope_parts, leaf) = parts.split_at(parts.len() - 1);

            // Close scopes that are no longer common
            let common = open_scopes
                .iter()
                .zip(scope_parts.iter())
                .take_while(|(a, b)| a.as_str() == **b)
                .count();

            // Close scopes from the end back to the common prefix
            while open_scopes.len() > common {
                rec.end_scope()?;
                open_scopes.pop();
            }

            // Open new scopes
            for &scope in &scope_parts[common..] {
                rec.begin_scope(scope)?;
                open_scopes.push(scope.to_string());
            }

            rec.register_signal(*id, leaf[0], *width)?;
        }

        // Close remaining open scopes
        while !open_scopes.is_empty() {
            rec.end_scope()?;
            open_scopes.pop();
        }

        Ok(())
    }

    /// Records the current value of every signal at the current time.
    ///
    /// Called after `register_waveform_signals` so that the waveform file
    /// contains a complete initial value dump (e.g. VCD `$dumpvars`).
    fn record_initial_values(&mut self) -> Result<(), SimError> {
        let rec = match &mut self.recorder {
            Some(r) => r,
            None => return Ok(()),
        };

        let time_fs = self.current_time.fs;
        for (id, state) in self.signals.iter() {
            rec.record_change(time_fs, id, &state.value)?;
        }
        Ok(())
    }

    /// Writes the waveform header (scopes and signal declarations) and the
    /// initial value dump, once per attached recorder.
    fn start_waveform(&mut self) -> Result<(), SimError> {
        if self.waveform_started || self.recorder.is_none() {
            return Ok(());
        }
        self.waveform_started = true;
        self.register_waveform_signals()?;
        self.record_initial_values()
    }

    /// Performs the time-0 start exactly once: starts the waveform, runs all
    /// Initial processes, then runs every combinational process once for
    /// initial propagation (which also starts looping `always` blocks).
    ///
    /// Later calls only start a waveform recorder attached since the last
    /// call, so `run()` twice, or `initialize()` followed by `run()` or
    /// `run_until()`, never re-runs initial blocks or rewrites the header.
    fn ensure_initialized(&mut self) -> Result<(), SimError> {
        self.start_waveform()?;
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        self.execute_initial_processes()?;
        self.execute_combinational_processes()
    }

    /// Runs process `idx` from `entry` until it completes, suspends on a
    /// delay, or calls `$finish`.
    ///
    /// This is the single place where process statements are executed, for
    /// every path: initial blocks and the time-0 combinational pass, sensitivity
    /// triggers in [`step_delta`](Self::step_delta), and delay wakeups. It:
    ///
    /// - evaluates against the current signal values and appends the
    ///   resulting signal updates to `pending` (the caller decides when they
    ///   take effect);
    /// - collects `$display` output and assertion failures;
    /// - on suspension, parks the continuation in `suspended_processes` to
    ///   wake at the current time plus the delay (the process is then not
    ///   re-triggered by its sensitivity list until it resumes and completes);
    /// - on `$finish`, marks the simulation finished.
    ///
    /// Returns `true` if the process called `$finish`.
    fn run_process(
        &mut self,
        idx: usize,
        entry: ProcessEntry,
        pending: &mut Vec<PendingUpdate>,
    ) -> Result<bool, SimError> {
        let mut display = Vec::new();
        let result = {
            let proc = &self.processes[idx];
            let stmt = match &entry {
                ProcessEntry::Body => &proc.body,
                ProcessEntry::Resume(continuation) => continuation,
            };
            let ctx = EvalContext {
                signals: &self.signals,
                signal_map: &proc.signal_map,
                types: &self.types,
            };
            exec_statement(&ctx, stmt, pending, &mut display)?
        };

        for msg in &display {
            if msg.starts_with("ASSERTION FAILED:") {
                self.assertion_failures.push(msg.clone());
            }
        }
        self.display_output.extend(display);

        match result {
            ExecResult::Continue => Ok(false),
            ExecResult::Finish => {
                self.finished = true;
                Ok(true)
            }
            ExecResult::Suspend {
                delay_fs,
                continuation,
            } => {
                let wake_time = SimTime {
                    fs: self.current_time.fs + delay_fs,
                    delta: 0,
                };
                self.suspended_processes.push((
                    wake_time,
                    SuspendedProcess {
                        process_idx: idx,
                        continuation: *continuation,
                    },
                ));
                Ok(false)
            }
        }
    }

    fn run_simulation(&mut self) -> Result<SimResult, SimError> {
        // Phases 0-2 (once): waveform header, Initial processes, and the
        // initial combinational propagation pass.
        self.ensure_initialized()?;

        // Phase 3: Event loop — process events and wakeups
        let mut deltas_at_current_time = 0u32;
        while !self.finished {
            // NBA region: once the active region at this time has drained.
            self.apply_nba_region();

            // Determine next event time: min(event_queue, suspended wakeups)
            let next_event_time = self.event_queue.peek().map(|e| e.0.time.fs);
            let next_wakeup_time = self.next_wakeup_time();
            let next_time_fs = match (next_event_time, next_wakeup_time) {
                (Some(e), Some(w)) => Some(e.min(w)),
                (Some(e), None) => Some(e),
                (None, Some(w)) => Some(w),
                (None, None) => None,
            };

            let next_time_fs = match next_time_fs {
                Some(t) => t,
                None => break, // Nothing left to do
            };

            // Check time limit
            if let Some(limit) = self.time_limit {
                if next_time_fs > limit {
                    break;
                }
            }

            // Reset delta counter on time change
            if next_time_fs != self.current_time.fs {
                deltas_at_current_time = 0;
            }

            // Process suspended wakeups at this time (before events), unless
            // signal events are already due now (see `wakeups_may_run`).
            if self.wakeups_may_run(next_time_fs) {
                self.process_wakeups(next_time_fs)?;
                // Wakeups that only made non-blocking writes leave no active
                // events, so their NBA region is applied in this iteration.
                self.apply_nba_region();
            }

            // Process signal events at this time
            let step = if self
                .event_queue
                .peek()
                .is_some_and(|e| e.0.time.fs <= next_time_fs)
            {
                Some(self.step_delta()?)
            } else {
                None
            };

            // Delta-cycle iterations always count; pure-wakeup iterations
            // count only when they leave more work at this same time (a
            // zero-delay resume), so zero-delay loops trip the limit.
            self.charge_iteration(step.is_some(), &mut deltas_at_current_time)?;

            if step == Some(StepResult::Done) {
                break;
            }
        }

        // Finalize waveform
        if let Some(rec) = &mut self.recorder {
            rec.finalize()?;
        }

        Ok(SimResult {
            final_time: self.current_time,
            finished_by_user: self.finished,
            total_deltas: self.total_deltas,
            display_output: self.display_output.clone(),
            assertion_failures: self.assertion_failures.clone(),
        })
    }

    /// Charges one scheduler iteration against the per-time-step delta budget.
    ///
    /// Called once per iteration of the `run_simulation()` / `run_until()`
    /// loops, after suspended processes were resumed and (if events were due)
    /// `step_delta()` ran. `stepped` says whether `step_delta()` ran.
    ///
    /// - Iterations that ran `step_delta()` are always charged, exactly as
    ///   delta cycles were before.
    /// - Pure-wakeup iterations (no signal events) are charged only when they
    ///   leave more work at the *same* timestamp. That is the signature of a
    ///   zero-delay loop such as `initial forever #0;`: each `#0` resume
    ///   re-queues the process at the same time without producing events, so
    ///   without this charge it would spin forever and never reach the limit.
    ///   A wakeup that finishes, calls `$finish`, or reschedules into the
    ///   future cannot loop, so it is not charged; otherwise a tight limit
    ///   (e.g. `max_delta_per_step == 1`) would reject finite processes.
    ///
    /// The caller resets `deltas_at_current_time` whenever time advances.
    /// Returns [`SimError::DeltaCycleLimit`] once `max_delta_per_step` charged
    /// iterations have run at the current time.
    fn charge_iteration(
        &self,
        stepped: bool,
        deltas_at_current_time: &mut u32,
    ) -> Result<(), SimError> {
        let zero_delay_resume =
            !self.finished && self.next_event_time_fs() == Some(self.current_time.fs);
        if !stepped && !zero_delay_resume {
            return Ok(());
        }
        *deltas_at_current_time += 1;
        if *deltas_at_current_time >= self.max_delta_per_step {
            return Err(SimError::DeltaCycleLimit {
                fs: self.current_time.fs,
                max_deltas: self.max_delta_per_step,
            });
        }
        Ok(())
    }

    /// Returns the earliest wakeup time among suspended processes, if any.
    fn next_wakeup_time(&self) -> Option<u64> {
        self.suspended_processes.iter().map(|(t, _)| t.fs).min()
    }

    /// Processes all suspended processes whose wakeup time has arrived.
    ///
    /// Executes their continuations, applies pending updates, and re-queues
    /// any processes that suspend again.
    fn process_wakeups(&mut self, time_fs: u64) -> Result<(), SimError> {
        // Collect wakeups due at or before this time
        let mut due: Vec<SuspendedProcess> = Vec::new();
        let mut remaining: Vec<(SimTime, SuspendedProcess)> = Vec::new();

        for (wake_time, proc) in self.suspended_processes.drain(..) {
            if wake_time.fs <= time_fs {
                due.push(proc);
            } else {
                remaining.push((wake_time, proc));
            }
        }
        self.suspended_processes = remaining;

        if due.is_empty() {
            return Ok(());
        }

        // Advance time to this wakeup
        self.current_time = SimTime {
            fs: time_fs,
            delta: 0,
        };

        for sp in due {
            let mut pending = Vec::new();
            let finished = self.run_process(
                sp.process_idx,
                ProcessEntry::Resume(sp.continuation),
                &mut pending,
            )?;

            // Schedule updates as events for the next delta cycle so that
            // step_delta() can properly detect the change (comparing previous_value
            // vs new value) and trigger sensitive processes (e.g. posedge clk →
            // sequential counter).  We must NOT apply updates immediately here,
            // because that would make step_delta see no difference and skip the
            // sensitivity check.
            let next_delta = self.current_time.next_delta();
            self.merge_and_schedule(pending, next_delta);

            if finished {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Executes all Initial processes once.
    fn execute_initial_processes(&mut self) -> Result<(), SimError> {
        let initial_indices: Vec<usize> = self
            .processes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.kind == ProcessKind::Initial)
            .map(|(i, _)| i)
            .collect();

        for idx in initial_indices {
            let mut pending = Vec::new();
            let finished = self.run_process(idx, ProcessEntry::Body, &mut pending)?;

            // Apply initial updates immediately
            for update in pending {
                self.apply_update_immediate(&update);
            }

            if finished {
                break;
            }
        }
        Ok(())
    }

    /// Executes all combinational processes once for initial propagation.
    ///
    /// This is also where a looping `always` block (no event control) starts:
    /// it runs up to its first delay and suspends.
    fn execute_combinational_processes(&mut self) -> Result<(), SimError> {
        let comb_indices: Vec<usize> = self
            .processes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.kind == ProcessKind::Combinational)
            .map(|(i, _)| i)
            .collect();

        for idx in comb_indices {
            let mut pending = Vec::new();
            let finished = self.run_process(idx, ProcessEntry::Body, &mut pending)?;

            // Schedule updates at time 0, delta 1 (merging slice updates)
            self.merge_and_schedule(pending, SimTime { fs: 0, delta: 1 });

            if finished {
                break;
            }
        }
        Ok(())
    }

    /// Merges pending updates by target signal and schedules one event per signal.
    ///
    /// Multiple bit-select assignments to the same signal (e.g., `leds[0] = ...; leds[1] = ...;`)
    /// produce separate [`PendingUpdate`] entries. Without merging, each update reads the
    /// current signal value independently, so only the last update's bit survives. This
    /// method accumulates all partial writes into a single merged value per signal.
    ///
    /// Only blocking updates are scheduled at `time`. Non-blocking updates
    /// are moved to the NBA queue instead (see
    /// [`apply_nba_region`](Self::apply_nba_region)); their merge base is the
    /// signal value when the NBA region runs, not the value now.
    fn merge_and_schedule(&mut self, updates: Vec<PendingUpdate>, time: SimTime) {
        let (nonblocking, blocking): (Vec<_>, Vec<_>) = updates
            .into_iter()
            .partition(|u| u.kind == AssignKind::NonBlocking);
        self.nba_queue.extend(nonblocking);
        self.schedule_merged(blocking, time);
    }

    /// Returns `true` if signal events or suspended-process wakeups are due
    /// at or before the current time, i.e. the active (or inactive) region
    /// of the current time step still has work.
    fn has_active_work_now(&self) -> bool {
        let now = self.current_time.fs;
        self.event_queue.peek().is_some_and(|e| e.0.time.fs <= now)
            || self.suspended_processes.iter().any(|(t, _)| t.fs <= now)
    }

    /// Applies the NBA region if it is due.
    ///
    /// Non-blocking updates collected at the current time are applied only
    /// after every active event and zero-delay wakeup at this time has run,
    /// so all processes triggered by the same edge read the old values. The
    /// updates are scheduled as events in the next delta (in execution
    /// order, merging partial writes), so applying them goes through
    /// `step_delta()`: changes trigger sensitive processes and the delta
    /// counts against the per-time-step limit like any other delta cycle.
    ///
    /// Returns `true` if the NBA region was applied.
    fn apply_nba_region(&mut self) -> bool {
        if self.nba_queue.is_empty() || self.has_active_work_now() {
            return false;
        }
        self.flush_nba_queue();
        true
    }

    /// Unconditionally schedules every queued non-blocking update as an event
    /// in the next delta of the current time.
    fn flush_nba_queue(&mut self) {
        if self.nba_queue.is_empty() {
            return;
        }
        let updates = std::mem::take(&mut self.nba_queue);
        let next_delta = self.current_time.next_delta();
        self.schedule_merged(updates, next_delta);
    }

    /// Returns `true` if suspended processes may be resumed at `time_fs` in
    /// this scheduler iteration.
    ///
    /// Signal events already due at `time_fs` (e.g. blocking writes made just
    /// before a `#0`) are processed first, so a zero-delay resume runs in the
    /// inactive region after the active events and sees every committed
    /// blocking write, as IEEE 1364 requires.
    fn wakeups_may_run(&self, time_fs: u64) -> bool {
        !self
            .event_queue
            .peek()
            .is_some_and(|e| e.0.time.fs <= time_fs)
    }

    /// Merges updates per target signal and schedules one event per signal
    /// at `time`, regardless of their [`AssignKind`].
    fn schedule_merged(&mut self, updates: Vec<PendingUpdate>, time: SimTime) {
        // Use a Vec of (SimSignalId, LogicVec) to preserve insertion order
        let mut merged: Vec<(SimSignalId, LogicVec)> = Vec::new();
        let mut index_map: HashMap<SimSignalId, usize> = HashMap::new();

        for update in updates {
            if let Some(&idx) = index_map.get(&update.target) {
                // Merge into existing entry
                let (_, ref mut val) = merged[idx];
                *val = apply_write(val, &update.value, update.range);
            } else {
                // New signal — start from current value and apply update,
                // resized to the signal's width (see `apply_write`).
                let current = &self.signals.get(update.target).value;
                let base = apply_write(current, &update.value, update.range);
                index_map.insert(update.target, merged.len());
                merged.push((update.target, base));
            }
        }

        for (signal, value) in merged {
            self.event_queue.push(Reverse(SimEvent {
                time,
                signal,
                value,
                _strength: DriveStrength::Strong,
            }));
        }
    }

    /// Applies an update immediately (for initial blocks).
    ///
    /// Non-blocking updates are not applied here: they go to the NBA queue
    /// and take effect in the NBA region of time 0.
    fn apply_update_immediate(&mut self, update: &PendingUpdate) {
        if update.kind == AssignKind::NonBlocking {
            self.nba_queue.push(update.clone());
            return;
        }
        // Resize to the signal's width: `reg [7:0] c; initial c = 0;` must
        // keep `c` 8 bits wide, not adopt the literal's width.
        let sig = self.signals.get_mut(update.target);
        sig.value = apply_write(&sig.value, &update.value, update.range);
    }

    /// Returns all signal names and their flat IDs.
    ///
    /// Useful for listing all available signals in interactive mode.
    pub fn all_signals(&self) -> Vec<(SimSignalId, &str, u32)> {
        self.signals
            .iter()
            .map(|(id, s)| (id, s.name.as_str(), s.width))
            .collect()
    }

    /// Returns whether the simulation has pending events in the queue, suspended
    /// processes, or non-blocking updates waiting for the NBA region.
    pub fn has_pending_events(&self) -> bool {
        !self.event_queue.is_empty()
            || !self.suspended_processes.is_empty()
            || !self.nba_queue.is_empty()
    }

    /// Returns whether the simulation has been terminated by `$finish`.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Performs the time-0 start: writes the waveform header (if a recorder
    /// is attached), runs all `initial` blocks, and runs every combinational
    /// process once for initial propagation.
    ///
    /// Idempotent: the start happens at most once per kernel, and
    /// [`run`](Self::run), [`run_to_completion`](Self::run_to_completion) and
    /// [`run_until`](Self::run_until) perform it implicitly if it has not
    /// happened yet. Interactive front ends call it explicitly to observe the
    /// time-0 state before stepping.
    pub fn initialize(&mut self) -> Result<(), SimError> {
        self.ensure_initialized()
    }

    /// Returns collected `$display` output and clears the buffer.
    pub fn take_display_output(&mut self) -> Vec<String> {
        std::mem::take(&mut self.display_output)
    }

    /// Returns collected assertion failures and clears the buffer.
    pub fn take_assertion_failures(&mut self) -> Vec<String> {
        std::mem::take(&mut self.assertion_failures)
    }

    /// Runs the simulation until the given target time in femtoseconds.
    ///
    /// Unlike `step_delta()`, this method processes both queued events and
    /// suspended process wakeups (delays), mirroring the main `run_simulation()`
    /// loop. Use this for advancing simulation time in the TUI or interactive
    /// mode where delay-based scheduling must be honored.
    ///
    /// Returns [`SimError::DeltaCycleLimit`] if more than the configured
    /// maximum number of scheduler iterations (delta cycles or zero-delay
    /// wakeups) occur at a single timestamp, so a zero-delay loop such as
    /// `initial forever #0;` terminates instead of hanging.
    ///
    /// Performs the time-0 start first if [`initialize`](Self::initialize)
    /// has not been called.
    pub fn run_until(&mut self, target_fs: u64) -> Result<StepResult, SimError> {
        self.ensure_initialized()?;
        // The target bounds `step_delta()` for this call only. Without it, a
        // stale limit from an earlier `run()` would make `step_delta()` refuse
        // events this loop has already chosen to process. The caller's limit is
        // restored on every return path so later `step_delta()` /
        // `run_to_completion()` calls are not cut off at this target.
        let saved_limit = self.time_limit.replace(target_fs);
        let result = self.run_until_inner(target_fs);
        self.time_limit = saved_limit;
        result
    }

    /// The body of [`run_until`](Self::run_until), run with `time_limit`
    /// temporarily set to `target_fs`.
    fn run_until_inner(&mut self, target_fs: u64) -> Result<StepResult, SimError> {
        let mut deltas_at_current_time = 0u32;

        while !self.finished {
            // NBA region: once the active region at this time has drained.
            self.apply_nba_region();

            let next_event_time = self.event_queue.peek().map(|e| e.0.time.fs);
            let next_wakeup_time = self.next_wakeup_time();
            let next_time_fs = match (next_event_time, next_wakeup_time) {
                (Some(e), Some(w)) => Some(e.min(w)),
                (Some(e), None) => Some(e),
                (None, Some(w)) => Some(w),
                (None, None) => None,
            };

            let next_time_fs = match next_time_fs {
                Some(t) => t,
                None => return Ok(StepResult::Done),
            };

            if next_time_fs > target_fs {
                // Nothing left to do before target time — advance time
                self.current_time = SimTime {
                    fs: target_fs,
                    delta: 0,
                };
                return Ok(StepResult::Continued);
            }

            // Reset delta counter on time change
            if next_time_fs != self.current_time.fs {
                deltas_at_current_time = 0;
            }

            // Process suspended wakeups at this time (before events), unless
            // signal events are already due now (see `wakeups_may_run`).
            if self.wakeups_may_run(next_time_fs) {
                self.process_wakeups(next_time_fs)?;
                // Wakeups that only made non-blocking writes leave no active
                // events, so their NBA region is applied in this iteration.
                self.apply_nba_region();
            }

            // Process signal events at this time
            let step = if self
                .event_queue
                .peek()
                .is_some_and(|e| e.0.time.fs <= next_time_fs)
            {
                Some(self.step_delta()?)
            } else {
                None
            };

            // Delta-cycle iterations always count; pure-wakeup iterations
            // count only when they leave more work at this same time (a
            // zero-delay resume), so zero-delay loops trip the limit.
            self.charge_iteration(step.is_some(), &mut deltas_at_current_time)?;

            if step == Some(StepResult::Done) && !self.has_pending_events() {
                return Ok(StepResult::Done);
            }
        }

        Ok(StepResult::Done)
    }

    /// Returns the earliest time at which a pending event or wakeup is scheduled.
    ///
    /// Waiting non-blocking updates count as work at the current time.
    /// Returns `None` if no events, wakeups or non-blocking updates are
    /// pending.
    pub fn next_event_time_fs(&self) -> Option<u64> {
        if !self.nba_queue.is_empty() {
            return Some(self.current_time.fs);
        }
        let next_event = self.event_queue.peek().map(|e| e.0.time.fs);
        let next_wakeup = self.next_wakeup_time();
        match (next_event, next_wakeup) {
            (Some(e), Some(w)) => Some(e.min(w)),
            (Some(e), None) => Some(e),
            (None, Some(w)) => Some(w),
            (None, None) => None,
        }
    }

    /// Schedules an event at a future time.
    pub fn schedule_event(&mut self, time: SimTime, signal: SimSignalId, value: LogicVec) {
        self.event_queue.push(Reverse(SimEvent {
            time,
            signal,
            value,
            _strength: DriveStrength::Strong,
        }));
    }
}

/// Checks if a signal has experienced the specified edge.
fn check_edge(prev: &LogicVec, curr: &LogicVec, edge: Edge) -> bool {
    if prev.width() == 0 || curr.width() == 0 {
        return false;
    }
    let prev_bit = prev.get(0);
    let curr_bit = curr.get(0);
    match edge {
        Edge::Posedge => prev_bit == Logic::Zero && curr_bit == Logic::One,
        Edge::Negedge => prev_bit == Logic::One && curr_bit == Logic::Zero,
        Edge::Both => {
            (prev_bit == Logic::Zero && curr_bit == Logic::One)
                || (prev_bit == Logic::One && curr_bit == Logic::Zero)
        }
    }
}

/// Resolves a SignalRef to a single SimSignalId (only for simple Signal refs).
fn resolve_signal_ref_to_sim_id(
    signal_ref: &SignalRef,
    signal_map: &HashMap<SignalId, SimSignalId>,
) -> Option<SimSignalId> {
    match signal_ref {
        SignalRef::Signal(sig_id) => signal_map.get(sig_id).copied(),
        SignalRef::Slice { signal, .. } => signal_map.get(signal).copied(),
        _ => None,
    }
}

/// Collects all SimSignalIds read by an expression.
fn collect_expr_read_signals(
    expr: &Expr,
    signal_map: &HashMap<SignalId, SimSignalId>,
) -> HashSet<SimSignalId> {
    let mut result = HashSet::new();
    collect_expr_reads_inner(expr, signal_map, &mut result);
    result
}

/// Recursively collects signal reads from expressions.
fn collect_expr_reads_inner(
    expr: &Expr,
    signal_map: &HashMap<SignalId, SimSignalId>,
    result: &mut HashSet<SimSignalId>,
) {
    match expr {
        Expr::Signal(sr) => collect_signal_ref_reads(sr, signal_map, result),
        Expr::Literal(_) => {}
        Expr::Unary { operand, .. } => collect_expr_reads_inner(operand, signal_map, result),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_reads_inner(lhs, signal_map, result);
            collect_expr_reads_inner(rhs, signal_map, result);
        }
        Expr::Ternary {
            condition,
            true_val,
            false_val,
            ..
        } => {
            collect_expr_reads_inner(condition, signal_map, result);
            collect_expr_reads_inner(true_val, signal_map, result);
            collect_expr_reads_inner(false_val, signal_map, result);
        }
        Expr::FuncCall { args, .. } => {
            for arg in args {
                collect_expr_reads_inner(arg, signal_map, result);
            }
        }
        Expr::Concat(parts) => {
            for p in parts {
                collect_expr_reads_inner(p, signal_map, result);
            }
        }
        Expr::Repeat { expr, .. } => collect_expr_reads_inner(expr, signal_map, result),
        Expr::Index { expr, index, .. } => {
            collect_expr_reads_inner(expr, signal_map, result);
            collect_expr_reads_inner(index, signal_map, result);
        }
        Expr::Slice {
            expr, high, low, ..
        } => {
            collect_expr_reads_inner(expr, signal_map, result);
            collect_expr_reads_inner(high, signal_map, result);
            collect_expr_reads_inner(low, signal_map, result);
        }
    }
}

/// Collects signal reads from a SignalRef.
fn collect_signal_ref_reads(
    sr: &SignalRef,
    signal_map: &HashMap<SignalId, SimSignalId>,
    result: &mut HashSet<SimSignalId>,
) {
    match sr {
        SignalRef::Signal(id) => {
            if let Some(&sim_id) = signal_map.get(id) {
                result.insert(sim_id);
            }
        }
        SignalRef::Slice { signal, .. } => {
            if let Some(&sim_id) = signal_map.get(signal) {
                result.insert(sim_id);
            }
        }
        SignalRef::Concat(refs) => {
            for r in refs {
                collect_signal_ref_reads(r, signal_map, result);
            }
        }
        SignalRef::Const(_) => {}
    }
}

/// Collects all SimSignalIds read by a statement tree.
fn collect_stmt_read_signals(
    stmt: &Statement,
    signal_map: &HashMap<SignalId, SimSignalId>,
) -> HashSet<SimSignalId> {
    let mut result = HashSet::new();
    collect_stmt_reads_inner(stmt, signal_map, &mut result);
    result
}

/// Recursively collects signal reads from statements.
fn collect_stmt_reads_inner(
    stmt: &Statement,
    signal_map: &HashMap<SignalId, SimSignalId>,
    result: &mut HashSet<SimSignalId>,
) {
    match stmt {
        Statement::Assign { value, .. } => {
            collect_expr_reads_inner(value, signal_map, result);
        }
        Statement::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            collect_expr_reads_inner(condition, signal_map, result);
            collect_stmt_reads_inner(then_body, signal_map, result);
            if let Some(eb) = else_body {
                collect_stmt_reads_inner(eb, signal_map, result);
            }
        }
        Statement::Case {
            subject,
            arms,
            default,
            ..
        } => {
            collect_expr_reads_inner(subject, signal_map, result);
            for arm in arms {
                for pat in &arm.patterns {
                    collect_expr_reads_inner(pat, signal_map, result);
                }
                collect_stmt_reads_inner(&arm.body, signal_map, result);
            }
            if let Some(def) = default {
                collect_stmt_reads_inner(def, signal_map, result);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                collect_stmt_reads_inner(s, signal_map, result);
            }
        }
        Statement::Assertion { condition, .. } => {
            collect_expr_reads_inner(condition, signal_map, result);
        }
        Statement::Display { args, .. } => {
            for arg in args {
                collect_expr_reads_inner(arg, signal_map, result);
            }
        }
        Statement::Wait { duration, .. } => {
            if let Some(dur) = duration {
                collect_expr_reads_inner(dur, signal_map, result);
            }
        }
        Statement::Delay { body, .. } | Statement::Forever { body, .. } => {
            collect_stmt_reads_inner(body, signal_map, result);
        }
        Statement::Finish { .. } | Statement::Nop => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{ContentHash, Ident, Interner};
    use aion_ir::{
        Arena, Assignment, BinaryOp, EdgeSensitivity, Module, Port, PortDirection, Signal, Type,
    };
    use aion_source::Span;

    fn make_type_db() -> TypeDb {
        let mut types = TypeDb::new();
        // Type 0: 1-bit
        types.intern(Type::Bit);
        // Type 1: 8-bit
        types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        // Type 2: 4-bit
        types.intern(Type::BitVec {
            width: 4,
            signed: false,
        });
        types
    }

    /// Creates a test interner pre-populated with names matching `Ident::from_raw()` indices.
    ///
    /// Indices: 0="__dummy__", 1="top", 2="clk", 3="out", 4="sel",
    /// 5="rst", 6="count", 7="q", 8="a", 9="b", 10="child",
    /// 11="in_sig", 12="in_port", 13="in_port", 14="out_port"
    fn make_test_interner() -> Interner {
        let interner = Interner::new();
        // Index 0
        interner.get_or_intern("__dummy__");
        // Index 1
        interner.get_or_intern("top");
        // Index 2
        interner.get_or_intern("clk");
        // Index 3
        interner.get_or_intern("out");
        // Index 4
        interner.get_or_intern("sel");
        // Index 5
        interner.get_or_intern("rst");
        // Index 6
        interner.get_or_intern("count");
        // Index 7
        interner.get_or_intern("q");
        // Index 8
        interner.get_or_intern("a");
        // Index 9
        interner.get_or_intern("b");
        // Index 10
        interner.get_or_intern("child");
        // Index 11
        interner.get_or_intern("in_sig");
        // Index 12
        interner.get_or_intern("in_port_name");
        interner
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

        // Signal 0: clk (1-bit wire)
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Signal 1: out (1-bit wire)
        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Combinational assignment: out = clk
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
            source_map: aion_ir::SourceMap::new(),
        }
    }

    #[test]
    fn kernel_construction() {
        let design = make_simple_design();
        let kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        assert!(kernel.signal_count() >= 2);
        assert!(kernel.process_count() >= 1); // implicit comb process
    }

    #[test]
    fn kernel_find_signal() {
        let design = make_simple_design();
        let kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let sig = kernel.find_signal("top.clk");
        assert!(sig.is_some());
    }

    #[test]
    fn kernel_current_time_starts_zero() {
        let design = make_simple_design();
        let kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        assert_eq!(kernel.current_time(), SimTime::zero());
    }

    #[test]
    fn check_edge_posedge() {
        let prev = LogicVec::from_bool(false);
        let curr = LogicVec::from_bool(true);
        assert!(check_edge(&prev, &curr, Edge::Posedge));
        assert!(!check_edge(&prev, &curr, Edge::Negedge));
        assert!(check_edge(&prev, &curr, Edge::Both));
    }

    #[test]
    fn check_edge_negedge() {
        let prev = LogicVec::from_bool(true);
        let curr = LogicVec::from_bool(false);
        assert!(!check_edge(&prev, &curr, Edge::Posedge));
        assert!(check_edge(&prev, &curr, Edge::Negedge));
        assert!(check_edge(&prev, &curr, Edge::Both));
    }

    #[test]
    fn check_edge_no_change() {
        let prev = LogicVec::from_bool(true);
        let curr = LogicVec::from_bool(true);
        assert!(!check_edge(&prev, &curr, Edge::Posedge));
        assert!(!check_edge(&prev, &curr, Edge::Negedge));
        assert!(!check_edge(&prev, &curr, Edge::Both));
    }

    #[test]
    fn combinational_propagation() {
        let design = make_simple_design();
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();

        // Set clk = 1
        let clk_id = kernel.find_signal("top.clk").unwrap();
        kernel.schedule_event(SimTime::from_ns(1), clk_id, LogicVec::from_bool(true));

        let _result = kernel.run(10 * crate::time::FS_PER_NS).unwrap();
        let out_id = kernel.find_signal("top.out").unwrap();
        // After propagation, out should follow clk
        assert_eq!(kernel.signal_value(out_id).to_u64(), Some(1));
    }

    #[test]
    fn initial_process_execution() {
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

        // Initial process: set signal to 1
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: Expr::Literal(LogicVec::from_bool(true)),
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let _result = kernel.run_to_completion().unwrap();
        let sig = kernel.find_signal("top.clk").unwrap();
        assert_eq!(kernel.signal_value(sig).to_u64(), Some(1));
    }

    #[test]
    fn finish_stops_simulation() {
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

        // Initial process with $finish
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Finish { span: Span::DUMMY },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let result = kernel.run_to_completion().unwrap();
        assert!(result.finished_by_user);
    }

    #[test]
    fn display_output_collected() {
        let types = make_type_db();
        let bit8_ty = aion_ir::TypeId::from_raw(1);

        let mut top = empty_module(0, Ident::from_raw(1));
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit8_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(42)),
            clock_domain: None,
            span: Span::DUMMY,
        });

        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Display {
                        format: "value = %d".into(),
                        args: vec![Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))],
                        span: Span::DUMMY,
                    },
                    Statement::Finish { span: Span::DUMMY },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let result = kernel.run_to_completion().unwrap();
        assert_eq!(result.display_output.len(), 1);
        assert_eq!(result.display_output[0], "value = 42");
    }

    #[test]
    fn sequential_process_edge_detection() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        // Signal 0: clk
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        // Signal 1: q (reg)
        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Reg,
            init: Some(ConstValue::Int(0)),
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Sequential process: on posedge clk, q <= 1
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(1)),
                value: Expr::Literal(LogicVec::from_bool(true)),
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: SignalId::from_raw(0),
                edge: Edge::Posedge,
            }]),
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();

        // Schedule posedge on clk
        let clk_id = kernel.find_signal("top.clk").unwrap();
        kernel.schedule_event(SimTime::from_ns(10), clk_id, LogicVec::from_bool(true));

        let _result = kernel.run(20 * crate::time::FS_PER_NS).unwrap();
        let q_id = kernel.find_signal("top.out").unwrap();
        assert_eq!(kernel.signal_value(q_id).to_u64(), Some(1));
    }

    #[test]
    fn empty_design_errors() {
        let types = TypeDb::new();
        let design = Design {
            modules: Arena::new(),
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };
        assert!(matches!(
            SimKernel::new(&design, &make_test_interner()),
            Err(SimError::NoTopModule)
        ));
    }

    #[test]
    fn schedule_event_works() {
        let design = make_simple_design();
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let sig = kernel.find_signal("top.clk").unwrap();
        kernel.schedule_event(SimTime::from_ns(5), sig, LogicVec::from_bool(true));
        // Event queue should have at least 1 event
        assert!(!kernel.event_queue.is_empty());
    }

    #[test]
    fn collect_expr_reads() {
        let mut map = HashMap::new();
        map.insert(SignalId::from_raw(0), SimSignalId::from_raw(10));
        map.insert(SignalId::from_raw(1), SimSignalId::from_raw(11));

        let expr = Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
            rhs: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(1)))),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        };

        let reads = collect_expr_read_signals(&expr, &map);
        assert!(reads.contains(&SimSignalId::from_raw(10)));
        assert!(reads.contains(&SimSignalId::from_raw(11)));
    }

    #[test]
    fn hierarchy_flattening() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        // Child module with one signal
        let mut child = empty_module(1, Ident::from_raw(10));
        child.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(11),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        child.ports.push(Port {
            id: aion_ir::PortId::from_raw(0),
            name: Ident::from_raw(12),
            direction: PortDirection::Input,
            ty: bit_ty,
            signal: SignalId::from_raw(0),
            span: Span::DUMMY,
        });

        // Parent module
        let mut parent = empty_module(0, Ident::from_raw(1));
        parent.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Instantiate child
        parent.cells.alloc(aion_ir::Cell {
            id: aion_ir::CellId::from_raw(0),
            name: Ident::from_raw(3),
            kind: CellKind::Instance {
                module: ModuleId::from_raw(1),
                params: Vec::new(),
            },
            connections: vec![aion_ir::Connection {
                port_name: Ident::from_raw(12),
                direction: PortDirection::Input,
                signal: SignalRef::Signal(SignalId::from_raw(0)),
            }],
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(parent);
        modules.alloc(child);

        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        // Parent signal + child's port signal should share the same SimSignalId
        assert!(kernel.signal_count() >= 1);
    }

    #[test]
    fn time_limit_stops_run() {
        let design = make_simple_design();
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();

        // Schedule events far in the future
        let sig = kernel.find_signal("top.clk").unwrap();
        kernel.schedule_event(SimTime::from_ns(100), sig, LogicVec::from_bool(true));
        kernel.schedule_event(SimTime::from_ns(200), sig, LogicVec::from_bool(false));

        // Run only 150 ns
        let result = kernel.run(150 * crate::time::FS_PER_NS).unwrap();
        // Should have processed the 100ns event but not the 200ns one
        assert!(result.final_time.fs <= 150 * crate::time::FS_PER_NS);
    }

    #[test]
    fn sim_result_fields() {
        let design = make_simple_design();
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let result = kernel.run_to_completion().unwrap();
        assert!(!result.finished_by_user);
        assert!(result.display_output.is_empty());
        assert!(result.assertion_failures.is_empty());
    }

    #[test]
    fn step_delta_done_on_empty() {
        let design = make_simple_design();
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        // After initial propagation, run to settle
        let _ = kernel.run_to_completion().unwrap();
        // Now step should return Done
        let result = kernel.step_delta().unwrap();
        assert_eq!(result, StepResult::Done);
    }

    #[test]
    fn set_max_delta() {
        let design = make_simple_design();
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(5000);
        assert_eq!(kernel.max_delta_per_step, 5000);
    }

    #[test]
    fn reg_signal_initializes_to_x() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Reg,
            init: None, // No init → X
            clock_domain: None,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let sig = kernel.find_signal("top.clk").unwrap();
        assert_eq!(kernel.signal_value(sig).get(0), Logic::X);
    }

    #[test]
    fn wire_signal_initializes_to_zero() {
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

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let sig = kernel.find_signal("top.clk").unwrap();
        assert_eq!(kernel.signal_value(sig).get(0), Logic::Zero);
    }

    // ---- Delay scheduling tests ----

    #[test]
    fn initial_delay_resumes() {
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

        // Initial: #20 sig = 1; $finish;
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Delay {
                        duration_fs: 20_000_000, // 20 ns
                        body: Box::new(Statement::Assign {
                            target: SignalRef::Signal(SignalId::from_raw(0)),
                            value: Expr::Literal(LogicVec::from_bool(true)),
                            kind: AssignKind::Blocking,
                            span: Span::DUMMY,
                        }),
                        span: Span::DUMMY,
                    },
                    Statement::Finish { span: Span::DUMMY },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let result = kernel.run(100 * crate::time::FS_PER_NS).unwrap();
        // Should finish at 20ns
        assert!(result.finished_by_user);
        assert!(result.final_time.fs >= 20_000_000);
        let sig = kernel.find_signal("top.clk").unwrap();
        assert_eq!(kernel.signal_value(sig).to_u64(), Some(1));
    }

    #[test]
    fn forever_generates_clock() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        // Signal 0: clk
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(0)),
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Initial: clk = 0; forever #5 clk = ~clk;
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(0)),
                        value: Expr::Literal(LogicVec::from_bool(false)),
                        kind: AssignKind::Blocking,
                        span: Span::DUMMY,
                    },
                    Statement::Forever {
                        body: Box::new(Statement::Delay {
                            duration_fs: 5_000_000, // 5 ns
                            body: Box::new(Statement::Assign {
                                target: SignalRef::Signal(SignalId::from_raw(0)),
                                value: Expr::Unary {
                                    op: aion_ir::UnaryOp::Not,
                                    operand: Box::new(Expr::Signal(SignalRef::Signal(
                                        SignalId::from_raw(0),
                                    ))),
                                    ty: bit_ty,
                                    span: Span::DUMMY,
                                },
                                kind: AssignKind::Blocking,
                                span: Span::DUMMY,
                            }),
                            span: Span::DUMMY,
                        }),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let result = kernel.run(50 * crate::time::FS_PER_NS).unwrap();
        // Should have run to 50ns
        assert!(!result.finished_by_user);
        // The clock should have toggled multiple times
        // At 50ns, we've had toggles at 5, 10, 15, 20, 25, 30, 35, 40, 45, 50 ns
        // Starting from 0, after 10 toggles the clock should be back to 0
        let clk_id = kernel.find_signal("top.clk").unwrap();
        // Just verify the simulation ran past 0 fs
        assert!(result.final_time.fs > 0);
        // Value alternates — exact value depends on timing
        let clk_val = kernel.signal_value(clk_id).to_u64();
        assert!(clk_val == Some(0) || clk_val == Some(1));
    }

    /// Builds `top` with one Initial process `body` and a 1-bit `clk` signal
    /// initialised to 0.
    fn make_initial_design(body: Statement) -> Design {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);
        let mut top = empty_module(0, Ident::from_raw(1));
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(0)),
            clock_domain: None,
            span: Span::DUMMY,
        });
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body,
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let mut modules = Arena::new();
        modules.alloc(top);
        Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        }
    }

    /// `forever #0;` — a zero-delay loop with an empty body.
    fn forever_zero_delay() -> Statement {
        Statement::Forever {
            body: Box::new(Statement::Delay {
                duration_fs: 0,
                body: Box::new(Statement::Nop),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        }
    }

    /// `forever #5 clk = ~clk;` — a legitimate free-running clock.
    fn forever_clock_5ns() -> Statement {
        let bit_ty = aion_ir::TypeId::from_raw(0);
        Statement::Forever {
            body: Box::new(Statement::Delay {
                duration_fs: 5_000_000,
                body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Unary {
                        op: aion_ir::UnaryOp::Not,
                        operand: Box::new(Expr::Signal(SignalRef::Signal(SignalId::from_raw(0)))),
                        ty: bit_ty,
                        span: Span::DUMMY,
                    },
                    kind: AssignKind::Blocking,
                    span: Span::DUMMY,
                }),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        }
    }

    // Regression for #5 (SIM-4): `initial forever #0;` used to hang run().
    #[test]
    fn forever_zero_delay_run_hits_delta_limit() {
        let design = make_initial_design(forever_zero_delay());
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let err = kernel.run(100 * crate::time::FS_PER_NS).unwrap_err();
        assert!(
            matches!(
                err,
                SimError::DeltaCycleLimit {
                    fs: 0,
                    max_deltas: 10_000
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    // Regression for #5 (SIM-4): `initial forever #0;` used to hang run_until().
    #[test]
    fn forever_zero_delay_run_until_hits_delta_limit() {
        let design = make_initial_design(forever_zero_delay());
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.initialize().unwrap();
        let err = kernel.run_until(100 * crate::time::FS_PER_NS).unwrap_err();
        assert!(
            matches!(
                err,
                SimError::DeltaCycleLimit {
                    fs: 0,
                    max_deltas: 10_000
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn zero_delay_loop_after_delay_reports_its_time() {
        // initial begin #10; forever #0; end — the loop starts at 10 ns.
        let design = make_initial_design(Statement::Delay {
            duration_fs: 10 * crate::time::FS_PER_NS,
            body: Box::new(forever_zero_delay()),
            span: Span::DUMMY,
        });
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(50);
        let err = kernel.run(100 * crate::time::FS_PER_NS).unwrap_err();
        assert!(
            matches!(
                err,
                SimError::DeltaCycleLimit {
                    fs: 10_000_000,
                    max_deltas: 50
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn nonzero_delay_forever_runs_under_tight_delta_limit() {
        // A tight limit proves the per-time counter resets as time advances,
        // so a legitimate `forever #5` loop never accumulates toward it.
        let design = make_initial_design(forever_clock_5ns());
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(2);
        let result = kernel.run(52 * crate::time::FS_PER_NS).unwrap();
        assert!(!result.finished_by_user);
        assert_eq!(result.final_time.fs, 50 * crate::time::FS_PER_NS);
        // 10 toggles (5..=50 ns) from 0 leaves clk at 0.
        let clk = kernel.find_signal("top.clk").unwrap();
        assert_eq!(kernel.signal_value(clk).to_u64(), Some(0));
    }

    #[test]
    fn nonzero_delay_forever_run_until_under_tight_delta_limit() {
        let design = make_initial_design(forever_clock_5ns());
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(2);
        kernel.initialize().unwrap();
        let clk = kernel.find_signal("top.clk").unwrap();

        // 5 ns, 10 ns, 15 ns → three toggles → clk = 1.
        let r = kernel.run_until(17 * crate::time::FS_PER_NS).unwrap();
        assert_eq!(r, StepResult::Continued);
        assert_eq!(kernel.current_time().fs, 17 * crate::time::FS_PER_NS);
        assert_eq!(kernel.signal_value(clk).to_u64(), Some(1));

        // Resuming continues normally: 20 ns, 25 ns, 30 ns → clk = 0.
        let r = kernel.run_until(32 * crate::time::FS_PER_NS).unwrap();
        assert_eq!(r, StepResult::Continued);
        assert_eq!(kernel.signal_value(clk).to_u64(), Some(0));
    }

    // PR #47 review: a finite pure-wakeup process must not trip a tight limit.
    #[test]
    fn terminal_zero_delay_wakeup_not_charged() {
        // initial #0;  — one zero-delay resume, then nothing left to do.
        let body = Statement::Delay {
            duration_fs: 0,
            body: Box::new(Statement::Nop),
            span: Span::DUMMY,
        };

        let design = make_initial_design(body.clone());
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(1);
        let result = kernel.run(100 * crate::time::FS_PER_NS).unwrap();
        assert_eq!(result.final_time.fs, 0);

        let design = make_initial_design(body);
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(1);
        kernel.initialize().unwrap();
        assert_eq!(
            kernel.run_until(100 * crate::time::FS_PER_NS).unwrap(),
            StepResult::Done
        );
    }

    #[test]
    fn future_rescheduling_wakeup_not_charged() {
        // initial forever #5;  — pure wakeups that always move time forward.
        let body = Statement::Forever {
            body: Box::new(Statement::Delay {
                duration_fs: 5 * crate::time::FS_PER_NS,
                body: Box::new(Statement::Nop),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };

        let design = make_initial_design(body.clone());
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(1);
        let result = kernel.run(50 * crate::time::FS_PER_NS).unwrap();
        assert_eq!(result.final_time.fs, 50 * crate::time::FS_PER_NS);

        let design = make_initial_design(body);
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(1);
        kernel.initialize().unwrap();
        assert_eq!(
            kernel.run_until(52 * crate::time::FS_PER_NS).unwrap(),
            StepResult::Continued
        );
    }

    #[test]
    fn zero_delay_loop_trips_limit_of_one() {
        // The tightest limit still catches the runaway loop immediately.
        let design = make_initial_design(forever_zero_delay());
        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        kernel.set_max_delta(1);
        let err = kernel.run(100 * crate::time::FS_PER_NS).unwrap_err();
        assert!(
            matches!(
                err,
                SimError::DeltaCycleLimit {
                    fs: 0,
                    max_deltas: 1
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn forever_without_delay_errors_instead_of_running_once() {
        // initial begin clk = 0; forever clk = ~clk; $display("unreachable"); end
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);
        let mut top = empty_module(0, Ident::from_raw(1));
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(0)),
            clock_domain: None,
            span: Span::DUMMY,
        });
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Forever {
                        body: Box::new(Statement::Assign {
                            target: SignalRef::Signal(SignalId::from_raw(0)),
                            value: Expr::Unary {
                                op: aion_ir::UnaryOp::Not,
                                operand: Box::new(Expr::Signal(SignalRef::Signal(
                                    SignalId::from_raw(0),
                                ))),
                                ty: bit_ty,
                                span: Span::DUMMY,
                            },
                            kind: AssignKind::Blocking,
                            span: Span::DUMMY,
                        }),
                        span: Span::DUMMY,
                    },
                    Statement::Display {
                        format: "\"unreachable\"".into(),
                        args: Vec::new(),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });
        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let err = kernel.run(100 * crate::time::FS_PER_NS).unwrap_err();
        assert!(matches!(err, SimError::ZeroTimeLoop), "got {err:?}");
        // The statement after the loop must not have run.
        assert!(kernel.take_display_output().is_empty());
    }

    #[test]
    fn multiple_initial_delays() {
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        // Signal 0: rst
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Initial: rst = 0; #20 rst = 1; #100 $finish;
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(0)),
                        value: Expr::Literal(LogicVec::from_bool(false)),
                        kind: AssignKind::Blocking,
                        span: Span::DUMMY,
                    },
                    Statement::Delay {
                        duration_fs: 20_000_000, // 20 ns
                        body: Box::new(Statement::Assign {
                            target: SignalRef::Signal(SignalId::from_raw(0)),
                            value: Expr::Literal(LogicVec::from_bool(true)),
                            kind: AssignKind::Blocking,
                            span: Span::DUMMY,
                        }),
                        span: Span::DUMMY,
                    },
                    Statement::Delay {
                        duration_fs: 100_000_000, // 100 ns
                        body: Box::new(Statement::Finish { span: Span::DUMMY }),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let result = kernel.run(200 * crate::time::FS_PER_NS).unwrap();
        // Should finish at 120 ns (20 + 100)
        assert!(result.finished_by_user);
        assert!(result.final_time.fs >= 120_000_000);
        let rst = kernel.find_signal("top.clk").unwrap();
        assert_eq!(kernel.signal_value(rst).to_u64(), Some(1));
    }

    #[test]
    fn full_testbench_pattern() {
        // Two initial blocks: clock gen + stimulus
        let types = make_type_db();
        let bit_ty = aion_ir::TypeId::from_raw(0);

        let mut top = empty_module(0, Ident::from_raw(1));
        // Signal 0: clk
        top.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(2),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(0)),
            clock_domain: None,
            span: Span::DUMMY,
        });
        // Signal 1: rst_n
        top.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(3),
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: Some(ConstValue::Int(0)),
            clock_domain: None,
            span: Span::DUMMY,
        });

        // Initial 0: clk = 0; forever #5 clk = ~clk;
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(0)),
                        value: Expr::Literal(LogicVec::from_bool(false)),
                        kind: AssignKind::Blocking,
                        span: Span::DUMMY,
                    },
                    Statement::Forever {
                        body: Box::new(Statement::Delay {
                            duration_fs: 5_000_000,
                            body: Box::new(Statement::Assign {
                                target: SignalRef::Signal(SignalId::from_raw(0)),
                                value: Expr::Unary {
                                    op: aion_ir::UnaryOp::Not,
                                    operand: Box::new(Expr::Signal(SignalRef::Signal(
                                        SignalId::from_raw(0),
                                    ))),
                                    ty: bit_ty,
                                    span: Span::DUMMY,
                                },
                                kind: AssignKind::Blocking,
                                span: Span::DUMMY,
                            }),
                            span: Span::DUMMY,
                        }),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        // Initial 1: rst_n = 0; #20 rst_n = 1; #100 $finish;
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(1),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Block {
                stmts: vec![
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(1)),
                        value: Expr::Literal(LogicVec::from_bool(false)),
                        kind: AssignKind::Blocking,
                        span: Span::DUMMY,
                    },
                    Statement::Delay {
                        duration_fs: 20_000_000,
                        body: Box::new(Statement::Assign {
                            target: SignalRef::Signal(SignalId::from_raw(1)),
                            value: Expr::Literal(LogicVec::from_bool(true)),
                            kind: AssignKind::Blocking,
                            span: Span::DUMMY,
                        }),
                        span: Span::DUMMY,
                    },
                    Statement::Delay {
                        duration_fs: 100_000_000,
                        body: Box::new(Statement::Finish { span: Span::DUMMY }),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        let result = kernel.run(200 * crate::time::FS_PER_NS).unwrap();
        // Should finish by user at 120ns
        assert!(result.finished_by_user);
        assert!(result.final_time.fs >= 120_000_000);
        // rst_n should be 1 (was set at 20ns)
        let rst = kernel.find_signal("top.out").unwrap();
        assert_eq!(kernel.signal_value(rst).to_u64(), Some(1));
    }

    #[test]
    fn suspended_process_tracking() {
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

        // Initial with just a delay — should result in a suspended process
        top.processes.alloc(aion_ir::Process {
            id: aion_ir::ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            body: Statement::Delay {
                duration_fs: 50_000_000,
                body: Box::new(Statement::Finish { span: Span::DUMMY }),
                span: Span::DUMMY,
            },
            sensitivity: Sensitivity::All,
            span: Span::DUMMY,
        });

        let mut modules = Arena::new();
        modules.alloc(top);
        let design = Design {
            modules,
            top: ModuleId::from_raw(0),
            types,
            source_map: aion_ir::SourceMap::new(),
        };

        let mut kernel = SimKernel::new(&design, &make_test_interner()).unwrap();
        // After executing initial processes, there should be a suspended process
        kernel.execute_initial_processes().unwrap();
        assert_eq!(kernel.suspended_processes.len(), 1);
        assert_eq!(kernel.suspended_processes[0].0.fs, 50_000_000);
    }

    /// Blocking vs non-blocking assignment scheduling (active / NBA regions).
    mod regions {
        use super::*;
        use crate::time::FS_PER_NS;

        /// One test signal: name, width, initial value (`None` = X).
        type SigSpec<'a> = (&'a str, u32, Option<u64>);

        /// A process: kind, sensitivity, body.
        type ProcSpec = (ProcessKind, Sensitivity, Statement);

        /// Builds a single-module design named `top` whose IR signal `i` is
        /// `sigs[i]`, with the given processes and continuous assignments.
        fn build(
            sigs: &[SigSpec<'_>],
            procs: Vec<ProcSpec>,
            assigns: Vec<(SignalRef, Expr)>,
        ) -> (Design, Interner) {
            let interner = Interner::new();
            let mut types = TypeDb::new();
            let mut top = empty_module(0, interner.get_or_intern("top"));
            for (i, &(name, width, init)) in sigs.iter().enumerate() {
                let ty = if width == 1 {
                    types.intern(Type::Bit)
                } else {
                    types.intern(Type::BitVec {
                        width,
                        signed: false,
                    })
                };
                top.signals.alloc(Signal {
                    id: SignalId::from_raw(i as u32),
                    name: interner.get_or_intern(name),
                    ty,
                    kind: SignalKind::Reg,
                    init: init.map(|v| ConstValue::Logic(LogicVec::from_u64(v, width))),
                    clock_domain: None,
                    span: Span::DUMMY,
                });
            }
            for (i, (kind, sensitivity, body)) in procs.into_iter().enumerate() {
                top.processes.alloc(aion_ir::Process {
                    id: aion_ir::ProcessId::from_raw(i as u32),
                    name: None,
                    kind,
                    body,
                    sensitivity,
                    span: Span::DUMMY,
                });
            }
            for (target, value) in assigns {
                top.assignments.push(Assignment {
                    target,
                    value,
                    span: Span::DUMMY,
                });
            }
            let mut modules = Arena::new();
            modules.alloc(top);
            let design = Design {
                modules,
                top: ModuleId::from_raw(0),
                types,
                source_map: aion_ir::SourceMap::new(),
            };
            (design, interner)
        }

        fn s(i: u32) -> SignalRef {
            SignalRef::Signal(SignalId::from_raw(i))
        }

        fn slice(i: u32, high: u32, low: u32) -> SignalRef {
            SignalRef::Slice {
                signal: SignalId::from_raw(i),
                high,
                low,
            }
        }

        fn rd(i: u32) -> Expr {
            Expr::Signal(s(i))
        }

        fn lit(v: u64, w: u32) -> Expr {
            Expr::Literal(LogicVec::from_u64(v, w))
        }

        fn add(a: Expr, b: Expr) -> Expr {
            Expr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(a),
                rhs: Box::new(b),
                ty: aion_ir::TypeId::from_raw(0),
                span: Span::DUMMY,
            }
        }

        fn not(e: Expr) -> Expr {
            Expr::Unary {
                op: aion_ir::UnaryOp::Not,
                operand: Box::new(e),
                ty: aion_ir::TypeId::from_raw(0),
                span: Span::DUMMY,
            }
        }

        /// Blocking assignment `target = value`.
        fn ba(target: SignalRef, value: Expr) -> Statement {
            Statement::Assign {
                target,
                value,
                kind: AssignKind::Blocking,
                span: Span::DUMMY,
            }
        }

        /// Non-blocking assignment `target <= value`.
        fn nb(target: SignalRef, value: Expr) -> Statement {
            Statement::Assign {
                target,
                value,
                kind: AssignKind::NonBlocking,
                span: Span::DUMMY,
            }
        }

        fn blk(stmts: Vec<Statement>) -> Statement {
            Statement::Block {
                stmts,
                span: Span::DUMMY,
            }
        }

        /// `#ns body`.
        fn dly(ns: u64, body: Statement) -> Statement {
            Statement::Delay {
                duration_fs: ns * FS_PER_NS,
                body: Box::new(body),
                span: Span::DUMMY,
            }
        }

        fn initial(body: Statement) -> ProcSpec {
            (ProcessKind::Initial, Sensitivity::All, body)
        }

        /// `always @(posedge sig) body`.
        fn at_posedge(sig: u32, body: Statement) -> ProcSpec {
            (
                ProcessKind::Sequential,
                Sensitivity::EdgeList(vec![EdgeSensitivity {
                    signal: SignalId::from_raw(sig),
                    edge: Edge::Posedge,
                }]),
                body,
            )
        }

        /// A clock generator on signal `clk`: `clk = 0;` then a rising edge
        /// every 10 ns (at 5, 15, 25, ...) for `edges` edges.
        fn clock(clk: u32, edges: u64) -> ProcSpec {
            let mut body = Statement::Nop;
            for _ in 0..edges {
                body = dly(
                    5,
                    blk(vec![
                        ba(s(clk), lit(1, 1)),
                        dly(5, blk(vec![ba(s(clk), lit(0, 1)), body])),
                    ]),
                );
            }
            initial(blk(vec![ba(s(clk), lit(0, 1)), body]))
        }

        fn run(design: &Design, interner: &Interner) -> (SimKernel, SimResult) {
            let mut kernel = SimKernel::new(design, interner).unwrap();
            let result = kernel.run_to_completion().unwrap();
            (kernel, result)
        }

        fn val(kernel: &SimKernel, name: &str) -> Option<u64> {
            let id = kernel
                .find_signal(&format!("top.{name}"))
                .unwrap_or_else(|| panic!("no signal {name}"));
            kernel.signal_value(id).to_u64()
        }

        #[test]
        fn sim3_blocking_write_visible_in_same_initial() {
            // initial begin a = 1; b = a; end  → a = 1, b = 1 (was X).
            let (design, interner) = build(
                &[("a", 1, None), ("b", 1, None)],
                vec![initial(blk(vec![ba(s(0), lit(1, 1)), ba(s(1), rd(0))]))],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "a"), Some(1));
            assert_eq!(val(&kernel, "b"), Some(1));
        }

        #[test]
        fn blocking_temp_chain_in_clocked_process() {
            // always @(posedge clk) begin tmp = x + y; z = tmp; end
            let (design, interner) = build(
                &[
                    ("clk", 1, None),
                    ("x", 8, Some(3)),
                    ("y", 8, Some(4)),
                    ("tmp", 8, None),
                    ("z", 8, None),
                ],
                vec![
                    clock(0, 1),
                    at_posedge(0, blk(vec![ba(s(3), add(rd(1), rd(2))), ba(s(4), rd(3))])),
                ],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "tmp"), Some(7));
            assert_eq!(val(&kernel, "z"), Some(7));
        }

        #[test]
        fn nonblocking_swap_on_posedge() {
            // always @(posedge clk) begin a <= b; b <= a; end
            let (design, interner) = build(
                &[("clk", 1, None), ("a", 8, Some(1)), ("b", 8, Some(2))],
                vec![
                    clock(0, 1),
                    at_posedge(0, blk(vec![nb(s(1), rd(2)), nb(s(2), rd(1))])),
                ],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "a"), Some(2));
            assert_eq!(val(&kernel, "b"), Some(1));
        }

        #[test]
        fn blocking_swap_does_not_swap() {
            // always @(posedge clk) begin a = b; b = a; end
            let (design, interner) = build(
                &[("clk", 1, None), ("a", 8, Some(1)), ("b", 8, Some(2))],
                vec![
                    clock(0, 1),
                    at_posedge(0, blk(vec![ba(s(1), rd(2)), ba(s(2), rd(1))])),
                ],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "a"), Some(2));
            assert_eq!(val(&kernel, "b"), Some(2));
        }

        /// d -> q1 -> q2 shift register built from two `<=` processes,
        /// listed in the given order; returns (q1, q2) after `edges` edges.
        fn shift_register(stage1_first: bool, edges: u64) -> (Option<u64>, Option<u64>) {
            let stage1 = at_posedge(0, nb(s(2), rd(1)));
            let stage2 = at_posedge(0, nb(s(3), rd(2)));
            let procs = if stage1_first {
                vec![clock(0, edges), stage1, stage2]
            } else {
                vec![clock(0, edges), stage2, stage1]
            };
            let (design, interner) = build(
                &[
                    ("clk", 1, None),
                    ("d", 1, Some(1)),
                    ("q1", 1, Some(0)),
                    ("q2", 1, Some(0)),
                ],
                procs,
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            (val(&kernel, "q1"), val(&kernel, "q2"))
        }

        #[test]
        fn nba_shift_register_is_process_order_independent() {
            for stage1_first in [true, false] {
                assert_eq!(
                    shift_register(stage1_first, 1),
                    (Some(1), Some(0)),
                    "one edge, stage1_first = {stage1_first}"
                );
                assert_eq!(
                    shift_register(stage1_first, 2),
                    (Some(1), Some(1)),
                    "two edges, stage1_first = {stage1_first}"
                );
            }
        }

        #[test]
        fn nba_waits_for_active_region_to_drain() {
            // assign clk2 = clk;
            // always @(posedge clk)  q1 <= d;
            // always @(posedge clk2) q2 <= q1;
            // clk2 rises one delta after clk, but q1's NBA must not be
            // applied before then: q2 samples the old q1, as in IEEE 1364.
            let (design, interner) = build(
                &[
                    ("clk", 1, None),
                    ("clk2", 1, Some(0)),
                    ("d", 1, Some(1)),
                    ("q1", 1, Some(0)),
                    ("q2", 1, Some(0)),
                ],
                vec![
                    clock(0, 1),
                    at_posedge(0, nb(s(3), rd(2))),
                    at_posedge(1, nb(s(4), rd(3))),
                ],
                vec![(s(1), rd(0))],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "clk2"), Some(0));
            assert_eq!(val(&kernel, "q1"), Some(1));
            assert_eq!(val(&kernel, "q2"), Some(0), "q2 must sample the old q1");
        }

        #[test]
        fn nba_rhs_is_sampled_when_the_statement_runs() {
            // always @(posedge clk) begin a <= b; b = 9; end
            // a gets b's value at the `<=` (2), not the later blocking 9.
            let (design, interner) = build(
                &[("clk", 1, None), ("a", 8, Some(0)), ("b", 8, Some(2))],
                vec![
                    clock(0, 1),
                    at_posedge(0, blk(vec![nb(s(1), rd(2)), ba(s(2), lit(9, 8))])),
                ],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "a"), Some(2));
            assert_eq!(val(&kernel, "b"), Some(9));
        }

        #[test]
        fn nonblocking_write_not_visible_later_in_same_process() {
            // initial #1 begin a <= 5; b = a; end  → b reads the old a.
            let (design, interner) = build(
                &[("a", 8, Some(1)), ("b", 8, None)],
                vec![initial(dly(
                    1,
                    blk(vec![nb(s(0), lit(5, 8)), ba(s(1), rd(0))]),
                ))],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "a"), Some(5));
            assert_eq!(val(&kernel, "b"), Some(1));
        }

        #[test]
        fn display_after_blocking_assign_shows_new_value() {
            // initial #1 begin a = 9; b <= 4; $display("a=%d b=%d", a, b); end
            let display = Statement::Display {
                format: "a=%d b=%d".into(),
                args: vec![rd(0), rd(1)],
                span: Span::DUMMY,
            };
            let (design, interner) = build(
                &[("a", 8, Some(0)), ("b", 8, Some(0))],
                vec![initial(dly(
                    1,
                    blk(vec![ba(s(0), lit(9, 8)), nb(s(1), lit(4, 8)), display]),
                ))],
                vec![],
            );
            let (_, result) = run(&design, &interner);
            assert_eq!(result.display_output, vec!["a=9 b=0".to_string()]);
        }

        #[test]
        fn blocking_writes_committed_before_delayed_resume() {
            // P1: initial #2 begin a = 5; #1 b = a; end
            // P2: initial #5 c = a;
            // After P1's delay its earlier write is committed for everyone.
            let (design, interner) = build(
                &[("a", 8, Some(0)), ("b", 8, None), ("c", 8, None)],
                vec![
                    initial(dly(
                        2,
                        blk(vec![ba(s(0), lit(5, 8)), dly(1, ba(s(1), rd(0)))]),
                    )),
                    initial(dly(5, ba(s(2), rd(0)))),
                ],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "b"), Some(5));
            assert_eq!(val(&kernel, "c"), Some(5));
        }

        #[test]
        fn zero_delay_resume_sees_active_region_results() {
            // assign w = a;
            // initial #5 begin a = 7; #0 b = w; end
            // `#0` resumes in the inactive region, after w has followed a.
            let (design, interner) = build(
                &[("a", 8, Some(0)), ("w", 8, None), ("b", 8, None)],
                vec![initial(dly(
                    5,
                    blk(vec![
                        ba(s(0), lit(7, 8)),
                        Statement::Delay {
                            duration_fs: 0,
                            body: Box::new(ba(s(2), rd(1))),
                            span: Span::DUMMY,
                        },
                    ]),
                ))],
                vec![(s(1), rd(0))],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "w"), Some(7));
            assert_eq!(val(&kernel, "b"), Some(7));
        }

        #[test]
        fn partial_blocking_writes_visible_and_committed() {
            // initial #1 begin x[3:0] = 4'hA; x[7:4] = 4'h5; y = x; end
            let (design, interner) = build(
                &[("x", 8, Some(0)), ("y", 8, None)],
                vec![initial(dly(
                    1,
                    blk(vec![
                        ba(slice(0, 3, 0), lit(0xA, 4)),
                        ba(slice(0, 7, 4), lit(0x5, 4)),
                        ba(s(1), rd(0)),
                    ]),
                ))],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "x"), Some(0x5A));
            assert_eq!(val(&kernel, "y"), Some(0x5A));
        }

        #[test]
        fn concat_blocking_target_visible_and_committed() {
            // initial #1 begin {a, b} = 16'h1234; c = a; end
            let (design, interner) = build(
                &[("a", 8, Some(0)), ("b", 8, Some(0)), ("c", 8, None)],
                vec![initial(dly(
                    1,
                    blk(vec![
                        ba(SignalRef::Concat(vec![s(0), s(1)]), lit(0x1234, 16)),
                        ba(s(2), rd(0)),
                    ]),
                ))],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "a"), Some(0x12));
            assert_eq!(val(&kernel, "b"), Some(0x34));
            assert_eq!(val(&kernel, "c"), Some(0x12));
        }

        #[test]
        fn partial_nonblocking_writes_merge() {
            // always @(posedge clk) begin x[3:0] <= 4'hA; x[7:4] <= 4'h5; end
            let (design, interner) = build(
                &[("clk", 1, None), ("x", 8, Some(0))],
                vec![
                    clock(0, 1),
                    at_posedge(
                        0,
                        blk(vec![
                            nb(slice(1, 3, 0), lit(0xA, 4)),
                            nb(slice(1, 7, 4), lit(0x5, 4)),
                        ]),
                    ),
                ],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "x"), Some(0x5A));
        }

        #[test]
        fn last_nonblocking_write_wins() {
            // always @(posedge clk) begin a <= 1; a <= 2; end
            let (design, interner) = build(
                &[("clk", 1, None), ("a", 8, Some(0))],
                vec![
                    clock(0, 1),
                    at_posedge(0, blk(vec![nb(s(1), lit(1, 8)), nb(s(1), lit(2, 8))])),
                ],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "a"), Some(2));
        }

        #[test]
        fn nonblocking_in_initial_applies_and_triggers_at_time_zero() {
            // initial a <= 1;   always @(posedge a) b = 3;
            let (design, interner) = build(
                &[("a", 1, Some(0)), ("b", 8, Some(0))],
                vec![
                    initial(nb(s(0), lit(1, 1))),
                    at_posedge(0, ba(s(1), lit(3, 8))),
                ],
                vec![],
            );
            let (kernel, result) = run(&design, &interner);
            assert_eq!(val(&kernel, "a"), Some(1));
            assert_eq!(val(&kernel, "b"), Some(3));
            assert_eq!(result.final_time.fs, 0);
        }

        /// `always @(a) a <= ~a;` kicked by an event at 1 ns: every NBA
        /// application re-triggers the process at the same time.
        fn nba_oscillator() -> (Design, Interner) {
            build(
                &[("a", 1, Some(0))],
                vec![(
                    ProcessKind::Sequential,
                    Sensitivity::SignalList(vec![SignalId::from_raw(0)]),
                    nb(s(0), not(rd(0))),
                )],
                vec![],
            )
        }

        #[test]
        fn nba_oscillation_hits_delta_limit() {
            let (design, interner) = nba_oscillator();
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            kernel.set_max_delta(50);
            let a = kernel.find_signal("top.a").unwrap();
            kernel.schedule_event(SimTime::from_ns(1), a, LogicVec::from_bool(true));
            let err = kernel.run_to_completion().unwrap_err();
            assert!(
                matches!(err, SimError::DeltaCycleLimit { fs, max_deltas: 50 } if fs == FS_PER_NS),
                "unexpected: {err:?}"
            );
        }

        #[test]
        fn nba_oscillation_hits_delta_limit_in_run_until() {
            let (design, interner) = nba_oscillator();
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            kernel.set_max_delta(50);
            kernel.initialize().unwrap();
            let a = kernel.find_signal("top.a").unwrap();
            kernel.schedule_event(SimTime::from_ns(1), a, LogicVec::from_bool(true));
            let err = kernel.run_until(10 * FS_PER_NS).unwrap_err();
            assert!(matches!(err, SimError::DeltaCycleLimit { .. }), "{err:?}");
        }

        #[test]
        fn nba_applications_are_charged_as_deltas() {
            // Two chained NBA stages after a clock edge need deltas for: the
            // edge, q1's NBA and q2's NBA. A limit that only covers the edge
            // must trip, one that covers all three must not.
            let make = || {
                build(
                    &[
                        ("clk", 1, None),
                        ("d", 1, Some(1)),
                        ("q1", 1, Some(0)),
                        ("q2", 1, Some(0)),
                    ],
                    vec![
                        clock(0, 1),
                        at_posedge(0, nb(s(2), rd(1))),
                        (
                            ProcessKind::Sequential,
                            Sensitivity::SignalList(vec![SignalId::from_raw(2)]),
                            nb(s(3), rd(2)),
                        ),
                    ],
                    vec![],
                )
            };
            let (design, interner) = make();
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            kernel.set_max_delta(2);
            assert!(matches!(
                kernel.run_to_completion(),
                Err(SimError::DeltaCycleLimit { .. })
            ));

            let (design, interner) = make();
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            kernel.set_max_delta(4);
            kernel.run_to_completion().unwrap();
            assert_eq!(val(&kernel, "q2"), Some(1));
        }

        #[test]
        fn run_until_applies_nba_region() {
            let (design, interner) = build(
                &[("clk", 1, None), ("a", 8, Some(1)), ("b", 8, Some(2))],
                vec![
                    clock(0, 1),
                    at_posedge(0, blk(vec![nb(s(1), rd(2)), nb(s(2), rd(1))])),
                ],
                vec![],
            );
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            kernel.initialize().unwrap();
            kernel.run_until(7 * FS_PER_NS).unwrap();
            assert_eq!(val(&kernel, "a"), Some(2));
            assert_eq!(val(&kernel, "b"), Some(1));
            assert!(kernel.nba_queue.is_empty());
        }

        /// `process(all)` that counts its own runs in `n` and drives
        /// `y <= a`; `n` is a VHDL variable when `n_is_variable`.
        fn counting_process(n_is_variable: bool) -> (Design, Interner) {
            let (mut design, interner) = build(
                &[("a", 1, Some(0)), ("n", 8, Some(0)), ("y", 1, None)],
                vec![(
                    ProcessKind::Combinational,
                    Sensitivity::All,
                    blk(vec![ba(s(1), add(rd(1), lit(1, 8))), nb(s(2), rd(0))]),
                )],
                vec![],
            );
            if n_is_variable {
                design
                    .modules
                    .get_mut(ModuleId::from_raw(0))
                    .signals
                    .get_mut(SignalId::from_raw(1))
                    .kind = SignalKind::Variable;
            }
            (design, interner)
        }

        #[test]
        fn variable_write_does_not_wake_its_process_all() {
            // Runs at time 0 and once per change of `a` (1 ns, 2 ns): n = 3.
            let (design, interner) = counting_process(true);
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            let a = kernel.find_signal("top.a").unwrap();
            kernel.schedule_event(SimTime::from_ns(1), a, LogicVec::from_bool(true));
            kernel.schedule_event(SimTime::from_ns(2), a, LogicVec::from_bool(false));
            kernel.run_to_completion().unwrap();
            assert_eq!(val(&kernel, "n"), Some(3));
            assert_eq!(val(&kernel, "y"), Some(0));
        }

        #[test]
        fn signal_write_still_wakes_its_process_all() {
            // The same body with `n` as an ordinary signal re-triggers itself
            // forever, which the delta-cycle limit reports.
            let (design, interner) = counting_process(false);
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            kernel.set_max_delta(100);
            assert!(matches!(
                kernel.run_to_completion(),
                Err(SimError::DeltaCycleLimit { fs: 0, .. })
            ));
        }

        #[test]
        fn declared_init_is_fitted_to_signal_width() {
            // A 4-bit init on an 8-bit signal is zero-extended, not adopted.
            let (mut design, interner) = build(&[("x", 8, None)], vec![], vec![]);
            design
                .modules
                .get_mut(ModuleId::from_raw(0))
                .signals
                .get_mut(SignalId::from_raw(0))
                .init = Some(ConstValue::Logic(LogicVec::from_u64(0xA, 4)));
            let kernel = SimKernel::new(&design, &interner).unwrap();
            let id = kernel.find_signal("top.x").unwrap();
            assert_eq!(kernel.signal_value(id).width(), 8);
            assert_eq!(kernel.signal_value(id).to_u64(), Some(0x0A));
        }

        #[test]
        fn initial_narrow_write_keeps_signal_width() {
            // reg [7:0] count; initial count = 1'b0;  then on each of three
            // clock edges count = count + 1'b1  → 3 (it wrapped mod 2 when
            // the immediate write shrank count to one bit).
            let (design, interner) = build(
                &[("clk", 1, None), ("count", 8, None)],
                vec![
                    clock(0, 3),
                    initial(ba(s(1), lit(0, 1))),
                    at_posedge(0, ba(s(1), add(rd(1), lit(1, 1)))),
                ],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            let id = kernel.find_signal("top.count").unwrap();
            assert_eq!(kernel.signal_value(id).width(), 8);
            assert_eq!(val(&kernel, "count"), Some(3));
        }

        #[test]
        fn merged_slice_after_narrow_full_write_does_not_panic() {
            // x = 1'b1; x[7:4] = 4'h5;  in one segment → 0x51.
            let (design, interner) = build(
                &[("x", 8, Some(0xFF))],
                vec![initial(dly(
                    1,
                    blk(vec![ba(s(0), lit(1, 1)), ba(slice(0, 7, 4), lit(5, 4))]),
                ))],
                vec![],
            );
            let (kernel, _) = run(&design, &interner);
            assert_eq!(val(&kernel, "x"), Some(0x51));
        }

        #[test]
        fn step_delta_applies_pending_nba_region() {
            // Interactive stepping: after initialize(), the only work left is
            // the initial block's NBA; step_delta() must apply it.
            let (design, interner) = build(
                &[("a", 8, Some(0))],
                vec![initial(nb(s(0), lit(6, 8)))],
                vec![],
            );
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            kernel.initialize().unwrap();
            assert!(kernel.has_pending_events());
            assert_eq!(kernel.next_event_time_fs(), Some(0));
            assert_eq!(val(&kernel, "a"), Some(0));
            assert_eq!(kernel.step_delta().unwrap(), StepResult::Continued);
            assert_eq!(val(&kernel, "a"), Some(6));
            assert!(!kernel.has_pending_events());
            assert_eq!(kernel.step_delta().unwrap(), StepResult::Done);
        }

        #[test]
        fn apply_nba_region_waits_for_active_work() {
            let (design, interner) = build(&[("a", 8, Some(0))], vec![], vec![]);
            let mut kernel = SimKernel::new(&design, &interner).unwrap();
            let a = kernel.find_signal("top.a").unwrap();
            kernel.nba_queue.push(PendingUpdate {
                target: a,
                value: LogicVec::from_u64(3, 8),
                range: None,
                kind: AssignKind::NonBlocking,
            });
            // An event due now is active work: the NBA region must wait.
            kernel.schedule_event(SimTime::zero(), a, LogicVec::from_u64(1, 8));
            assert!(!kernel.apply_nba_region());
            assert_eq!(kernel.nba_queue.len(), 1);
            kernel.step_delta().unwrap();
            // Active region drained: now it applies, one delta later.
            assert!(kernel.apply_nba_region());
            assert!(kernel.nba_queue.is_empty());
            kernel.step_delta().unwrap();
            assert_eq!(val(&kernel, "a"), Some(3));
            // Nothing queued: a no-op.
            assert!(!kernel.apply_nba_region());
        }
    }

    // ---- Unified process dispatch (#4: SIM-1, SIM-2, SIM-21) ----

    const NS: u64 = crate::time::FS_PER_NS;

    /// `SignalRef` to signal `i` of the test module.
    fn sref(i: u32) -> SignalRef {
        SignalRef::Signal(SignalId::from_raw(i))
    }

    /// Reads signal `i`.
    fn rd(i: u32) -> Expr {
        Expr::Signal(sref(i))
    }

    /// A `width`-bit literal.
    fn lit(v: u64, width: u32) -> Expr {
        Expr::Literal(LogicVec::from_u64(v, width))
    }

    /// `~sig[i]` on a 1-bit signal.
    fn inv(i: u32) -> Expr {
        Expr::Unary {
            op: aion_ir::UnaryOp::Not,
            operand: Box::new(rd(i)),
            ty: aion_ir::TypeId::from_raw(0),
            span: Span::DUMMY,
        }
    }

    /// `sig[i] + 1` on an 8-bit signal.
    fn inc(i: u32) -> Expr {
        Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(rd(i)),
            rhs: Box::new(lit(1, 8)),
            ty: aion_ir::TypeId::from_raw(1),
            span: Span::DUMMY,
        }
    }

    /// `sig[i] = value;`
    fn set(i: u32, value: Expr) -> Statement {
        Statement::Assign {
            target: sref(i),
            value,
            kind: AssignKind::Blocking,
            span: Span::DUMMY,
        }
    }

    /// `#ns body`
    fn after(ns: u64, body: Statement) -> Statement {
        Statement::Delay {
            duration_fs: ns * NS,
            body: Box::new(body),
            span: Span::DUMMY,
        }
    }

    fn seq(stmts: Vec<Statement>) -> Statement {
        Statement::Block {
            stmts,
            span: Span::DUMMY,
        }
    }

    fn looping(body: Statement) -> Statement {
        Statement::Forever {
            body: Box::new(body),
            span: Span::DUMMY,
        }
    }

    fn show(format: &str, args: Vec<Expr>) -> Statement {
        Statement::Display {
            format: format.into(),
            args,
            span: Span::DUMMY,
        }
    }

    fn posedge(i: u32) -> Sensitivity {
        Sensitivity::EdgeList(vec![EdgeSensitivity {
            signal: SignalId::from_raw(i),
            edge: Edge::Posedge,
        }])
    }

    /// Builds module `top` from `(interner name index, type id, init)` signals
    /// (signal `n` is the `n`-th entry), processes, and continuous assignments.
    fn build_design(
        signals: &[(u32, u32, Option<i64>)],
        processes: Vec<(ProcessKind, Sensitivity, Statement)>,
        assigns: Vec<(u32, Expr)>,
    ) -> Design {
        let mut top = empty_module(0, Ident::from_raw(1));
        for (n, &(name, ty, init)) in signals.iter().enumerate() {
            top.signals.alloc(Signal {
                id: SignalId::from_raw(n as u32),
                name: Ident::from_raw(name),
                ty: aion_ir::TypeId::from_raw(ty),
                kind: SignalKind::Reg,
                init: init.map(ConstValue::Int),
                clock_domain: None,
                span: Span::DUMMY,
            });
        }
        for (n, (kind, sensitivity, body)) in processes.into_iter().enumerate() {
            top.processes.alloc(aion_ir::Process {
                id: aion_ir::ProcessId::from_raw(n as u32),
                name: None,
                kind,
                body,
                sensitivity,
                span: Span::DUMMY,
            });
        }
        for (target, value) in assigns {
            top.assignments.push(Assignment {
                target: sref(target),
                value,
                span: Span::DUMMY,
            });
        }
        let mut modules = Arena::new();
        modules.alloc(top);
        Design {
            modules,
            top: ModuleId::from_raw(0),
            types: make_type_db(),
            source_map: aion_ir::SourceMap::new(),
        }
    }

    fn kernel_for(design: &Design) -> SimKernel {
        SimKernel::new(design, &make_test_interner()).unwrap()
    }

    fn value_of(kernel: &SimKernel, name: &str) -> Option<u64> {
        kernel
            .signal_value(kernel.find_signal(name).unwrap())
            .to_u64()
    }

    /// Initial process driving a single rising edge on signal 0 at 5 ns.
    fn one_posedge_at_5ns() -> (ProcessKind, Sensitivity, Statement) {
        (
            ProcessKind::Initial,
            Sensitivity::All,
            seq(vec![set(0, lit(0, 1)), after(5, set(0, lit(1, 1)))]),
        )
    }

    // Repro A (SIM-1): an initial block that reads a signal driven by a
    // continuous assignment used to be re-run from the top whenever that
    // signal changed, repeating `b = 0` after `b = 1`.
    #[test]
    fn initial_block_not_retriggered_by_read_signal() {
        // Signals: 0 = a, 1 = b.  assign a = ~b;
        // initial begin b = 0; #10 begin b = 1; $display(a); end end
        let design = build_design(
            &[(8, 0, None), (9, 0, None)],
            vec![(
                ProcessKind::Initial,
                Sensitivity::All,
                seq(vec![
                    set(1, lit(0, 1)),
                    after(10, seq(vec![set(1, lit(1, 1)), show("a=%b", vec![rd(0)])])),
                ]),
            )],
            vec![(0, inv(1))],
        );
        let mut kernel = kernel_for(&design);
        let result = kernel.run(100 * NS).unwrap();
        assert_eq!(value_of(&kernel, "top.b"), Some(1), "b must stay 1");
        assert_eq!(value_of(&kernel, "top.a"), Some(0));
        assert_eq!(result.display_output, vec!["a=1".to_string()]);
    }

    #[test]
    fn initial_block_without_delay_runs_exactly_once() {
        // Signals: 0 = a, 1 = b.  assign a = ~b;
        // initial begin $display(a); b = 1; end  -- reads `a`, which changes.
        let design = build_design(
            &[(8, 0, None), (9, 0, Some(0))],
            vec![(
                ProcessKind::Initial,
                Sensitivity::All,
                seq(vec![show("a=%b", vec![rd(0)]), set(1, lit(1, 1))]),
            )],
            vec![(0, inv(1))],
        );
        let mut kernel = kernel_for(&design);
        let result = kernel.run(50 * NS).unwrap();
        assert_eq!(
            result.display_output.len(),
            1,
            "{:?}",
            result.display_output
        );
        assert_eq!(value_of(&kernel, "top.a"), Some(0));
    }

    #[test]
    fn initial_and_looping_processes_not_in_sensitivity_map() {
        // Signals: 0 = clk, 1 = q. Process 0 (initial) and process 1 (a
        // looping always) both read clk; only process 2 (`q = clk`) may be
        // woken by it.
        let design = build_design(
            &[(2, 0, Some(0)), (7, 0, Some(0))],
            vec![
                (
                    ProcessKind::Initial,
                    Sensitivity::All,
                    show("clk=%b", vec![rd(0)]),
                ),
                (
                    ProcessKind::Combinational,
                    Sensitivity::All,
                    looping(after(5, set(0, inv(0)))),
                ),
                (ProcessKind::Combinational, Sensitivity::All, set(1, rd(0))),
            ],
            vec![],
        );
        let kernel = kernel_for(&design);
        let registered: HashSet<usize> =
            kernel.sensitivity_map.values().flatten().copied().collect();
        assert_eq!(registered, HashSet::from([2]));
    }

    // Repro B (SIM-2): a combinational process whose body is a delay (the
    // old lowering of `always #5 clk = ~clk;`) never ran: its Suspend was
    // dropped by the initial combinational pass.
    #[test]
    fn combinational_delay_suspend_is_honored() {
        let design = build_design(
            &[(2, 0, None)],
            vec![
                (ProcessKind::Initial, Sensitivity::All, set(0, lit(0, 1))),
                (
                    ProcessKind::Combinational,
                    Sensitivity::All,
                    after(5, set(0, inv(0))),
                ),
            ],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        let result = kernel.run(47 * NS).unwrap();
        assert_eq!(result.final_time.fs, 45 * NS);
        // Toggles at 5, 10, ..., 45 ns: nine toggles from 0.
        assert_eq!(value_of(&kernel, "top.clk"), Some(1));
    }

    /// `always #5 clk = ~clk;` as the elaborator now lowers it (a loop).
    #[test]
    fn looping_always_clock_generator() {
        let design = build_design(
            &[(2, 0, None)],
            vec![
                (ProcessKind::Initial, Sensitivity::All, set(0, lit(0, 1))),
                (
                    ProcessKind::Combinational,
                    Sensitivity::All,
                    looping(after(5, set(0, inv(0)))),
                ),
            ],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        let result = kernel.run(47 * NS).unwrap();
        assert_eq!(result.final_time.fs, 45 * NS);
        assert_eq!(value_of(&kernel, "top.clk"), Some(1));
        // Exactly one copy of the process is ever pending.
        assert_eq!(kernel.suspended_processes.len(), 1);
    }

    /// `always begin #5 clk = 0; #5 clk = 1; end`, observed across several
    /// `run()` calls (which must not restart the process).
    #[test]
    fn looping_always_with_two_delays() {
        let design = build_design(
            &[(2, 0, None)],
            vec![(
                ProcessKind::Combinational,
                Sensitivity::All,
                looping(seq(vec![
                    after(5, set(0, lit(0, 1))),
                    after(5, set(0, lit(1, 1))),
                ])),
            )],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        kernel.run(3 * NS).unwrap();
        assert_eq!(value_of(&kernel, "top.clk"), None, "still X before 5 ns");
        let mut seen = Vec::new();
        for _ in 0..4 {
            kernel.run(5 * NS).unwrap(); // 8, 13, 18, 23 ns
            seen.push(value_of(&kernel, "top.clk"));
        }
        assert_eq!(seen, vec![Some(0), Some(1), Some(0), Some(1)]);
        assert_eq!(kernel.suspended_processes.len(), 1);
    }

    // Repro B2 (SIM-2): `always @(posedge clk) begin q = 1; #2 q = 0; end`
    // lost everything after the delay.
    #[test]
    fn edge_triggered_process_resumes_after_delay() {
        // Signals: 0 = clk, 1 = q.
        let design = build_design(
            &[(2, 0, None), (7, 0, Some(0))],
            vec![
                one_posedge_at_5ns(),
                (
                    ProcessKind::Sequential,
                    posedge(0),
                    seq(vec![set(1, lit(1, 1)), after(2, set(1, lit(0, 1)))]),
                ),
            ],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        kernel.run(6 * NS).unwrap();
        assert_eq!(value_of(&kernel, "top.q"), Some(1), "q = 1 at the edge");
        kernel.run(44 * NS).unwrap();
        assert_eq!(value_of(&kernel, "top.q"), Some(0), "q = 0 two ns later");
    }

    // A sensitivity-triggered process that is mid-delay misses further
    // events (IEEE 1800 9.4): it must not be started a second time.
    #[test]
    fn suspended_process_not_retriggered() {
        // Signals: 0 = clk (posedges at 5, 15, 25, 35, 45 ns), 1 = count,
        // 2 = q.
        // always @(posedge clk) begin count = count + 1; #12 q = q + 1; end
        let design = build_design(
            &[(2, 0, None), (6, 1, Some(0)), (7, 1, Some(0))],
            vec![
                (
                    ProcessKind::Initial,
                    Sensitivity::All,
                    seq(vec![set(0, lit(0, 1)), looping(after(5, set(0, inv(0))))]),
                ),
                (
                    ProcessKind::Sequential,
                    posedge(0),
                    seq(vec![set(1, inc(1)), after(12, set(2, inc(2)))]),
                ),
            ],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        kernel.run(50 * NS).unwrap();
        // Edges at 5, 25, 45 start the block; 15 and 35 arrive mid-delay.
        assert_eq!(value_of(&kernel, "top.count"), Some(3));
        // Delays complete at 17 and 37 ns (the third resumes at 57 ns).
        assert_eq!(value_of(&kernel, "top.q"), Some(2));
    }

    #[test]
    fn edge_triggered_process_without_delay_still_retriggers() {
        // always @(posedge clk) count = count + 1;  with posedges every 10 ns.
        let design = build_design(
            &[(2, 0, None), (6, 1, Some(0))],
            vec![
                (
                    ProcessKind::Initial,
                    Sensitivity::All,
                    seq(vec![set(0, lit(0, 1)), looping(after(5, set(0, inv(0))))]),
                ),
                (ProcessKind::Sequential, posedge(0), set(1, inc(1))),
            ],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        kernel.run(50 * NS).unwrap();
        assert_eq!(value_of(&kernel, "top.count"), Some(5));
    }

    #[test]
    fn finish_after_delay_in_triggered_process_stops_simulation() {
        // always @(posedge clk) #1 $finish;
        let design = build_design(
            &[(2, 0, None)],
            vec![
                one_posedge_at_5ns(),
                (
                    ProcessKind::Sequential,
                    posedge(0),
                    after(1, Statement::Finish { span: Span::DUMMY }),
                ),
            ],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        let result = kernel.run(100 * NS).unwrap();
        assert!(result.finished_by_user);
        assert_eq!(result.final_time.fs, 6 * NS);
    }

    #[test]
    fn looping_process_without_timing_control_errors() {
        // always clk = ~clk;  (lowered as a loop) must fail, not hang.
        let design = build_design(
            &[(2, 0, Some(0))],
            vec![(
                ProcessKind::Combinational,
                Sensitivity::All,
                looping(set(0, inv(0))),
            )],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        assert!(matches!(kernel.run(10 * NS), Err(SimError::ZeroTimeLoop)));
    }

    // SIM-21: run() twice / initialize() then run() must not re-run
    // initial blocks or the initial combinational pass.
    fn hello_design() -> Design {
        build_design(
            &[(2, 0, None)],
            vec![
                (
                    ProcessKind::Initial,
                    Sensitivity::All,
                    show("hello", vec![]),
                ),
                (
                    ProcessKind::Combinational,
                    Sensitivity::All,
                    looping(after(5, set(0, lit(1, 1)))),
                ),
            ],
            vec![],
        )
    }

    #[test]
    fn run_twice_does_not_rerun_initial_blocks() {
        let mut kernel = kernel_for(&hello_design());
        kernel.run(10 * NS).unwrap();
        let result = kernel.run(10 * NS).unwrap();
        assert_eq!(result.display_output, vec!["hello".to_string()]);
        assert_eq!(kernel.suspended_processes.len(), 1);
    }

    #[test]
    fn initialize_then_run_does_not_rerun_initial_blocks() {
        let mut kernel = kernel_for(&hello_design());
        kernel.initialize().unwrap();
        kernel.initialize().unwrap();
        let result = kernel.run(10 * NS).unwrap();
        assert_eq!(result.display_output, vec!["hello".to_string()]);
        assert_eq!(kernel.suspended_processes.len(), 1);
    }

    #[test]
    fn run_until_initializes_once() {
        let mut kernel = kernel_for(&hello_design());
        kernel.run_until(7 * NS).unwrap();
        assert_eq!(value_of(&kernel, "top.clk"), Some(1));
        kernel.run_until(12 * NS).unwrap();
        kernel.run(5 * NS).unwrap();
        assert_eq!(kernel.take_display_output(), vec!["hello".to_string()]);
        assert_eq!(kernel.suspended_processes.len(), 1);
    }

    #[test]
    fn run_until_does_not_leave_its_target_as_time_limit() {
        // The 5 ns wakeup is beyond run_until(2 ns). Afterwards, an unbounded
        // run_to_completion() must still process it rather than stopping at
        // the 2 ns target left behind as a stale limit.
        // initial #5 clk = 1; — a single wakeup, then the design is idle.
        let design = build_design(
            &[(2, 0, None)],
            vec![(
                ProcessKind::Initial,
                Sensitivity::All,
                after(5, set(0, lit(1, 1))),
            )],
            vec![],
        );
        let mut kernel = kernel_for(&design);
        assert_eq!(kernel.run_until(2 * NS).unwrap(), StepResult::Continued);
        assert_eq!(kernel.time_limit, None);
        kernel.run_to_completion().unwrap();
        assert_eq!(value_of(&kernel, "top.clk"), Some(1));
        assert_eq!(kernel.current_time().fs, 5 * NS);
    }

    #[test]
    fn run_until_restores_caller_time_limit() {
        let mut kernel = kernel_for(&hello_design());
        kernel.set_time_limit(9 * NS);
        kernel.run_until(2 * NS).unwrap();
        assert_eq!(kernel.time_limit, Some(9 * NS));
    }

    #[test]
    fn run_until_restores_time_limit_on_error() {
        // `initial forever #0;` trips the delta limit inside run_until; the
        // caller's (absent) limit must still be restored.
        let mut kernel = kernel_for(&make_initial_design(forever_zero_delay()));
        kernel.set_max_delta(10);
        assert!(kernel.run_until(5 * NS).is_err());
        assert_eq!(kernel.time_limit, None);
    }

    #[test]
    fn run_until_after_run_is_not_bounded_by_stale_limit() {
        // run() leaves time_limit at 3 ns; run_until(7 ns) must still apply
        // the 5 ns wakeup's update instead of spinning on it.
        let mut kernel = kernel_for(&hello_design());
        kernel.run(3 * NS).unwrap();
        assert_eq!(kernel.run_until(7 * NS).unwrap(), StepResult::Continued);
        assert_eq!(value_of(&kernel, "top.clk"), Some(1));
        assert_eq!(kernel.current_time().fs, 7 * NS);
    }

    /// A waveform recorder that counts `(register_signal, begin_scope,
    /// time-0 record_change)` calls.
    #[derive(Default, Clone)]
    struct CountingRecorder {
        counts: std::sync::Arc<std::sync::Mutex<(usize, usize, usize)>>,
    }

    impl WaveformRecorder for CountingRecorder {
        fn register_signal(&mut self, _: SimSignalId, _: &str, _: u32) -> Result<(), SimError> {
            self.counts.lock().unwrap().0 += 1;
            Ok(())
        }
        fn begin_scope(&mut self, _: &str) -> Result<(), SimError> {
            self.counts.lock().unwrap().1 += 1;
            Ok(())
        }
        fn end_scope(&mut self) -> Result<(), SimError> {
            Ok(())
        }
        fn record_change(&mut self, t: u64, _: SimSignalId, _: &LogicVec) -> Result<(), SimError> {
            if t == 0 {
                self.counts.lock().unwrap().2 += 1;
            }
            Ok(())
        }
        fn finalize(&mut self) -> Result<(), SimError> {
            Ok(())
        }
    }

    #[test]
    fn run_twice_writes_waveform_header_once() {
        let rec = CountingRecorder::default();
        let mut kernel = kernel_for(&hello_design());
        kernel.set_recorder(Box::new(rec.clone()));
        kernel.run(10 * NS).unwrap();
        kernel.run(10 * NS).unwrap();
        // One signal, one scope, one time-0 dump.
        assert_eq!(*rec.counts.lock().unwrap(), (1, 1, 1));
    }

    #[test]
    fn initialize_then_run_writes_waveform_header_once() {
        let rec = CountingRecorder::default();
        let mut kernel = kernel_for(&hello_design());
        kernel.set_recorder(Box::new(rec.clone()));
        kernel.initialize().unwrap();
        kernel.run(10 * NS).unwrap();
        kernel.run_until(30 * NS).unwrap();
        assert_eq!(*rec.counts.lock().unwrap(), (1, 1, 1));
    }

    #[test]
    fn recorder_attached_after_initialize_still_gets_header() {
        let rec = CountingRecorder::default();
        let mut kernel = kernel_for(&hello_design());
        kernel.initialize().unwrap();
        kernel.set_recorder(Box::new(rec.clone()));
        kernel.run(10 * NS).unwrap();
        assert_eq!(*rec.counts.lock().unwrap(), (1, 1, 1));
    }
}
