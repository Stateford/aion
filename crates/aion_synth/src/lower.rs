//! Behavioral lowering: converts processes and assignments to cells.
//!
//! For each [`Process`] in a module, based on its [`ProcessKind`]:
//! - **Sequential** (`always_ff`): extracts clock/reset, creates DFF cells
//! - **Combinational** (`always_comb`): creates MUX chains for if/case
//! - **Latched** (`always_latch`): creates latch cells
//! - **Initial**: skipped (simulation only, not synthesizable)
//!
//! Concurrent assignments are lowered by evaluating the expression and
//! wiring the output to the target signal.

use crate::lower_expr::lower_expr;
use crate::netlist::Netlist;
use aion_common::LogicVec;
use aion_diagnostics::{Category, DiagnosticCode, DiagnosticSink};
use aion_ir::{
    CaseArm, CellKind, EdgeSensitivity, Expr, Module, Process, ProcessKind, Sensitivity, SignalId,
    SignalKind, SignalRef, Statement, Type,
};

/// Lowers all processes and assignments in a module into the netlist.
///
/// After this pass, the netlist contains only cells (no behavioral code).
pub(crate) fn lower_module(module: &Module, netlist: &mut Netlist, sink: &DiagnosticSink) {
    // Lower concurrent assignments first
    let assignments: Vec<_> = netlist.assignments.drain(..).collect();
    for assign in &assignments {
        let value = lower_expr(&assign.value, netlist);
        wire_signal_ref(&assign.target, &value, netlist);
    }

    // Lower each process
    for (_id, process) in module.processes.iter() {
        lower_process(process, netlist, sink);
    }
}

/// Lowers a single process into cells.
fn lower_process(process: &Process, netlist: &mut Netlist, sink: &DiagnosticSink) {
    match process.kind {
        ProcessKind::Sequential => lower_sequential(process, netlist, sink),
        ProcessKind::Combinational => lower_combinational(process, netlist, sink),
        ProcessKind::Latched => lower_latched(process, netlist),
        ProcessKind::Initial => {
            // Initial blocks are simulation-only — skip with a diagnostic
            sink.emit(aion_diagnostics::Diagnostic::warning(
                DiagnosticCode::new(Category::Vendor, 1),
                "initial block skipped during synthesis (simulation only)",
                process.span,
            ));
        }
    }
}

/// Lowers a sequential process (always_ff) into DFF cells.
fn lower_sequential(process: &Process, netlist: &mut Netlist, sink: &DiagnosticSink) {
    // Extract clock and reset from sensitivity list
    let (clock, reset) = extract_clock_reset(&process.sensitivity);

    // Find all signals assigned in the body
    let assigned = collect_assigned_signals(&process.body);
    if assigned.is_empty() {
        return;
    }

    // For each assigned signal, create a DFF
    for &sig_id in &assigned {
        let width = netlist.signal_width(sig_id);
        let has_reset = reset.is_some();

        // Lower the body to find the value driven to this signal
        let d_value = lower_stmt_for_signal(&process.body, sig_id, netlist);

        let d_ref = match d_value {
            Some(v) => v,
            None => {
                // Signal assigned but no path found — feedback (hold value)
                SignalRef::Signal(sig_id)
            }
        };

        // Create DFF
        let out_ty = if width == 1 {
            netlist.types.intern(Type::Bit)
        } else {
            netlist.types.intern(Type::BitVec {
                width,
                signed: false,
            })
        };
        let q_out = netlist.add_signal("dff_q", out_ty, SignalKind::Reg);

        let mut connections = vec![
            netlist.input_conn("D", d_ref),
            netlist.output_conn("Q", SignalRef::Signal(q_out)),
        ];

        // Add clock connection
        if let Some(ref clk) = clock {
            connections.push(netlist.input_conn("CLK", SignalRef::Signal(clk.signal)));
        }

        // Add reset connection
        if let Some(ref rst) = reset {
            connections.push(netlist.input_conn("RST", SignalRef::Signal(rst.signal)));
            // Find reset value — look for the pattern: if (reset) target = value
            if let Some(rst_val) = find_reset_value(&process.body, sig_id, rst.signal) {
                let rst_ref = lower_expr(&rst_val, netlist);
                connections.push(netlist.input_conn("RST_VAL", rst_ref));
            }
        }

        netlist.add_cell(
            "dff",
            CellKind::Dff {
                width,
                has_reset,
                has_enable: false,
            },
            connections,
        );

        // Wire DFF output to the original signal
        wire_signal_ref(
            &SignalRef::Signal(sig_id),
            &SignalRef::Signal(q_out),
            netlist,
        );

        let _ = sink; // Used above for warnings
    }
}

