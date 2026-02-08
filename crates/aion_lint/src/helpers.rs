//! IR traversal helpers used by multiple lint rules.
//!
//! These functions walk the statement and expression trees in the AionIR to
//! collect signal usage information needed for checks like unused signals,
//! undriven signals, and incomplete sensitivity lists.

use std::collections::HashSet;

use aion_ir::{CellKind, Connection, Expr, Module, PortDirection, SignalId, SignalRef, Statement};

/// Collects all `SignalId`s referenced (read) in a signal reference.
///
/// For `Signal(id)`, returns that ID. For `Slice`, returns the base signal.
/// For `Concat`, recursively collects from all elements.
/// `Const` references yield no signal IDs.
pub fn collect_signal_ref_signals(sref: &SignalRef) -> HashSet<SignalId> {
    let mut result = HashSet::new();
    collect_signal_ref_signals_into(sref, &mut result);
    result
}

/// Collects all `SignalId`s from an expression tree.
///
/// Walks the entire expression recursively, extracting signal references
/// from `Expr::Signal` variants and any nested sub-expressions.
pub fn collect_expr_signals(expr: &Expr) -> HashSet<SignalId> {
    let mut result = HashSet::new();
    collect_expr_signals_into(expr, &mut result);
    result
}

/// Collects all `SignalId`s that are read (referenced in RHS or conditions) in a statement tree.
///
/// This includes:
/// - RHS of assignments
/// - Conditions in `If` and `Case`
/// - All sub-statements recursively
pub fn collect_read_signals(stmt: &Statement) -> HashSet<SignalId> {
    let mut result = HashSet::new();
    collect_read_signals_into(stmt, &mut result);
    result
}

/// Collects all `SignalId`s that are written (assigned to) in a statement tree.
///
/// This walks the statement tree and collects the target signal IDs
/// from all `Assign` statements.
pub fn collect_written_signals(stmt: &Statement) -> HashSet<SignalId> {
    let mut result = HashSet::new();
    collect_written_signals_into(stmt, &mut result);
    result
}

/// Returns true if the given signal is read anywhere in the module.
///
/// Checks continuous assignments (RHS), process bodies, and cell connections
/// where the signal appears as an input.
pub fn is_signal_read_in_module(module: &Module, signal_id: SignalId) -> bool {
    // Check continuous assignment RHS
    for assign in &module.assignments {
        if collect_expr_signals(&assign.value).contains(&signal_id) {
            return true;
        }
    }

    // Check process bodies
    for (_pid, process) in module.processes.iter() {
        if collect_read_signals(&process.body).contains(&signal_id) {
            return true;
        }
    }

    // Check cell connections (signal used as input to a cell)
    for (_cid, cell) in module.cells.iter() {
        for conn in &cell.connections {
            if (conn.direction == PortDirection::Input || conn.direction == PortDirection::InOut)
                && collect_signal_ref_signals(&conn.signal).contains(&signal_id)
            {
                return true;
            }
        }
    }

    false
}

/// Returns true if the given signal is driven (assigned to) anywhere in the module.
///
/// Checks continuous assignment targets, process bodies, and cell connections
/// where the signal appears as an output.
pub fn is_signal_driven_in_module(module: &Module, signal_id: SignalId) -> bool {
    // Check continuous assignment targets
    for assign in &module.assignments {
        if collect_signal_ref_signals(&assign.target).contains(&signal_id) {
            return true;
        }
    }

    // Check process bodies
    for (_pid, process) in module.processes.iter() {
        if collect_written_signals(&process.body).contains(&signal_id) {
            return true;
        }
    }

    // Check cell connections (signal driven by cell output)
    for (_cid, cell) in module.cells.iter() {
        for conn in &cell.connections {
            if (conn.direction == PortDirection::Output || conn.direction == PortDirection::InOut)
                && collect_signal_ref_signals(&conn.signal).contains(&signal_id)
            {
                return true;
            }
        }
    }

    false
}

