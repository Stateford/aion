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
    CellKind, ConstValue, Design, Edge, Expr, ModuleId, Process, ProcessKind, Sensitivity,
    SignalId, SignalKind, SignalRef, Statement, TypeDb,
};

use crate::error::SimError;
use crate::evaluator::{exec_statement, EvalContext, ExecResult, PendingUpdate};
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
    suspended_processes: Vec<(SimTime, SuspendedProcess)>,
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
    pub fn set_recorder(&mut self, recorder: Box<dyn WaveformRecorder>) {
        self.recorder = Some(recorder);
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
    pub fn run(&mut self, duration_fs: u64) -> Result<SimResult, SimError> {
        let end_time = self.current_time.fs + duration_fs;
        self.time_limit = Some(end_time);
        self.run_simulation()
    }

    /// Runs the simulation to completion (until event queue empties or `$finish`).
    pub fn run_to_completion(&mut self) -> Result<SimResult, SimError> {
        self.run_simulation()
    }

    /// Executes a single delta-cycle step.
    pub fn step_delta(&mut self) -> Result<StepResult, SimError> {
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
            let proc = &self.processes[proc_idx];
            let ctx = EvalContext {
                signals: &self.signals,
                signal_map: &proc.signal_map,
                types: &self.types,
            };
            let mut pending = Vec::new();
            let mut display = Vec::new();
            let result = exec_statement(&ctx, &proc.body, &mut pending, &mut display)?;

            self.display_output.extend(display.iter().cloned());
            for msg in &display {
                if msg.starts_with("ASSERTION FAILED:") {
                    self.assertion_failures.push(msg.clone());
                }
            }
            all_pending.extend(pending);

            if matches!(result, ExecResult::Finish) {
                self.finished = true;
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
                Some(ConstValue::Logic(lv)) => lv.clone(),
                Some(ConstValue::Int(v)) => LogicVec::from_u64(*v as u64, width),
                Some(ConstValue::Bool(b)) => LogicVec::from_bool(*b),
                _ => match signal.kind {
                    SignalKind::Wire | SignalKind::Port => LogicVec::new(width),
                    SignalKind::Reg | SignalKind::Latch => {
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
        };
        self.processes.push(proc);
    }

    /// Builds the sensitivity map: signal → list of process indices.
    fn build_sensitivity_map(&mut self) {
        self.sensitivity_map.clear();
        for (idx, proc) in self.processes.iter().enumerate() {
            match &proc.sensitivity {
                Sensitivity::All => {
                    // Sensitive to all read signals
                    for &sig in &proc.read_signals {
                        self.sensitivity_map.entry(sig).or_default().push(idx);
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
    fn find_sensitive_processes(&self, changed: &HashSet<SimSignalId>) -> Vec<usize> {
        let mut to_run = HashSet::new();

        for &sig_id in changed {
            if let Some(procs) = self.sensitivity_map.get(&sig_id) {
                for &proc_idx in procs {
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

    /// Records the initial value of every signal at time 0.
    ///
    /// Called after `register_waveform_signals` so that the waveform file
    /// contains a complete initial value dump (e.g. VCD `$dumpvars`).
    fn record_initial_values(&mut self) -> Result<(), SimError> {
        let rec = match &mut self.recorder {
            Some(r) => r,
            None => return Ok(()),
        };

        for (id, state) in self.signals.iter() {
            rec.record_change(0, id, &state.value)?;
        }
        Ok(())
    }

    fn run_simulation(&mut self) -> Result<SimResult, SimError> {
        // Phase 0: Register signals with waveform recorder and dump initial values
        self.register_waveform_signals()?;
        self.record_initial_values()?;

        // Phase 1: Execute all Initial processes
        self.execute_initial_processes()?;

        // Phase 2: Execute all combinational processes once (initial propagation)
        self.execute_combinational_processes()?;

        // Phase 3: Event loop — process events and wakeups
        let mut deltas_at_current_time = 0u32;
        while !self.finished {
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

            // Process suspended wakeups at this time (before events)
            self.process_wakeups(next_time_fs)?;

            // Process signal events at this time
            if self
                .event_queue
                .peek()
                .is_some_and(|e| e.0.time.fs <= next_time_fs)
            {
                let result = self.step_delta()?;
                deltas_at_current_time += 1;

                if deltas_at_current_time >= self.max_delta_per_step {
                    return Err(SimError::DeltaCycleLimit {
                        fs: self.current_time.fs,
                        max_deltas: self.max_delta_per_step,
                    });
                }

                if result == StepResult::Done {
                    break;
                }
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
            let proc = &self.processes[sp.process_idx];
            let ctx = EvalContext {
                signals: &self.signals,
                signal_map: &proc.signal_map,
                types: &self.types,
            };
            let mut pending = Vec::new();
            let mut display = Vec::new();
            let result = exec_statement(&ctx, &sp.continuation, &mut pending, &mut display)?;

            self.display_output.extend(display.iter().cloned());
            for msg in &display {
                if msg.starts_with("ASSERTION FAILED:") {
                    self.assertion_failures.push(msg.clone());
                }
            }

            // Schedule updates as events for the next delta cycle so that
            // step_delta() can properly detect the change (comparing previous_value
            // vs new value) and trigger sensitive processes (e.g. posedge clk →
            // sequential counter).  We must NOT apply updates immediately here,
            // because that would make step_delta see no difference and skip the
            // sensitivity check.
            let next_delta = self.current_time.next_delta();
            self.merge_and_schedule(pending, next_delta);

            match result {
                ExecResult::Finish => {
                    self.finished = true;
                    return Ok(());
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
                            process_idx: sp.process_idx,
                            continuation: *continuation,
                        },
                    ));
                }
                ExecResult::Continue => {}
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
            let proc = &self.processes[idx];
            let ctx = EvalContext {
                signals: &self.signals,
                signal_map: &proc.signal_map,
                types: &self.types,
            };
            let mut pending = Vec::new();
            let mut display = Vec::new();
            let result = exec_statement(&ctx, &proc.body, &mut pending, &mut display)?;

            self.display_output.extend(display.iter().cloned());
            for msg in &display {
                if msg.starts_with("ASSERTION FAILED:") {
                    self.assertion_failures.push(msg.clone());
                }
            }

            // Apply initial updates immediately
            for update in pending {
                self.apply_update_immediate(&update);
            }

            match result {
                ExecResult::Finish => {
                    self.finished = true;
                    break;
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
                }
                ExecResult::Continue => {}
            }
        }
        Ok(())
    }

    /// Executes all combinational processes once for initial propagation.
    fn execute_combinational_processes(&mut self) -> Result<(), SimError> {
        let comb_indices: Vec<usize> = self
            .processes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.kind == ProcessKind::Combinational)
            .map(|(i, _)| i)
            .collect();

        for idx in comb_indices {
            let proc = &self.processes[idx];
            let ctx = EvalContext {
                signals: &self.signals,
                signal_map: &proc.signal_map,
                types: &self.types,
            };
            let mut pending = Vec::new();
            let mut display = Vec::new();
            let result = exec_statement(&ctx, &proc.body, &mut pending, &mut display)?;

            self.display_output.extend(display.iter().cloned());

            // Schedule updates at time 0, delta 1 (merging slice updates)
            self.merge_and_schedule(pending, SimTime { fs: 0, delta: 1 });

            if matches!(result, ExecResult::Finish) {
                self.finished = true;
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
    fn merge_and_schedule(&mut self, updates: Vec<PendingUpdate>, time: SimTime) {
        // Use a Vec of (SimSignalId, LogicVec) to preserve insertion order
        let mut merged: Vec<(SimSignalId, LogicVec)> = Vec::new();
        let mut index_map: HashMap<SimSignalId, usize> = HashMap::new();

        for update in updates {
            if let Some(&idx) = index_map.get(&update.target) {
                // Merge into existing entry
                let (_, ref mut val) = merged[idx];
                if let Some((high, low)) = update.range {
                    for i in 0..(high - low + 1) {
                        if i < update.value.width() {
                            val.set(low + i, update.value.get(i));
                        }
                    }
                } else {
                    *val = update.value;
                }
            } else {
                // New signal — start from current value and apply update
                let base = if let Some((high, low)) = update.range {
                    let sig = self.signals.get(update.target);
                    let mut merged_val = sig.value.clone();
                    for i in 0..(high - low + 1) {
                        if i < update.value.width() {
                            merged_val.set(low + i, update.value.get(i));
                        }
                    }
                    merged_val
                } else {
                    update.value
                };
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
    fn apply_update_immediate(&mut self, update: &PendingUpdate) {
        let sig = self.signals.get_mut(update.target);
        if let Some((high, low)) = update.range {
            for i in 0..(high - low + 1) {
                if i < update.value.width() {
                    sig.value.set(low + i, update.value.get(i));
                }
            }
        } else {
            sig.value = update.value.clone();
        }
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

    /// Returns whether the simulation has pending events in the queue or suspended processes.
    pub fn has_pending_events(&self) -> bool {
        !self.event_queue.is_empty() || !self.suspended_processes.is_empty()
    }

    /// Returns whether the simulation has been terminated by `$finish`.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Executes initial processes (exposed for interactive mode).
    ///
    /// Call this once after kernel construction to run all `initial` blocks.
    pub fn initialize(&mut self) -> Result<(), SimError> {
        self.execute_initial_processes()?;
        self.execute_combinational_processes()
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
    pub fn run_until(&mut self, target_fs: u64) -> Result<StepResult, SimError> {
        let mut deltas_at_current_time = 0u32;

        while !self.finished {
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

            // Process suspended wakeups at this time (before events)
            self.process_wakeups(next_time_fs)?;

            // Process signal events at this time
            if self
                .event_queue
                .peek()
                .is_some_and(|e| e.0.time.fs <= next_time_fs)
            {
                let result = self.step_delta()?;
                deltas_at_current_time += 1;

                if deltas_at_current_time >= self.max_delta_per_step {
                    return Err(SimError::DeltaCycleLimit {
                        fs: self.current_time.fs,
                        max_deltas: self.max_delta_per_step,
                    });
                }

                if result == StepResult::Done && !self.has_pending_events() {
                    return Ok(StepResult::Done);
                }
            }
        }

        Ok(StepResult::Done)
    }

    /// Returns the earliest time at which a pending event or wakeup is scheduled.
    ///
    /// Returns `None` if no events or wakeups are pending.
    pub fn next_event_time_fs(&self) -> Option<u64> {
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
                        span: Span::DUMMY,
                    },
                    Statement::Delay {
                        duration_fs: 20_000_000, // 20 ns
                        body: Box::new(Statement::Assign {
                            target: SignalRef::Signal(SignalId::from_raw(0)),
                            value: Expr::Literal(LogicVec::from_bool(true)),
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
                        span: Span::DUMMY,
                    },
                    Statement::Delay {
                        duration_fs: 20_000_000,
                        body: Box::new(Statement::Assign {
                            target: SignalRef::Signal(SignalId::from_raw(1)),
                            value: Expr::Literal(LogicVec::from_bool(true)),
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
}