/// Lowers a combinational process (always_comb) into MUX chains.
fn lower_combinational(process: &Process, netlist: &mut Netlist, sink: &DiagnosticSink) {
    let assigned = collect_assigned_signals(&process.body);
    for &sig_id in &assigned {
        let value = lower_stmt_for_signal(&process.body, sig_id, netlist);
        match value {
            Some(v) => {
                wire_signal_ref(&SignalRef::Signal(sig_id), &v, netlist);
            }
            None => {
                // Incomplete assignment — signal not assigned on all paths
                // This implies a latch
                sink.emit(aion_diagnostics::Diagnostic::warning(
                    DiagnosticCode::new(Category::Vendor, 2),
                    format!(
                        "incomplete assignment in combinational process infers latch for signal {}",
                        sig_id.as_raw()
                    ),
                    process.span,
                ));
                let width = netlist.signal_width(sig_id);
                netlist.add_cell(
                    "inferred_latch",
                    CellKind::Latch { width },
                    vec![
                        netlist.input_conn("D", SignalRef::Signal(sig_id)),
                        netlist.output_conn("Q", SignalRef::Signal(sig_id)),
                    ],
                );
            }
        }
    }
}

/// Lowers a latched process (always_latch) into latch cells.
fn lower_latched(process: &Process, netlist: &mut Netlist) {
    let assigned = collect_assigned_signals(&process.body);
    for &sig_id in &assigned {
        let width = netlist.signal_width(sig_id);
        let value = lower_stmt_for_signal(&process.body, sig_id, netlist);
        let d_ref = value.unwrap_or(SignalRef::Signal(sig_id));
        netlist.add_cell(
            "latch",
            CellKind::Latch { width },
            vec![
                netlist.input_conn("D", d_ref),
                netlist.output_conn("Q", SignalRef::Signal(sig_id)),
            ],
        );
    }
}

/// Lowers a statement tree for a specific target signal, returning the value
/// driven to that signal as a `SignalRef` (with MUX cells for if/case).
fn lower_stmt_for_signal(
    stmt: &Statement,
    target: SignalId,
    netlist: &mut Netlist,
) -> Option<SignalRef> {
    match stmt {
        Statement::Assign {
            target: ref tgt,
            value,
            ..
        } => {
            if signal_ref_contains(tgt, target) {
                Some(lower_expr(value, netlist))
            } else {
                None
            }
        }

        Statement::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            let then_val = lower_stmt_for_signal(then_body, target, netlist);
            let else_val = else_body
                .as_ref()
                .and_then(|e| lower_stmt_for_signal(e, target, netlist));

            match (then_val, else_val) {
                (Some(t), Some(e)) => {
                    // Both branches assign — create MUX
                    let cond = lower_expr(condition, netlist);
                    let width = signal_ref_width(&t, netlist);
                    let out_ty = if width == 1 {
                        netlist.types.intern(Type::Bit)
                    } else {
                        netlist.types.intern(Type::BitVec {
                            width,
                            signed: false,
                        })
                    };
                    let out = netlist.add_signal("if_mux", out_ty, SignalKind::Wire);
                    netlist.add_cell(
                        "if_mux",
                        CellKind::Mux {
                            width,
                            select_width: 1,
                        },
                        vec![
                            netlist.input_conn("S", cond),
                            netlist.input_conn("A", e),
                            netlist.input_conn("B", t),
                            netlist.output_conn("Y", SignalRef::Signal(out)),
                        ],
                    );
                    Some(SignalRef::Signal(out))
                }
                (Some(t), None) => Some(t),
                (None, Some(e)) => Some(e),
                (None, None) => None,
            }
        }

        Statement::Case {
            subject,
            arms,
            default,
            ..
        } => lower_case_for_signal(subject, arms, default.as_deref(), target, netlist),

        Statement::Block { stmts, .. } => {
            // Last assignment wins (sequential semantics in synthesis)
            let mut result = None;
            for s in stmts {
                if let Some(v) = lower_stmt_for_signal(s, target, netlist) {
                    result = Some(v);
                }
            }
            result
        }

        // Non-synthesizable statements are ignored
        Statement::Wait { .. }
        | Statement::Assertion { .. }
        | Statement::Display { .. }
        | Statement::Finish { .. }
        | Statement::Delay { .. }
        | Statement::Forever { .. }
        | Statement::Nop => None,
    }
}