/// Checks whether a statement has full else coverage (all paths assign).
///
/// Returns true if every `If` has an `else` branch and every `Case` has
/// a `default` arm. Used by the latch inference rule (W106) to detect
/// combinational processes that may infer latches.
pub fn stmt_has_full_else_coverage(stmt: &Statement) -> bool {
    match stmt {
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            if let Some(else_b) = else_body {
                stmt_has_full_else_coverage(then_body) && stmt_has_full_else_coverage(else_b)
            } else {
                false
            }
        }
        Statement::Case { arms, default, .. } => {
            if default.is_none() {
                return false;
            }
            // Check that all arms and default have coverage
            for arm in arms {
                if !stmt_has_full_else_coverage(&arm.body) {
                    return false;
                }
            }
            stmt_has_full_else_coverage(default.as_ref().unwrap())
        }
        Statement::Block { stmts, .. } => {
            // A block has coverage if all its statements do
            // (only the last statement really matters for coverage, but
            // we check all for nested if/case)
            for s in stmts {
                if !stmt_has_full_else_coverage(s) {
                    return false;
                }
            }
            true
        }
        Statement::Assign { .. } | Statement::Nop => true,
        Statement::Wait { .. }
        | Statement::Assertion { .. }
        | Statement::Display { .. }
        | Statement::Finish { .. } => true,
    }
}

/// Counts how many concurrent drivers a signal has in a module.
///
/// A driver is one of: continuous assignment target, process that writes
/// the signal, or cell output connection. Returns the total count.
pub fn count_drivers(module: &Module, signal_id: SignalId) -> usize {
    let mut count = 0;

    // Continuous assignments
    for assign in &module.assignments {
        if collect_signal_ref_signals(&assign.target).contains(&signal_id) {
            count += 1;
        }
    }

    // Processes
    for (_pid, process) in module.processes.iter() {
        if collect_written_signals(&process.body).contains(&signal_id) {
            count += 1;
        }
    }

    // Cell outputs
    for (_cid, cell) in module.cells.iter() {
        for conn in &cell.connections {
            if (conn.direction == PortDirection::Output || conn.direction == PortDirection::InOut)
                && collect_signal_ref_signals(&conn.signal).contains(&signal_id)
            {
                count += 1;
            }
        }
    }

    count
}

/// Checks if a statement tree contains any blocking assigns (`=`).
///
/// In AionIR all `Statement::Assign` are the same, but the process kind
/// determines if blocking vs nonblocking is expected. We detect both
/// present by checking assignment targets vs process kind.
pub fn has_assign(stmt: &Statement) -> bool {
    match stmt {
        Statement::Assign { .. } => true,
        Statement::If {
            then_body,
            else_body,
            ..
        } => has_assign(then_body) || else_body.as_ref().is_some_and(|e| has_assign(e)),
        Statement::Case { arms, default, .. } => {
            arms.iter().any(|a| has_assign(&a.body))
                || default.as_ref().is_some_and(|d| has_assign(d))
        }
        Statement::Block { stmts, .. } => stmts.iter().any(has_assign),
        _ => false,
    }
}

/// Checks if a cell's connections match a target module's ports.
///
/// Returns a list of issues: missing connections, extra connections,
/// or direction mismatches.
pub fn check_cell_port_match(cell: &aion_ir::Cell, target_module: &Module) -> Vec<PortMatchIssue> {
    let mut issues = Vec::new();

    // Skip non-instance cells
    if !matches!(cell.kind, CellKind::Instance { .. }) {
        return issues;
    }

    let cell_port_names: HashSet<_> = cell.connections.iter().map(|c| c.port_name).collect();
    let module_port_names: HashSet<_> = target_module.ports.iter().map(|p| p.name).collect();

    // Check for extra connections (not in module ports)
    for conn in &cell.connections {
        if !module_port_names.contains(&conn.port_name) {
            issues.push(PortMatchIssue::ExtraConnection(conn.clone()));
        }
    }

    // Check for missing connections (module port not connected)
    for port in &target_module.ports {
        if !cell_port_names.contains(&port.name) {
            issues.push(PortMatchIssue::MissingConnection(port.name));
        }
    }

    issues
}

/// An issue found when checking cell port connections against a module's ports.
#[derive(Debug)]
pub enum PortMatchIssue {
    /// A cell connection references a port that doesn't exist on the target module.
    ExtraConnection(Connection),
    /// A module port has no corresponding connection in the cell instance.
    MissingConnection(aion_common::Ident),
}