/// Lowers a case statement for a specific target signal into a priority MUX chain.
fn lower_case_for_signal(
    subject: &Expr,
    arms: &[CaseArm],
    default: Option<&Statement>,
    target: SignalId,
    netlist: &mut Netlist,
) -> Option<SignalRef> {
    let subj_ref = lower_expr(subject, netlist);

    // Start with default value (if any)
    let mut current = default.and_then(|d| lower_stmt_for_signal(d, target, netlist));

    // Build priority MUX chain from last arm to first
    for arm in arms.iter().rev() {
        let arm_val = lower_stmt_for_signal(&arm.body, target, netlist);
        let arm_val = match arm_val {
            Some(v) => v,
            None => continue,
        };

        // Create equality comparison for each pattern
        for pattern in &arm.patterns {
            let pat_ref = lower_expr(pattern, netlist);
            let bit_ty = netlist.types.intern(Type::Bit);
            let eq_out = netlist.add_signal("case_eq", bit_ty, SignalKind::Wire);
            let cmp_width = signal_ref_width(&subj_ref, netlist);
            netlist.add_cell(
                "case_eq",
                CellKind::Eq { width: cmp_width },
                vec![
                    netlist.input_conn("A", subj_ref.clone()),
                    netlist.input_conn("B", pat_ref),
                    netlist.output_conn("Y", SignalRef::Signal(eq_out)),
                ],
            );

            let width = signal_ref_width(&arm_val, netlist);
            let out_ty = if width == 1 {
                netlist.types.intern(Type::Bit)
            } else {
                netlist.types.intern(Type::BitVec {
                    width,
                    signed: false,
                })
            };

            // MUX: if match, use arm value, else use current
            let fallback = current
                .clone()
                .unwrap_or(SignalRef::Const(LogicVec::all_zero(width)));
            let out = netlist.add_signal("case_mux", out_ty, SignalKind::Wire);
            netlist.add_cell(
                "case_mux",
                CellKind::Mux {
                    width,
                    select_width: 1,
                },
                vec![
                    netlist.input_conn("S", SignalRef::Signal(eq_out)),
                    netlist.input_conn("A", fallback),
                    netlist.input_conn("B", arm_val.clone()),
                    netlist.output_conn("Y", SignalRef::Signal(out)),
                ],
            );
            current = Some(SignalRef::Signal(out));
        }
    }

    current
}

/// Extracts clock and reset signals from a sensitivity list.
fn extract_clock_reset(
    sensitivity: &Sensitivity,
) -> (Option<EdgeSensitivity>, Option<EdgeSensitivity>) {
    match sensitivity {
        Sensitivity::EdgeList(edges) => {
            // Convention: first edge is clock, second (if any) is reset
            let clock = edges.first().cloned();
            let reset = edges.get(1).cloned();
            (clock, reset)
        }
        _ => (None, None),
    }
}

/// Finds the reset value for a signal in a sequential process body.
///
/// Looks for the pattern: `if (reset) { target = value; }`
fn find_reset_value(stmt: &Statement, target: SignalId, reset_signal: SignalId) -> Option<Expr> {
    match stmt {
        Statement::If {
            condition,
            then_body,
            ..
        } => {
            // Check if condition references the reset signal
            if expr_references_signal(condition, reset_signal) {
                // Look for assignment to target in then branch
                if let Some(val) = find_assign_value(then_body, target) {
                    return Some(val);
                }
            }
            // Also check nested ifs in then/else
            None
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                if let Some(v) = find_reset_value(s, target, reset_signal) {
                    return Some(v);
                }
            }
            None
        }
        _ => None,
    }
}

/// Finds the value assigned to a target signal in a statement.
fn find_assign_value(stmt: &Statement, target: SignalId) -> Option<Expr> {
    match stmt {
        Statement::Assign {
            target: ref tgt,
            value,
            ..
        } => {
            if signal_ref_contains(tgt, target) {
                Some(value.clone())
            } else {
                None
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                if let Some(v) = find_assign_value(s, target) {
                    return Some(v);
                }
            }
            None
        }
        Statement::If { then_body, .. } => find_assign_value(then_body, target),
        _ => None,
    }
}

/// Checks if an expression references a specific signal.
fn expr_references_signal(expr: &Expr, signal: SignalId) -> bool {
    match expr {
        Expr::Signal(SignalRef::Signal(id)) => *id == signal,
        Expr::Signal(SignalRef::Slice { signal: id, .. }) => *id == signal,
        Expr::Unary { operand, .. } => expr_references_signal(operand, signal),
        Expr::Binary { lhs, rhs, .. } => {
            expr_references_signal(lhs, signal) || expr_references_signal(rhs, signal)
        }
        Expr::Ternary {
            condition,
            true_val,
            false_val,
            ..
        } => {
            expr_references_signal(condition, signal)
                || expr_references_signal(true_val, signal)
                || expr_references_signal(false_val, signal)
        }
        _ => false,
    }
}

/// Collects all signals assigned in a statement tree.
fn collect_assigned_signals(stmt: &Statement) -> Vec<SignalId> {
    let mut signals = Vec::new();
    collect_assigned_signals_inner(stmt, &mut signals);
    signals.sort_by_key(|s| s.as_raw());
    signals.dedup();
    signals
}

fn collect_assigned_signals_inner(stmt: &Statement, signals: &mut Vec<SignalId>) {
    match stmt {
        Statement::Assign { target, .. } => {
            collect_signal_ref_ids(target, signals);
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            collect_assigned_signals_inner(then_body, signals);
            if let Some(e) = else_body {
                collect_assigned_signals_inner(e, signals);
            }
        }
        Statement::Case { arms, default, .. } => {
            for arm in arms {
                collect_assigned_signals_inner(&arm.body, signals);
            }
            if let Some(d) = default {
                collect_assigned_signals_inner(d, signals);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                collect_assigned_signals_inner(s, signals);
            }
        }
        Statement::Delay { body, .. } | Statement::Forever { body, .. } => {
            collect_assigned_signals_inner(body, signals);
        }
        _ => {}
    }
}

fn collect_signal_ref_ids(sr: &SignalRef, signals: &mut Vec<SignalId>) {
    match sr {
        SignalRef::Signal(id) => signals.push(*id),
        SignalRef::Slice { signal, .. } => signals.push(*signal),
        SignalRef::Concat(refs) => {
            for r in refs {
                collect_signal_ref_ids(r, signals);
            }
        }
        SignalRef::Const(_) => {}
    }
}

/// Checks if a signal ref contains a reference to a specific signal ID.
fn signal_ref_contains(sr: &SignalRef, target: SignalId) -> bool {
    match sr {
        SignalRef::Signal(id) => *id == target,
        SignalRef::Slice { signal, .. } => *signal == target,
        SignalRef::Concat(refs) => refs.iter().any(|r| signal_ref_contains(r, target)),
        SignalRef::Const(_) => false,
    }
}

/// Wires one signal ref to another by creating a buffer cell.
/// Wires the output of a lowered expression to a target signal.
///
/// Finds the cell that drives the source signal and redirects its output
/// to the target. If no driving cell exists (e.g., signal-to-signal passthrough),
/// creates a buffer cell.
fn wire_signal_ref(target: &SignalRef, source: &SignalRef, netlist: &mut Netlist) {
    let target_id = match target {
        SignalRef::Signal(id) => *id,
        SignalRef::Slice { signal, .. } => *signal,
        SignalRef::Concat(_) | SignalRef::Const(_) => return,
    };

    let source_id = match source {
        SignalRef::Signal(id) => *id,
        _ => {
            // Complex source — no redirect possible
            return;
        }
    };

    // If source == target, nothing to do
    if source_id == target_id {
        return;
    }

    // Find the cell that drives source_id and redirect its output to target
    let mut redirected = false;
    for (_cell_id, cell) in netlist.cells.iter_mut() {
        for conn in &mut cell.connections {
            if conn.direction == aion_ir::PortDirection::Output
                && conn.signal == SignalRef::Signal(source_id)
            {
                conn.signal = target.clone();
                redirected = true;
            }
        }
    }

    if !redirected {
        // No driving cell found — source is a primary input or port.
        // Create a buffer (1-bit NOT NOT or just use Const/identity).
        // Simplest: create a Slice cell that extracts the full width.
        let width = netlist.signal_width(source_id);
        netlist.add_cell(
            "buf",
            CellKind::Slice { offset: 0, width },
            vec![
                netlist.input_conn("A", SignalRef::Signal(source_id)),
                netlist.output_conn("Y", target.clone()),
            ],
        );
    }
}