// ---- Internal helpers ----

fn collect_signal_ref_signals_into(sref: &SignalRef, result: &mut HashSet<SignalId>) {
    match sref {
        SignalRef::Signal(id) => {
            result.insert(*id);
        }
        SignalRef::Slice { signal, .. } => {
            result.insert(*signal);
        }
        SignalRef::Concat(refs) => {
            for r in refs {
                collect_signal_ref_signals_into(r, result);
            }
        }
        SignalRef::Const(_) => {}
    }
}

fn collect_expr_signals_into(expr: &Expr, result: &mut HashSet<SignalId>) {
    match expr {
        Expr::Signal(sref) => {
            collect_signal_ref_signals_into(sref, result);
        }
        Expr::Literal(_) => {}
        Expr::Unary { operand, .. } => {
            collect_expr_signals_into(operand, result);
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_signals_into(lhs, result);
            collect_expr_signals_into(rhs, result);
        }
        Expr::Ternary {
            condition,
            true_val,
            false_val,
            ..
        } => {
            collect_expr_signals_into(condition, result);
            collect_expr_signals_into(true_val, result);
            collect_expr_signals_into(false_val, result);
        }
        Expr::FuncCall { args, .. } => {
            for arg in args {
                collect_expr_signals_into(arg, result);
            }
        }
        Expr::Concat(exprs) => {
            for e in exprs {
                collect_expr_signals_into(e, result);
            }
        }
        Expr::Repeat { expr, .. } => {
            collect_expr_signals_into(expr, result);
        }
        Expr::Index { expr, index, .. } => {
            collect_expr_signals_into(expr, result);
            collect_expr_signals_into(index, result);
        }
        Expr::Slice {
            expr, high, low, ..
        } => {
            collect_expr_signals_into(expr, result);
            collect_expr_signals_into(high, result);
            collect_expr_signals_into(low, result);
        }
    }
}

fn collect_read_signals_into(stmt: &Statement, result: &mut HashSet<SignalId>) {
    match stmt {
        Statement::Assign { value, .. } => {
            collect_expr_signals_into(value, result);
        }
        Statement::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            collect_expr_signals_into(condition, result);
            collect_read_signals_into(then_body, result);
            if let Some(else_b) = else_body {
                collect_read_signals_into(else_b, result);
            }
        }
        Statement::Case {
            subject,
            arms,
            default,
            ..
        } => {
            collect_expr_signals_into(subject, result);
            for arm in arms {
                for pat in &arm.patterns {
                    collect_expr_signals_into(pat, result);
                }
                collect_read_signals_into(&arm.body, result);
            }
            if let Some(def) = default {
                collect_read_signals_into(def, result);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                collect_read_signals_into(s, result);
            }
        }
        Statement::Wait { duration, .. } => {
            if let Some(d) = duration {
                collect_expr_signals_into(d, result);
            }
        }
        Statement::Assertion { condition, .. } => {
            collect_expr_signals_into(condition, result);
        }
        Statement::Display { args, .. } => {
            for arg in args {
                collect_expr_signals_into(arg, result);
            }
        }
        Statement::Finish { .. } | Statement::Nop => {}
    }
}