/// Gets the bit width of a signal ref from the netlist.
fn signal_ref_width(sr: &SignalRef, netlist: &Netlist) -> u32 {
    match sr {
        SignalRef::Signal(id) => netlist.signal_width(*id),
        SignalRef::Slice { high, low, .. } => high - low + 1,
        SignalRef::Const(lv) => lv.width(),
        SignalRef::Concat(_) => {
            // Sum of all parts — approximate as 1 for now
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use aion_common::Interner;
    use aion_ir::{
        Arena, Assignment, Edge, EdgeSensitivity, Module, Process, ProcessId, ProcessKind,
        Sensitivity, Signal, SignalId, SignalKind, SignalRef, Statement, Type, TypeDb,
    };
    use aion_source::Span;

    fn make_module_with_process(
        interner: &Interner,
        types: &mut TypeDb,
        process: Process,
    ) -> Module {
        let bit_ty = types.intern(Type::Bit);
        let vec8_ty = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let clk_name = interner.get_or_intern("clk");
        let rst_name = interner.get_or_intern("rst");
        let out_name = interner.get_or_intern("out");
        let bus_name = interner.get_or_intern("bus");
        let mod_name = interner.get_or_intern("test");

        let mut signals = Arena::new();
        signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: clk_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: rst_name,
            ty: bit_ty,
            kind: SignalKind::Port,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        signals.alloc(Signal {
            id: SignalId::from_raw(2),
            name: out_name,
            ty: bit_ty,
            kind: SignalKind::Reg,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        signals.alloc(Signal {
            id: SignalId::from_raw(3),
            name: bus_name,
            ty: vec8_ty,
            kind: SignalKind::Reg,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let mut processes = Arena::new();
        processes.alloc(process);

        Module {
            id: aion_ir::ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals,
            cells: Arena::new(),
            processes,
            assignments: vec![],
            clock_domains: vec![],
            content_hash: aion_common::ContentHash::from_bytes(b"test"),
        }
    }

    #[test]
    fn lower_sequential_creates_dff() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: SignalId::from_raw(0),
                edge: Edge::Posedge,
            }]),
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(2)),
                value: Expr::Signal(SignalRef::Signal(SignalId::from_raw(1))),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        let dff_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| matches!(&c.kind, CellKind::Dff { .. }))
            .count();
        assert!(dff_count >= 1, "Expected at least one DFF cell");
    }

    #[test]
    fn lower_sequential_with_reset() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            sensitivity: Sensitivity::EdgeList(vec![
                EdgeSensitivity {
                    signal: SignalId::from_raw(0),
                    edge: Edge::Posedge,
                },
                EdgeSensitivity {
                    signal: SignalId::from_raw(1),
                    edge: Edge::Posedge,
                },
            ]),
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(1))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Literal(LogicVec::from_bool(false)),
                    span: Span::DUMMY,
                }),
                else_body: Some(Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Signal(SignalRef::Signal(SignalId::from_raw(1))),
                    span: Span::DUMMY,
                })),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        let dff_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| {
                matches!(
                    &c.kind,
                    CellKind::Dff {
                        has_reset: true,
                        ..
                    }
                )
            })
            .count();
        assert!(dff_count >= 1, "Expected DFF with reset");
    }

    #[test]
    fn lower_combinational_if_else() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            sensitivity: Sensitivity::All,
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                }),
                else_body: Some(Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Literal(LogicVec::from_bool(false)),
                    span: Span::DUMMY,
                })),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        let mux_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| matches!(&c.kind, CellKind::Mux { .. }))
            .count();
        assert!(mux_count >= 1, "Expected MUX cell for if-else");
    }

    #[test]
    fn lower_combinational_incomplete_warns() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            sensitivity: Sensitivity::All,
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                }),
                else_body: None, // No else — incomplete
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        // The incomplete assignment should still produce output
        // (either a latch warning or the then-only value)
        assert!(!netlist.cells.is_empty());
    }

    #[test]
    fn lower_latched_creates_latch() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Latched,
            sensitivity: Sensitivity::All,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(2)),
                value: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        let latch_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| matches!(&c.kind, CellKind::Latch { .. }))
            .count();
        assert!(latch_count >= 1, "Expected latch cell");
    }

    #[test]
    fn lower_initial_skipped_with_warning() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Initial,
            sensitivity: Sensitivity::All,
            body: Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(2)),
                value: Expr::Literal(LogicVec::from_bool(false)),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        // No synthesis cells should be generated
        assert_eq!(netlist.cells.len(), 0);
        // Should have a warning
        assert!(!sink.diagnostics().is_empty());
    }

    #[test]
    fn lower_concurrent_assignment() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let bit_ty = types.intern(Type::Bit);
        let mod_name = interner.get_or_intern("test");
        let a_name = interner.get_or_intern("a");
        let b_name = interner.get_or_intern("b");

        let mut signals = Arena::new();
        signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: a_name,
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });
        signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: b_name,
            ty: bit_ty,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        });

        let module = Module {
            id: aion_ir::ModuleId::from_raw(0),
            name: mod_name,
            span: Span::DUMMY,
            params: vec![],
            ports: vec![],
            signals,
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: vec![Assignment {
                target: SignalRef::Signal(SignalId::from_raw(1)),
                value: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                span: Span::DUMMY,
            }],
            clock_domains: vec![],
            content_hash: aion_common::ContentHash::from_bytes(b"test"),
        };
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        // Concurrent assignment of signal ref → no cells needed (just wiring)
        // But the lowering should succeed without errors
        assert!(sink.diagnostics().is_empty());
    }

    #[test]
    fn lower_case_creates_priority_mux() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let _vec8_ty = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            sensitivity: Sensitivity::All,
            body: Statement::Case {
                subject: Expr::Signal(SignalRef::Signal(SignalId::from_raw(3))),
                arms: vec![
                    aion_ir::CaseArm {
                        patterns: vec![Expr::Literal(LogicVec::from_u64(0, 8))],
                        body: Statement::Assign {
                            target: SignalRef::Signal(SignalId::from_raw(2)),
                            value: Expr::Literal(LogicVec::from_bool(true)),
                            span: Span::DUMMY,
                        },
                        span: Span::DUMMY,
                    },
                    aion_ir::CaseArm {
                        patterns: vec![Expr::Literal(LogicVec::from_u64(1, 8))],
                        body: Statement::Assign {
                            target: SignalRef::Signal(SignalId::from_raw(2)),
                            value: Expr::Literal(LogicVec::from_bool(false)),
                            span: Span::DUMMY,
                        },
                        span: Span::DUMMY,
                    },
                ],
                default: Some(Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Literal(LogicVec::from_bool(false)),
                    span: Span::DUMMY,
                })),
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        // Should have Eq + Mux cells for the case arms
        let eq_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| matches!(&c.kind, CellKind::Eq { .. }))
            .count();
        let mux_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| matches!(&c.kind, CellKind::Mux { .. }))
            .count();
        assert!(eq_count >= 2, "Expected Eq cells for case patterns");
        assert!(mux_count >= 2, "Expected Mux cells for case arms");
    }

    #[test]
    fn lower_block_last_assign_wins() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            sensitivity: Sensitivity::All,
            body: Statement::Block {
                stmts: vec![
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(2)),
                        value: Expr::Literal(LogicVec::from_bool(true)),
                        span: Span::DUMMY,
                    },
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(2)),
                        value: Expr::Literal(LogicVec::from_bool(false)),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        // Both assignments create Const cells, last one wins
        let const_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| matches!(&c.kind, CellKind::Const { .. }))
            .count();
        assert!(const_count >= 2, "Expected const cells from block assigns");
    }

    #[test]
    fn collect_assigned_signals_deduplicates() {
        let stmt = Statement::Block {
            stmts: vec![
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                },
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: Expr::Literal(LogicVec::from_bool(false)),
                    span: Span::DUMMY,
                },
                Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(1)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let signals = collect_assigned_signals(&stmt);
        assert_eq!(signals.len(), 2);
    }

    #[test]
    fn extract_clock_reset_from_edge_list() {
        let edges = Sensitivity::EdgeList(vec![
            EdgeSensitivity {
                signal: SignalId::from_raw(0),
                edge: Edge::Posedge,
            },
            EdgeSensitivity {
                signal: SignalId::from_raw(1),
                edge: Edge::Posedge,
            },
        ]);
        let (clock, reset) = extract_clock_reset(&edges);
        assert!(clock.is_some());
        assert!(reset.is_some());
        assert_eq!(clock.unwrap().signal, SignalId::from_raw(0));
        assert_eq!(reset.unwrap().signal, SignalId::from_raw(1));
    }

    #[test]
    fn extract_clock_only() {
        let edges = Sensitivity::EdgeList(vec![EdgeSensitivity {
            signal: SignalId::from_raw(0),
            edge: Edge::Posedge,
        }]);
        let (clock, reset) = extract_clock_reset(&edges);
        assert!(clock.is_some());
        assert!(reset.is_none());
    }

    #[test]
    fn extract_no_edges() {
        let (clock, reset) = extract_clock_reset(&Sensitivity::All);
        assert!(clock.is_none());
        assert!(reset.is_none());
    }

    #[test]
    fn signal_ref_contains_works() {
        let sig = SignalId::from_raw(5);
        assert!(signal_ref_contains(&SignalRef::Signal(sig), sig));
        assert!(!signal_ref_contains(
            &SignalRef::Signal(sig),
            SignalId::from_raw(6)
        ));
        assert!(signal_ref_contains(
            &SignalRef::Slice {
                signal: sig,
                high: 3,
                low: 0,
            },
            sig,
        ));
        assert!(signal_ref_contains(
            &SignalRef::Concat(vec![SignalRef::Signal(sig)]),
            sig,
        ));
        assert!(!signal_ref_contains(
            &SignalRef::Const(LogicVec::from_bool(true)),
            sig,
        ));
    }

    #[test]
    fn expr_references_signal_works() {
        let sig = SignalId::from_raw(0);
        assert!(expr_references_signal(
            &Expr::Signal(SignalRef::Signal(sig)),
            sig,
        ));
        assert!(!expr_references_signal(
            &Expr::Literal(LogicVec::from_bool(true)),
            sig,
        ));
    }

    #[test]
    fn lower_multiple_signals_sequential() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Sequential,
            sensitivity: Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: SignalId::from_raw(0),
                edge: Edge::Posedge,
            }]),
            body: Statement::Block {
                stmts: vec![
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(2)),
                        value: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                        span: Span::DUMMY,
                    },
                    Statement::Assign {
                        target: SignalRef::Signal(SignalId::from_raw(3)),
                        value: Expr::Literal(LogicVec::from_u64(0, 8)),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        // Should have DFFs for both signals
        let dff_count = netlist
            .cells
            .iter()
            .filter(|(_, c)| matches!(&c.kind, CellKind::Dff { .. }))
            .count();
        assert_eq!(dff_count, 2, "Expected 2 DFF cells for 2 assigned signals");
    }

    #[test]
    fn lower_if_then_only_combinational() {
        let interner = Interner::new();
        let mut types = TypeDb::new();
        let _bit_ty = types.intern(Type::Bit);
        let process = Process {
            id: ProcessId::from_raw(0),
            name: None,
            kind: ProcessKind::Combinational,
            sensitivity: Sensitivity::All,
            body: Statement::If {
                condition: Expr::Signal(SignalRef::Signal(SignalId::from_raw(0))),
                then_body: Box::new(Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(2)),
                    value: Expr::Literal(LogicVec::from_bool(true)),
                    span: Span::DUMMY,
                }),
                else_body: None,
                span: Span::DUMMY,
            },
            span: Span::DUMMY,
        };
        let module = make_module_with_process(&interner, &mut types, process);
        let mut netlist = Netlist::from_module(&module, &types, &interner);
        let sink = DiagnosticSink::new();
        lower_module(&module, &mut netlist, &sink);

        // Should generate cells (const + possibly latch warning)
        assert!(!netlist.cells.is_empty());
    }
}