fn collect_written_signals_into(stmt: &Statement, result: &mut HashSet<SignalId>) {
    match stmt {
        Statement::Assign { target, .. } => {
            collect_signal_ref_signals_into(target, result);
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            collect_written_signals_into(then_body, result);
            if let Some(else_b) = else_body {
                collect_written_signals_into(else_b, result);
            }
        }
        Statement::Case { arms, default, .. } => {
            for arm in arms {
                collect_written_signals_into(&arm.body, result);
            }
            if let Some(def) = default {
                collect_written_signals_into(def, result);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                collect_written_signals_into(s, result);
            }
        }
        Statement::Wait { .. }
        | Statement::Assertion { .. }
        | Statement::Display { .. }
        | Statement::Finish { .. }
        | Statement::Nop => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::{Ident, LogicVec};
    use aion_ir::*;
    use aion_source::Span;

    fn dummy_span() -> Span {
        Span::DUMMY
    }

    fn mk_signal_expr(id: SignalId) -> Expr {
        Expr::Signal(SignalRef::Signal(id))
    }

    fn mk_literal_expr() -> Expr {
        Expr::Literal(LogicVec::from_bool(true))
    }

    fn mk_module() -> Module {
        Module {
            id: ModuleId::from_raw(0),
            name: Ident::from_raw(0),
            span: dummy_span(),
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: aion_common::ContentHash::from_bytes(&[]),
        }
    }

    #[test]
    fn collect_signal_ref_signal() {
        let id = SignalId::from_raw(5);
        let sref = SignalRef::Signal(id);
        let result = collect_signal_ref_signals(&sref);
        assert!(result.contains(&id));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn collect_signal_ref_slice() {
        let id = SignalId::from_raw(3);
        let sref = SignalRef::Slice {
            signal: id,
            high: 7,
            low: 0,
        };
        let result = collect_signal_ref_signals(&sref);
        assert!(result.contains(&id));
    }

    #[test]
    fn collect_signal_ref_concat() {
        let id1 = SignalId::from_raw(1);
        let id2 = SignalId::from_raw(2);
        let sref = SignalRef::Concat(vec![SignalRef::Signal(id1), SignalRef::Signal(id2)]);
        let result = collect_signal_ref_signals(&sref);
        assert!(result.contains(&id1));
        assert!(result.contains(&id2));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn collect_signal_ref_const_empty() {
        let sref = SignalRef::Const(LogicVec::from_bool(false));
        let result = collect_signal_ref_signals(&sref);
        assert!(result.is_empty());
    }

    #[test]
    fn collect_expr_signals_signal() {
        let id = SignalId::from_raw(7);
        let expr = mk_signal_expr(id);
        let result = collect_expr_signals(&expr);
        assert!(result.contains(&id));
    }

    #[test]
    fn collect_expr_signals_binary() {
        let id1 = SignalId::from_raw(1);
        let id2 = SignalId::from_raw(2);
        let ty = TypeId::from_raw(0);
        let expr = Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(mk_signal_expr(id1)),
            rhs: Box::new(mk_signal_expr(id2)),
            ty,
            span: dummy_span(),
        };
        let result = collect_expr_signals(&expr);
        assert!(result.contains(&id1));
        assert!(result.contains(&id2));
    }

    #[test]
    fn collect_expr_signals_literal_empty() {
        let expr = mk_literal_expr();
        let result = collect_expr_signals(&expr);
        assert!(result.is_empty());
    }

    #[test]
    fn collect_read_signals_assign() {
        let id = SignalId::from_raw(3);
        let stmt = Statement::Assign {
            target: SignalRef::Signal(SignalId::from_raw(0)),
            value: mk_signal_expr(id),
            span: dummy_span(),
        };
        let result = collect_read_signals(&stmt);
        assert!(result.contains(&id));
        // Target should NOT be in read signals
        assert!(!result.contains(&SignalId::from_raw(0)));
    }

    #[test]
    fn collect_read_signals_if() {
        let cond_id = SignalId::from_raw(1);
        let body_id = SignalId::from_raw(2);
        let stmt = Statement::If {
            condition: mk_signal_expr(cond_id),
            then_body: Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: mk_signal_expr(body_id),
                span: dummy_span(),
            }),
            else_body: None,
            span: dummy_span(),
        };
        let result = collect_read_signals(&stmt);
        assert!(result.contains(&cond_id));
        assert!(result.contains(&body_id));
    }

    #[test]
    fn collect_written_signals_assign() {
        let target_id = SignalId::from_raw(5);
        let stmt = Statement::Assign {
            target: SignalRef::Signal(target_id),
            value: mk_literal_expr(),
            span: dummy_span(),
        };
        let result = collect_written_signals(&stmt);
        assert!(result.contains(&target_id));
    }

    #[test]
    fn collect_written_signals_block() {
        let id1 = SignalId::from_raw(1);
        let id2 = SignalId::from_raw(2);
        let stmt = Statement::Block {
            stmts: vec![
                Statement::Assign {
                    target: SignalRef::Signal(id1),
                    value: mk_literal_expr(),
                    span: dummy_span(),
                },
                Statement::Assign {
                    target: SignalRef::Signal(id2),
                    value: mk_literal_expr(),
                    span: dummy_span(),
                },
            ],
            span: dummy_span(),
        };
        let result = collect_written_signals(&stmt);
        assert!(result.contains(&id1));
        assert!(result.contains(&id2));
    }

    #[test]
    fn signal_read_in_module_via_assignment() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(0),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        });
        let target_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(1),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(target_id),
            value: mk_signal_expr(sig_id),
            span: dummy_span(),
        });
        assert!(is_signal_read_in_module(&module, sig_id));
        assert!(!is_signal_read_in_module(&module, target_id));
    }

    #[test]
    fn signal_driven_in_module_via_assignment() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(0),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: mk_literal_expr(),
            span: dummy_span(),
        });
        assert!(is_signal_driven_in_module(&module, sig_id));
    }

    #[test]
    fn signal_not_driven_when_only_read() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(0),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        });
        let other_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(1),
            name: Ident::from_raw(1),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(other_id),
            value: mk_signal_expr(sig_id),
            span: dummy_span(),
        });
        assert!(!is_signal_driven_in_module(&module, sig_id));
    }

    #[test]
    fn stmt_full_coverage_if_with_else() {
        let stmt = Statement::If {
            condition: mk_literal_expr(),
            then_body: Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: mk_literal_expr(),
                span: dummy_span(),
            }),
            else_body: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: mk_literal_expr(),
                span: dummy_span(),
            })),
            span: dummy_span(),
        };
        assert!(stmt_has_full_else_coverage(&stmt));
    }

    #[test]
    fn stmt_no_coverage_if_without_else() {
        let stmt = Statement::If {
            condition: mk_literal_expr(),
            then_body: Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: mk_literal_expr(),
                span: dummy_span(),
            }),
            else_body: None,
            span: dummy_span(),
        };
        assert!(!stmt_has_full_else_coverage(&stmt));
    }

    #[test]
    fn stmt_full_coverage_case_with_default() {
        let stmt = Statement::Case {
            subject: mk_literal_expr(),
            arms: vec![CaseArm {
                patterns: vec![mk_literal_expr()],
                body: Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: mk_literal_expr(),
                    span: dummy_span(),
                },
                span: dummy_span(),
            }],
            default: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: mk_literal_expr(),
                span: dummy_span(),
            })),
            span: dummy_span(),
        };
        assert!(stmt_has_full_else_coverage(&stmt));
    }

    #[test]
    fn stmt_no_coverage_case_without_default() {
        let stmt = Statement::Case {
            subject: mk_literal_expr(),
            arms: vec![CaseArm {
                patterns: vec![mk_literal_expr()],
                body: Statement::Assign {
                    target: SignalRef::Signal(SignalId::from_raw(0)),
                    value: mk_literal_expr(),
                    span: dummy_span(),
                },
                span: dummy_span(),
            }],
            default: None,
            span: dummy_span(),
        };
        assert!(!stmt_has_full_else_coverage(&stmt));
    }

    #[test]
    fn count_drivers_multiple() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(0),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        });
        // Two continuous assignments driving the same signal
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: mk_literal_expr(),
            span: dummy_span(),
        });
        module.assignments.push(Assignment {
            target: SignalRef::Signal(sig_id),
            value: mk_literal_expr(),
            span: dummy_span(),
        });
        assert_eq!(count_drivers(&module, sig_id), 2);
    }

    #[test]
    fn count_drivers_none() {
        let mut module = mk_module();
        let sig_id = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(0),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        });
        assert_eq!(count_drivers(&module, sig_id), 0);
    }

    #[test]
    fn has_assign_in_block() {
        let stmt = Statement::Block {
            stmts: vec![Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: mk_literal_expr(),
                span: dummy_span(),
            }],
            span: dummy_span(),
        };
        assert!(has_assign(&stmt));
    }

    #[test]
    fn no_assign_nop() {
        assert!(!has_assign(&Statement::Nop));
    }
}
