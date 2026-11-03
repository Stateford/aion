//! IR traversal helpers used by multiple lint rules.
//!
//! These functions walk the statement and expression trees in the AionIR to
//! collect signal usage information needed for checks like unused signals,
//! undriven signals, and incomplete sensitivity lists.
//!
//! It also hosts the single, shared bit-width computation used by the
//! width-mismatch (W103) and truncation (W107) rules. All width arithmetic is
//! checked: ranges of either direction, overflowing concatenations or
//! replications, and placeholder types yield `None` ("width unknown") rather
//! than panicking or wrapping around.

use std::collections::HashSet;

use aion_ir::ids::TypeId;
use aion_ir::{
    CellKind, Connection, Design, Expr, Module, PortDirection, Sensitivity, SignalId, SignalRef,
    Statement, Type, TypeDb,
};
use aion_source::Span;

use crate::context::is_ident_byte;
use crate::LintContext;

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
/// Checks continuous assignments (RHS), process bodies, process sensitivity
/// lists (a signal used only as a clock or asynchronous reset in
/// `@(posedge gclk)`, or listed in a VHDL `process (a, b)`, is still read),
/// and cell connections where the signal appears as an input.
pub fn is_signal_read_in_module(module: &Module, signal_id: SignalId) -> bool {
    // Check continuous assignment RHS
    for assign in &module.assignments {
        if collect_expr_signals(&assign.value).contains(&signal_id) {
            return true;
        }
    }

    // Check process bodies and sensitivity lists
    for (_pid, process) in module.processes.iter() {
        let in_sensitivity = match &process.sensitivity {
            Sensitivity::All => false,
            Sensitivity::EdgeList(edges) => edges.iter().any(|e| e.signal == signal_id),
            Sensitivity::SignalList(signals) => signals.contains(&signal_id),
        };
        if in_sensitivity || collect_read_signals(&process.body).contains(&signal_id) {
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
        Statement::Delay { body, .. } | Statement::Forever { body, .. } => {
            stmt_has_full_else_coverage(body)
        }
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
        Statement::Delay { body, .. } | Statement::Forever { body, .. } => has_assign(body),
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

/// Computes the number of bits covered by the inclusive range between `high`
/// and `low`, regardless of range direction.
///
/// Descending ranges (`[7:0]`), ascending ranges (Verilog `[0:7]`, VHDL
/// `0 to 7`), and single-bit ranges (`[3:3]`) are all handled: the width is
/// `|high - low| + 1`. Returns `None` if the width does not fit in a `u32`
/// (e.g. `[u64::MAX:0]`), so callers treat it as unknown instead of
/// overflowing.
pub fn range_width(high: u64, low: u64) -> Option<u32> {
    let span = high.abs_diff(low).checked_add(1)?;
    u32::try_from(span).ok()
}

/// Returns the bit width of the type `ty` in `types`, using checked arithmetic.
///
/// Mirrors [`TypeDb::bit_width`] but never panics: an out-of-range `TypeId`
/// yields `None`, and an array whose total width (`element width * size`)
/// overflows a `u32` yields `None`. Types without a fixed width (integers,
/// reals, strings, enums, records, error types) also yield `None`.
pub fn type_width(types: &TypeDb, ty: TypeId) -> Option<u32> {
    if ty.as_raw() as usize >= types.len() {
        return None;
    }
    match types.get(ty) {
        Type::Bit | Type::Bool => Some(1),
        Type::BitVec { width, .. } => Some(*width),
        Type::Array { element, size } => type_width(types, *element)?.checked_mul(*size),
        _ => None,
    }
}

/// Returns the bit width of an assignment target (`SignalRef`), if it can be
/// determined from the module's signal types.
///
/// Whole-signal references use the signal's declared type; slices use
/// [`range_width`] so ascending and descending ranges are both supported;
/// concatenations sum their parts with overflow checking; constants use
/// their literal width. Returns `None` whenever any part is unknown or the
/// total overflows a `u32`.
pub fn signal_ref_width(sref: &SignalRef, module: &Module, design: &Design) -> Option<u32> {
    match sref {
        SignalRef::Signal(id) => {
            let signal = module.signals.get(*id);
            type_width(&design.types, signal.ty)
        }
        SignalRef::Slice { high, low, .. } => range_width(u64::from(*high), u64::from(*low)),
        SignalRef::Concat(refs) => refs.iter().try_fold(0u32, |total, r| {
            total.checked_add(signal_ref_width(r, module, design)?)
        }),
        SignalRef::Const(lv) => Some(lv.width()),
    }
}

/// Returns the bit width of an expression, if it can be determined without
/// module context, using checked arithmetic throughout.
///
/// This is the shared width oracle for the W103 (width-mismatch) and W107
/// (truncation) rules. It returns `None` ("unknown, do not warn") for:
///
/// - whole-signal and concatenated-signal references (no module context);
/// - 1-bit all-zero / all-one literals, which represent context-dependent
///   VHDL `(others => '0')` / `(others => '1')` aggregates;
/// - named constants (parameters, generics): an untyped parameter is 32
///   bits by the LRM, so `state <= IDLE` would otherwise always look like a
///   mismatch, although the value fits;
/// - unary, binary, ternary, and function-call expressions stamped with the
///   `TypeId::from_raw(0)` placeholder, which the elaborator uses for
///   unresolved result types and which may alias an unrelated real type;
/// - slices whose bounds are not literals, or exceed 64 bits;
/// - concatenations or replications whose total width overflows a `u32`.
pub fn expr_width(expr: &Expr, design: &Design) -> Option<u32> {
    match expr {
        Expr::Signal(sref) => match sref {
            SignalRef::Slice { high, low, .. } => range_width(u64::from(*high), u64::from(*low)),
            SignalRef::Const(lv) => Some(lv.width()),
            // Would need module context to resolve signal types.
            SignalRef::Signal(_) | SignalRef::Concat(_) => None,
        },
        Expr::Literal(lv) => {
            if lv.width() == 1 && (lv.is_all_zero() || lv.is_all_one()) {
                return None;
            }
            Some(lv.width())
        }
        Expr::NamedConst { .. } => None,
        Expr::Unary { ty, .. }
        | Expr::Binary { ty, .. }
        | Expr::Ternary { ty, .. }
        | Expr::FuncCall { ty, .. } => {
            if *ty == TypeId::from_raw(0) {
                return None;
            }
            type_width(&design.types, *ty)
        }
        Expr::Concat(exprs) => exprs
            .iter()
            .try_fold(0u32, |total, e| total.checked_add(expr_width(e, design)?)),
        Expr::Repeat { expr, count, .. } => expr_width(expr, design)?.checked_mul(*count),
        Expr::Index { .. } => Some(1),
        Expr::Slice { high, low, .. } => match (high.as_ref(), low.as_ref()) {
            (
                Expr::Literal(h) | Expr::NamedConst { value: h, .. },
                Expr::Literal(l) | Expr::NamedConst { value: l, .. },
            ) => range_width(h.to_u64()?, l.to_u64()?),
            _ => None,
        },
    }
}

/// Returns true if `module` is a second (or later) parameter specialization
/// of the same source declaration as an earlier module in `design`.
///
/// Elaboration creates one IR module per distinct parameterization, and all
/// of them share the declaration's name and span. Rules that check the
/// declaration itself (its name, its doc comment) use this to report once per
/// declaration rather than once per specialization.
pub(crate) fn is_duplicate_specialization(module: &Module, design: &Design) -> bool {
    if module.span.is_dummy() {
        return false;
    }
    design
        .modules
        .values()
        .find(|m| m.name == module.name && m.span == module.span)
        .is_some_and(|first| !std::ptr::eq(first, module))
}

/// Returns the identifiers that appear inside an index on the left-hand side
/// of an assignment whose IR target lost that index.
///
/// AionIR has no dynamic-index target: elaboration lowers `mem[addr] <= d`
/// (Verilog) or `mem(addr) <= d` (VHDL) to a whole-signal target, so `addr`
/// disappears from the IR even though the assignment reads it. For every
/// assignment whose target contains a whole-signal reference, this inspects
/// the source text of the statement, takes the part before the assignment
/// operator, and collects the identifiers written inside an index bracket:
/// any `[...]`, or a `(...)` that directly follows a name (`mem(addr)`).
/// Concatenation braces `{...}` and a VHDL aggregate target `(carry, s)` are
/// *not* index brackets: the signals they list are written, not read, and
/// must stay eligible for W101. Literal text is skipped too, so the base
/// letter and digits of a Verilog sized literal (`d1` in `8'd1`), a VHDL
/// attribute name (`length` in `x'length`) or the contents of a string are
/// never taken for signal names.
///
/// Names are compared the way the source language does: identifiers from
/// VHDL files are folded to lowercase (matching the interned VHDL names),
/// while Verilog/SystemVerilog identifiers are kept exactly as written, since
/// `Addr` and `addr` are different signals there.
///
/// The result is only used to *suppress* unused-signal warnings (W101), so
/// every name it returns hides a potential warning. It therefore only takes
/// identifiers that really are read: those inside an index or part-select
/// bracket. Extra names that are not signals (a function or parameter used
/// inside the index) match nothing and are harmless. Assignments without
/// source text contribute nothing.
pub(crate) fn dynamic_index_reads(module: &Module, ctx: &LintContext<'_>) -> HashSet<String> {
    let mut names = HashSet::new();
    for assign in &module.assignments {
        add_dynamic_index_reads(&assign.target, assign.span, ctx, &mut names);
    }
    for (_pid, process) in module.processes.iter() {
        for_each_assign(&process.body, &mut |target, span| {
            add_dynamic_index_reads(target, span, ctx, &mut names);
        });
    }
    names
}

/// Calls `f` with the target and span of every `Assign` in `stmt`.
fn for_each_assign(stmt: &Statement, f: &mut dyn FnMut(&SignalRef, Span)) {
    match stmt {
        Statement::Assign { target, span, .. } => f(target, *span),
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            for_each_assign(then_body, f);
            if let Some(e) = else_body {
                for_each_assign(e, f);
            }
        }
        Statement::Case { arms, default, .. } => {
            for arm in arms {
                for_each_assign(&arm.body, f);
            }
            if let Some(d) = default {
                for_each_assign(d, f);
            }
        }
        Statement::Block { stmts, .. } => {
            for s in stmts {
                for_each_assign(s, f);
            }
        }
        Statement::Delay { body, .. } | Statement::Forever { body, .. } => {
            for_each_assign(body, f);
        }
        Statement::Wait { .. }
        | Statement::Assertion { .. }
        | Statement::Display { .. }
        | Statement::Finish { .. }
        | Statement::Nop => {}
    }
}

/// Returns true if `sref` is, or contains, a whole-signal reference — the
/// form a dynamically indexed target is lowered to.
fn has_whole_signal(sref: &SignalRef) -> bool {
    match sref {
        SignalRef::Signal(_) => true,
        SignalRef::Concat(parts) => parts.iter().any(has_whole_signal),
        SignalRef::Slice { .. } | SignalRef::Const(_) => false,
    }
}

fn add_dynamic_index_reads(
    target: &SignalRef,
    span: Span,
    ctx: &LintContext<'_>,
    names: &mut HashSet<String>,
) {
    if !has_whole_signal(target) {
        return;
    }
    let Some(text) = ctx.source_text(span) else {
        return;
    };
    let vhdl = ctx.is_vhdl_source(span);
    let lhs = assignment_lhs(text);
    let bytes = lhs.as_bytes();
    // One entry per open bracket: `true` if it encloses (part of) an index.
    let mut open: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let in_index = open.last() == Some(&true);
        match bytes[i] {
            b'[' => open.push(true),
            // `mem(addr)` indexes `mem`; `(carry, s)` opening the target, or
            // a `(` after `,`/`(`/an operator, groups or aggregates instead.
            // Inside an index every bracket is part of the index expression.
            b'(' => open.push(in_index || follows_name(bytes, i)),
            b']' | b')' => {
                open.pop();
            }
            b'"' => {
                i = skip_string(bytes, i, vhdl);
                continue;
            }
            b'\'' => {
                i = if vhdl {
                    skip_vhdl_tick(bytes, i)
                } else {
                    skip_verilog_based_literal(bytes, i)
                };
                continue;
            }
            // VHDL based literal `16#FF#`: its digits are not names.
            b'#' if vhdl => {
                i = bytes[i + 1..]
                    .iter()
                    .position(|&c| c == b'#')
                    .map_or(bytes.len(), |p| i + p + 2);
                continue;
            }
            b if is_ident_start(b) && (i == 0 || !is_ident_byte(bytes[i - 1])) => {
                let start = i;
                while i < bytes.len() && is_ident_byte(bytes[i]) {
                    i += 1;
                }
                // `x"FF"` in VHDL: the identifier is a bit-string base.
                let bit_string_base = vhdl && bytes.get(i) == Some(&b'"');
                if in_index && !bit_string_base {
                    let ident = &lhs[start..i];
                    names.insert(if vhdl {
                        ident.to_ascii_lowercase()
                    } else {
                        ident.to_string()
                    });
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}

/// Returns true if the `(` at `bytes[open]` directly follows a name — an
/// identifier character, `)` or `]`, ignoring whitespace — and so starts an
/// index or call (`mem(addr)`, `f(x)(y)`) rather than a parenthesised group
/// or aggregate.
fn follows_name(bytes: &[u8], open: usize) -> bool {
    bytes[..open]
        .iter()
        .rev()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|&b| is_ident_byte(b) || b == b')' || b == b']')
}

/// Returns the index just past the string literal starting with the `"` at
/// `bytes[start]` (or the end of `bytes` if it is unterminated). Verilog
/// strings use `\` escapes; VHDL doubles the quote instead, which scans as
/// two adjacent strings and needs no special case.
fn skip_string(bytes: &[u8], start: usize, vhdl: bool) -> usize {
    let mut j = start + 1;
    while j < bytes.len() && bytes[j] != b'"' {
        if !vhdl && bytes[j] == b'\\' {
            j += 1;
        }
        j += 1;
    }
    (j + 1).min(bytes.len())
}

/// Returns the index just past the Verilog/SystemVerilog based-literal tail
/// that starts with the `'` at `bytes[tick]`: an optional signedness `s`, a
/// base letter (`b`, `o`, `d`, `h`, any case) and its digits, so that the
/// `d1` of `8'd1` or the `hA` of `4'hA` is not read as a signal name.
///
/// Anything else after the `'` (an SV cast `'(`, a fill literal `'0`) only
/// skips the `'` itself.
fn skip_verilog_based_literal(bytes: &[u8], tick: usize) -> usize {
    let mut j = tick + 1;
    if matches!(bytes.get(j), Some(b's' | b'S')) {
        j += 1;
    }
    if !matches!(
        bytes.get(j),
        Some(b'b' | b'B' | b'o' | b'O' | b'd' | b'D' | b'h' | b'H')
    ) {
        return tick + 1;
    }
    j += 1;
    while bytes.get(j).is_some_and(|b| b.is_ascii_whitespace()) {
        j += 1;
    }
    while bytes.get(j).is_some_and(|&b| {
        b.is_ascii_hexdigit() || matches!(b, b'_' | b'x' | b'X' | b'z' | b'Z' | b'?')
    }) {
        j += 1;
    }
    j
}

/// Returns the index just past the VHDL construct that starts with the `'`
/// at `bytes[tick]`: a character literal (`'1'`) or an attribute name
/// (`'length` in `x'length`). Neither is a signal. Anything else (the `'` of
/// a qualified expression `t'(...)`) only skips the `'` itself.
fn skip_vhdl_tick(bytes: &[u8], tick: usize) -> usize {
    if bytes.get(tick + 2) == Some(&b'\'') {
        return tick + 3;
    }
    let mut j = tick + 1;
    if bytes.get(j).is_some_and(|&b| is_ident_start(b)) {
        while bytes.get(j).is_some_and(|&b| is_ident_byte(b)) {
            j += 1;
        }
    }
    j
}

/// Returns true if `b` can start an HDL identifier.
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Returns the text before the first assignment operator (`<=`, `:=`, or a
/// lone `=`) that is not nested inside brackets, or all of `text` if there
/// is none.
fn assignment_lhs(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' | b'(' | b'{' => depth += 1,
            b']' | b')' | b'}' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            b'<' | b':' if bytes.get(i + 1) == Some(&b'=') => return &text[..i],
            b'=' => {
                let prev = i.checked_sub(1).map(|p| bytes[p]);
                let next = bytes.get(i + 1).copied();
                let prev_is_op = matches!(prev, Some(b'<' | b'>' | b'!' | b'=' | b':'));
                if !prev_is_op && next != Some(b'=') {
                    return &text[..i];
                }
            }
            _ => {}
        }
    }
    text
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
        Expr::Literal(_) | Expr::NamedConst { .. } => {}
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
        Statement::Delay { body, .. } | Statement::Forever { body, .. } => {
            collect_read_signals_into(body, result);
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
        Statement::Delay { body, .. } | Statement::Forever { body, .. } => {
            collect_written_signals_into(body, result);
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
            kind: aion_ir::AssignKind::Blocking,
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
                kind: aion_ir::AssignKind::Blocking,
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
            kind: aion_ir::AssignKind::Blocking,
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
                    kind: aion_ir::AssignKind::Blocking,
                    span: dummy_span(),
                },
                Statement::Assign {
                    target: SignalRef::Signal(id2),
                    value: mk_literal_expr(),
                    kind: aion_ir::AssignKind::Blocking,
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
                kind: aion_ir::AssignKind::Blocking,
                span: dummy_span(),
            }),
            else_body: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: mk_literal_expr(),
                kind: aion_ir::AssignKind::Blocking,
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
                kind: aion_ir::AssignKind::Blocking,
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
                    kind: aion_ir::AssignKind::Blocking,
                    span: dummy_span(),
                },
                span: dummy_span(),
            }],
            default: Some(Box::new(Statement::Assign {
                target: SignalRef::Signal(SignalId::from_raw(0)),
                value: mk_literal_expr(),
                kind: aion_ir::AssignKind::Blocking,
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
                    kind: aion_ir::AssignKind::Blocking,
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
                kind: aion_ir::AssignKind::Blocking,
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

    // ---- Width helpers (issue #38) ----

    fn mk_design_with(module: Module, types: TypeDb) -> Design {
        let mut modules = Arena::new();
        let top = modules.alloc(module);
        Design {
            modules,
            top,
            types,
            source_map: SourceMap::new(),
        }
    }

    fn lit(value: u64, width: u32) -> Expr {
        Expr::Literal(LogicVec::from_u64(value, width))
    }

    fn binary(ty: TypeId) -> Expr {
        Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(lit(1, 8)),
            rhs: Box::new(lit(2, 8)),
            ty,
            span: dummy_span(),
        }
    }

    fn slice_expr(high: u64, low: u64) -> Expr {
        Expr::Slice {
            expr: Box::new(mk_signal_expr(SignalId::from_raw(0))),
            high: Box::new(lit(high, 32)),
            low: Box::new(lit(low, 32)),
            span: dummy_span(),
        }
    }

    #[test]
    fn range_width_descending() {
        assert_eq!(range_width(7, 0), Some(8));
        assert_eq!(range_width(15, 8), Some(8));
    }

    #[test]
    fn range_width_ascending() {
        // Verilog `[0:3]`, VHDL `0 to 3`, and the `[0 +: 8]` lowering.
        assert_eq!(range_width(0, 3), Some(4));
        assert_eq!(range_width(0, 8), Some(9));
    }

    #[test]
    fn range_width_equal_bounds() {
        assert_eq!(range_width(0, 0), Some(1));
        assert_eq!(range_width(5, 5), Some(1));
    }

    #[test]
    fn range_width_overflow_is_none() {
        assert_eq!(range_width(u64::MAX, 0), None);
        assert_eq!(range_width(0, u64::MAX), None);
        // Exactly u32::MAX bits fits; one more does not.
        assert_eq!(range_width(u64::from(u32::MAX) - 1, 0), Some(u32::MAX));
        assert_eq!(range_width(u64::from(u32::MAX), 0), None);
    }

    #[test]
    fn type_width_basic_types() {
        let mut types = TypeDb::new();
        let bit = types.intern(Type::Bit);
        let v8 = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let int = types.intern(Type::Integer);
        assert_eq!(type_width(&types, bit), Some(1));
        assert_eq!(type_width(&types, v8), Some(8));
        assert_eq!(type_width(&types, int), None);
    }

    #[test]
    fn type_width_array_overflow_and_out_of_range() {
        let mut types = TypeDb::new();
        let v8 = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let small = types.intern(Type::Array {
            element: v8,
            size: 4,
        });
        let huge = types.intern(Type::Array {
            element: v8,
            size: u32::MAX,
        });
        assert_eq!(type_width(&types, small), Some(32));
        assert_eq!(type_width(&types, huge), None);
        assert_eq!(type_width(&types, TypeId::from_raw(1000)), None);
    }

    #[test]
    fn signal_ref_width_slices_both_directions() {
        let module = mk_module();
        let design = mk_design_with(mk_module(), TypeDb::new());
        let desc = SignalRef::Slice {
            signal: SignalId::from_raw(0),
            high: 7,
            low: 4,
        };
        let asc = SignalRef::Slice {
            signal: SignalId::from_raw(0),
            high: 0,
            low: 3,
        };
        assert_eq!(signal_ref_width(&desc, &module, &design), Some(4));
        assert_eq!(signal_ref_width(&asc, &module, &design), Some(4));
    }

    #[test]
    fn signal_ref_width_signal_and_concat() {
        let mut module = mk_module();
        let mut types = TypeDb::new();
        let v8 = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        let sig = module.signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(1),
            ty: v8,
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        });
        let design = mk_design_with(mk_module(), types);
        let cat = SignalRef::Concat(vec![
            SignalRef::Signal(sig),
            SignalRef::Slice {
                signal: sig,
                high: 0,
                low: 1,
            },
        ]);
        assert_eq!(signal_ref_width(&cat, &module, &design), Some(10));
    }

    #[test]
    fn signal_ref_width_concat_overflow_is_none() {
        let module = mk_module();
        let design = mk_design_with(mk_module(), TypeDb::new());
        let max = SignalRef::Slice {
            signal: SignalId::from_raw(0),
            high: u32::MAX - 1,
            low: 0,
        };
        assert_eq!(signal_ref_width(&max, &module, &design), Some(u32::MAX));
        let cat = SignalRef::Concat(vec![max.clone(), max]);
        assert_eq!(signal_ref_width(&cat, &module, &design), None);
        // u32::MAX:0 is 2^32 bits wide, which itself does not fit.
        let too_wide = SignalRef::Slice {
            signal: SignalId::from_raw(0),
            high: u32::MAX,
            low: 0,
        };
        assert_eq!(signal_ref_width(&too_wide, &module, &design), None);
    }

    #[test]
    fn expr_width_slice_both_directions() {
        let design = mk_design_with(mk_module(), TypeDb::new());
        assert_eq!(expr_width(&slice_expr(7, 0), &design), Some(8));
        assert_eq!(expr_width(&slice_expr(0, 3), &design), Some(4));
        assert_eq!(expr_width(&slice_expr(2, 2), &design), Some(1));
        // A slice bound so large the width exceeds u32 is unknown, not wrapped.
        let huge = Expr::Slice {
            expr: Box::new(mk_signal_expr(SignalId::from_raw(0))),
            high: Box::new(lit(u64::MAX, 64)),
            low: Box::new(lit(0, 64)),
            span: dummy_span(),
        };
        assert_eq!(expr_width(&huge, &design), None);
    }

    #[test]
    fn expr_width_non_literal_slice_bounds_unknown() {
        let design = mk_design_with(mk_module(), TypeDb::new());
        let e = Expr::Slice {
            expr: Box::new(mk_signal_expr(SignalId::from_raw(0))),
            high: Box::new(mk_signal_expr(SignalId::from_raw(1))),
            low: Box::new(lit(0, 32)),
            span: dummy_span(),
        };
        assert_eq!(expr_width(&e, &design), None);
    }

    #[test]
    fn expr_width_named_constant() {
        let design = mk_design_with(mk_module(), TypeDb::new());
        let named = |v| Expr::NamedConst {
            name: Ident::from_raw(1),
            value: LogicVec::from_u64(v, 32),
        };
        // A parameter's width is context-dependent: never a mismatch alone.
        assert_eq!(expr_width(&named(1), &design), None);
        // But a parameter is a known constant slice bound (`a[MSB:0]`).
        let e = Expr::Slice {
            expr: Box::new(mk_signal_expr(SignalId::from_raw(0))),
            high: Box::new(named(7)),
            low: Box::new(lit(0, 32)),
            span: dummy_span(),
        };
        assert_eq!(expr_width(&e, &design), Some(8));
        // ...and a parameter reads no signals.
        assert!(collect_expr_signals(&named(3)).is_empty());
    }

    #[test]
    fn expr_width_concat_and_repeat() {
        let design = mk_design_with(mk_module(), TypeDb::new());
        let cat = Expr::Concat(vec![lit(0xA, 4), lit(0xBC, 8)]);
        assert_eq!(expr_width(&cat, &design), Some(12));
        let rep = Expr::Repeat {
            expr: Box::new(lit(0xFF, 8)),
            count: 4,
            span: dummy_span(),
        };
        assert_eq!(expr_width(&rep, &design), Some(32));
    }

    #[test]
    fn expr_width_repeat_overflow_is_none() {
        let design = mk_design_with(mk_module(), TypeDb::new());
        let rep = Expr::Repeat {
            expr: Box::new(lit(0xFF, 8)),
            count: 1_000_000_000,
            span: dummy_span(),
        };
        assert_eq!(expr_width(&rep, &design), None);
    }

    #[test]
    fn expr_width_concat_overflow_is_none() {
        let design = mk_design_with(mk_module(), TypeDb::new());
        let big = Expr::Repeat {
            expr: Box::new(lit(0xF, 4)),
            count: u32::MAX / 4,
            span: dummy_span(),
        };
        assert_eq!(expr_width(&big, &design), Some(u32::MAX - 3));
        let cat = Expr::Concat(vec![big, lit(0xFF, 8)]);
        assert_eq!(expr_width(&cat, &design), None);
    }

    #[test]
    fn expr_width_placeholder_type_is_none() {
        // TypeId(0) aliases a real 8-bit type here, exactly as when the first
        // port of a module is 8 bits wide; it must still be treated as unknown.
        let mut types = TypeDb::new();
        let v8 = types.intern(Type::BitVec {
            width: 8,
            signed: false,
        });
        assert_eq!(v8, TypeId::from_raw(0));
        let v4 = types.intern(Type::BitVec {
            width: 4,
            signed: false,
        });
        let design = mk_design_with(mk_module(), types);
        assert_eq!(expr_width(&binary(TypeId::from_raw(0)), &design), None);
        assert_eq!(expr_width(&binary(v4), &design), Some(4));
    }

    #[test]
    fn expr_width_placeholder_type_with_empty_typedb() {
        // An empty TypeDb would panic in `TypeDb::get`; the guard and the
        // bounds check must both prevent that.
        let design = mk_design_with(mk_module(), TypeDb::new());
        assert_eq!(expr_width(&binary(TypeId::from_raw(0)), &design), None);
        assert_eq!(expr_width(&binary(TypeId::from_raw(7)), &design), None);
    }

    #[test]
    fn expr_width_literals_and_signals() {
        let design = mk_design_with(mk_module(), TypeDb::new());
        assert_eq!(expr_width(&lit(0xA5, 8), &design), Some(8));
        // 1-bit all-zero/all-one literals are context-dependent aggregates.
        assert_eq!(
            expr_width(&Expr::Literal(LogicVec::all_zero(1)), &design),
            None
        );
        assert_eq!(
            expr_width(&mk_signal_expr(SignalId::from_raw(0)), &design),
            None
        );
        let idx = Expr::Index {
            expr: Box::new(mk_signal_expr(SignalId::from_raw(0))),
            index: Box::new(lit(3, 32)),
            span: dummy_span(),
        };
        assert_eq!(expr_width(&idx, &design), Some(1));
    }

    // ---- Sensitivity lists, dynamic indices, specializations (issue #39) ----

    fn add_wire(module: &mut Module, raw_name: u32) -> SignalId {
        let id = SignalId::from_raw(module.signals.len() as u32);
        module.signals.alloc(Signal {
            id,
            name: Ident::from_raw(raw_name),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: dummy_span(),
        })
    }

    fn add_process(module: &mut Module, sensitivity: Sensitivity, body: Statement) {
        module.processes.alloc(Process {
            id: ProcessId::from_raw(module.processes.len() as u32),
            name: None,
            kind: ProcessKind::Sequential,
            body,
            sensitivity,
            span: dummy_span(),
        });
    }

    #[test]
    fn edge_list_signal_counts_as_read() {
        // `always @(posedge gclk) q <= d;` — gclk only appears in the edge list.
        let mut module = mk_module();
        let gclk = add_wire(&mut module, 0);
        let q = add_wire(&mut module, 1);
        let d = add_wire(&mut module, 2);
        let unrelated = add_wire(&mut module, 3);
        add_process(
            &mut module,
            Sensitivity::EdgeList(vec![EdgeSensitivity {
                signal: gclk,
                edge: Edge::Posedge,
            }]),
            Statement::Assign {
                target: SignalRef::Signal(q),
                value: mk_signal_expr(d),
                kind: AssignKind::NonBlocking,
                span: dummy_span(),
            },
        );
        assert!(is_signal_read_in_module(&module, gclk));
        assert!(is_signal_read_in_module(&module, d));
        assert!(!is_signal_read_in_module(&module, q));
        assert!(!is_signal_read_in_module(&module, unrelated));
    }

    #[test]
    fn signal_list_entry_counts_as_read_but_all_does_not() {
        let mut module = mk_module();
        let a = add_wire(&mut module, 0);
        let b = add_wire(&mut module, 1);
        add_process(
            &mut module,
            Sensitivity::SignalList(vec![a]),
            Statement::Nop,
        );
        add_process(&mut module, Sensitivity::All, Statement::Nop);
        assert!(is_signal_read_in_module(&module, a));
        assert!(!is_signal_read_in_module(&module, b));
    }

    #[test]
    fn assignment_lhs_finds_top_level_operator() {
        assert_eq!(assignment_lhs("mem[addr] <= d;"), "mem[addr] ");
        assert_eq!(assignment_lhs("mem[a <= b] <= d;"), "mem[a <= b] ");
        assert_eq!(assignment_lhs("assign y[i] = a == b;"), "assign y[i] ");
        assert_eq!(assignment_lhs("v(idx) := x;"), "v(idx) ");
        assert_eq!(assignment_lhs("cnt[k] += 1;"), "cnt[k] +");
        assert_eq!(assignment_lhs("no operator"), "no operator");
        assert_eq!(assignment_lhs(""), "");
    }

    #[test]
    fn dynamic_index_reads_from_source_text() {
        use aion_common::Interner;
        use aion_source::SourceDb;

        let interner = Interner::new();
        let mut db = SourceDb::new();
        let text = "mem[Addr + off] <= d;  q[3] <= e;  mem <= x[sel];  {carry, s} <= a + b;  {mem[k], q} <= z;";
        let fid = db.add_source("t.v", text.to_string());
        let span_of = |s: &str| {
            let start = text.find(s).unwrap() as u32;
            Span::new(fid, start, start + s.len() as u32)
        };

        let mut module = mk_module();
        let mem = add_wire(&mut module, 0);
        let q = add_wire(&mut module, 1);
        let body = Statement::Block {
            stmts: vec![
                // Dynamic index lowered to a whole-signal target.
                Statement::Assign {
                    target: SignalRef::Signal(mem),
                    value: Expr::Literal(LogicVec::all_zero(1)),
                    kind: AssignKind::NonBlocking,
                    span: span_of("mem[Addr + off] <= d;"),
                },
                // Constant index kept as a slice: nothing lost, nothing added.
                Statement::Assign {
                    target: SignalRef::Slice {
                        signal: q,
                        high: 3,
                        low: 3,
                    },
                    value: Expr::Literal(LogicVec::all_zero(1)),
                    kind: AssignKind::NonBlocking,
                    span: span_of("q[3] <= e;"),
                },
                // Index on the RHS only: not part of the LHS.
                Statement::Assign {
                    target: SignalRef::Signal(mem),
                    value: Expr::Literal(LogicVec::all_zero(1)),
                    kind: AssignKind::NonBlocking,
                    span: span_of("mem <= x[sel];"),
                },
                // Concatenation target: its members are written, not read.
                Statement::Assign {
                    target: SignalRef::Concat(vec![SignalRef::Signal(mem), SignalRef::Signal(q)]),
                    value: Expr::Literal(LogicVec::all_zero(1)),
                    kind: AssignKind::NonBlocking,
                    span: span_of("{carry, s} <= a + b;"),
                },
                // A dynamic index inside a concatenation is still a read.
                Statement::Assign {
                    target: SignalRef::Concat(vec![SignalRef::Signal(mem), SignalRef::Signal(q)]),
                    value: Expr::Literal(LogicVec::all_zero(1)),
                    kind: AssignKind::NonBlocking,
                    span: span_of("{mem[k], q} <= z;"),
                },
                // No source text: ignored.
                Statement::Assign {
                    target: SignalRef::Signal(mem),
                    value: Expr::Literal(LogicVec::all_zero(1)),
                    kind: AssignKind::NonBlocking,
                    span: dummy_span(),
                },
            ],
            span: dummy_span(),
        };
        add_process(&mut module, Sensitivity::All, body);

        let ctx = LintContext::new(&interner, &db);
        let names = dynamic_index_reads(&module, &ctx);
        let mut sorted: Vec<_> = names.into_iter().collect();
        sorted.sort();
        // Verilog is case-sensitive: `Addr` is kept as written, so it cannot
        // be confused with a different signal named `addr`.
        assert_eq!(
            sorted,
            vec!["Addr".to_string(), "k".to_string(), "off".to_string()]
        );
    }

    #[test]
    fn dynamic_index_reads_fold_case_for_vhdl() {
        use aion_common::Interner;
        use aion_source::SourceDb;

        let interner = Interner::new();
        let mut db = SourceDb::new();
        let text = "mem(Addr) <= d;";
        let fid = db.add_source("t.vhd", text.to_string());
        let mut module = mk_module();
        let mem = add_wire(&mut module, 0);
        module.assignments.push(Assignment {
            target: SignalRef::Signal(mem),
            value: Expr::Literal(LogicVec::all_zero(1)),
            span: Span::new(fid, 0, text.len() as u32),
        });
        let ctx = LintContext::new(&interner, &db);
        let names = dynamic_index_reads(&module, &ctx);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec!["addr".to_string()]
        );
    }

    /// Returns the sorted dynamic-index reads of one continuous assignment
    /// whose source text is `text` in a file named `file`, with a target of
    /// two whole signals (as both a dynamic index and an aggregate lower to).
    fn index_reads_of(file: &str, text: &str) -> Vec<String> {
        use aion_common::Interner;
        use aion_source::SourceDb;

        let interner = Interner::new();
        let mut db = SourceDb::new();
        let fid = db.add_source(file, text.to_string());
        let mut module = mk_module();
        let a = add_wire(&mut module, 0);
        let b = add_wire(&mut module, 1);
        module.assignments.push(Assignment {
            target: SignalRef::Concat(vec![SignalRef::Signal(a), SignalRef::Signal(b)]),
            value: Expr::Literal(LogicVec::all_zero(1)),
            span: Span::new(fid, 0, text.len() as u32),
        });
        let ctx = LintContext::new(&interner, &db);
        let mut names: Vec<_> = dynamic_index_reads(&module, &ctx).into_iter().collect();
        names.sort();
        names
    }

    #[test]
    fn vhdl_aggregate_target_is_not_an_index() {
        assert!(index_reads_of("t.vhd", "(carry, s) <= ('0' & a) + b;").is_empty());
        assert!(index_reads_of("t.vhd", "( (carry), s ) <= z;").is_empty());
        // A real index inside an aggregate is still a read.
        assert_eq!(index_reads_of("t.vhd", "(mem(K), q) <= z;"), vec!["k"]);
    }

    #[test]
    fn vhdl_indexed_write_counts_index_as_read() {
        assert_eq!(index_reads_of("t.vhd", "mem(addr) <= d;"), vec!["addr"]);
        assert_eq!(index_reads_of("t.vhd", "mem (addr) <= d;"), vec!["addr"]);
        assert_eq!(
            index_reads_of("t.vhd", "mem(to_integer(addr) + (off)) <= d;"),
            vec!["addr", "off", "to_integer"]
        );
        // Chained index after `)`.
        assert_eq!(index_reads_of("t.vhd", "m(i)(j) <= d;"), vec!["i", "j"]);
    }

    #[test]
    fn vhdl_literals_and_attributes_are_not_names() {
        // Attribute names and character literals after `'`.
        assert_eq!(
            index_reads_of("t.vhd", "mem(v'length - 1) <= d;"),
            vec!["v"]
        );
        assert_eq!(index_reads_of("t.vhd", "mem(c, 'a') <= d;"), vec!["c"]);
        // Based literals, bit-string literals and strings.
        assert_eq!(index_reads_of("t.vhd", "mem(16#FF# + i) <= d;"), vec!["i"]);
        assert_eq!(index_reads_of("t.vhd", "mem(i + x\"0F\") <= d;"), vec!["i"]);
        // A qualified expression only skips the tick.
        assert_eq!(index_reads_of("t.vhd", "mem(t'(i)) <= d;"), vec!["i", "t"]);
    }

    #[test]
    fn verilog_sized_literal_tail_is_not_a_name() {
        assert_eq!(
            index_reads_of("t.v", "mem[addr + 8'd1] <= x;"),
            vec!["addr"]
        );
        assert_eq!(
            index_reads_of(
                "t.sv",
                "mem[k + 4'hA + 2'b10 + 3'sd2 + 'o7 + 8'h ff + 4'bx1z?] <= x;"
            ),
            vec!["k"]
        );
        // An SV cast and a fill literal skip only the tick.
        assert_eq!(
            index_reads_of("t.sv", "mem[int'(i) + '1] <= x;"),
            vec!["i", "int"]
        );
        // String contents are not names.
        assert_eq!(
            index_reads_of("t.v", "mem[i + \"a\\\"b\"] <= x;"),
            vec!["i"]
        );
    }

    #[test]
    fn follows_name_cases() {
        assert!(follows_name(b"mem(", 3));
        assert!(follows_name(b"mem  (", 5));
        assert!(follows_name(b"f(x)(", 4));
        assert!(follows_name(b"a[1](", 4));
        assert!(!follows_name(b"(", 0));
        assert!(!follows_name(b"  (", 2));
        assert!(!follows_name(b"a, (", 3));
        assert!(!follows_name(b"((", 1));
        assert!(!follows_name(b"a + (", 4));
    }

    #[test]
    fn literal_skippers_stop_at_the_right_place() {
        assert_eq!(skip_verilog_based_literal(b"'d1]", 0), 3);
        assert_eq!(skip_verilog_based_literal(b"'Sh_Fz]", 0), 6);
        assert_eq!(skip_verilog_based_literal(b"'(x)", 0), 1);
        assert_eq!(skip_verilog_based_literal(b"'s", 0), 1);
        assert_eq!(skip_verilog_based_literal(b"'", 0), 1);
        assert_eq!(skip_vhdl_tick(b"'1')", 0), 3);
        assert_eq!(skip_vhdl_tick(b"'high)", 0), 5);
        assert_eq!(skip_vhdl_tick(b"'(", 0), 1);
        assert_eq!(skip_vhdl_tick(b"'", 0), 1);
        assert_eq!(skip_string(b"\"ab\" x", 0, true), 4);
        assert_eq!(skip_string(b"\"a\\\"b\" x", 0, false), 6);
        assert_eq!(skip_string(b"\"open", 0, false), 5);
    }

    #[test]
    fn duplicate_specialization_detection() {
        let mut first = mk_module();
        first.span = Span::new(aion_source::FileId::from_raw(0), 0, 10);
        let second = first.clone();
        let mut other = mk_module();
        other.name = Ident::from_raw(5);
        other.span = Span::new(aion_source::FileId::from_raw(0), 20, 30);
        let mut modules = Arena::new();
        let a = modules.alloc(first);
        let b = modules.alloc(second);
        let c = modules.alloc(other);
        let design = mk_design_from_arena(modules, a);
        assert!(!is_duplicate_specialization(design.modules.get(a), &design));
        assert!(is_duplicate_specialization(design.modules.get(b), &design));
        assert!(!is_duplicate_specialization(design.modules.get(c), &design));
    }

    #[test]
    fn dummy_span_modules_are_never_duplicates() {
        let mut modules = Arena::new();
        let a = modules.alloc(mk_module());
        let b = modules.alloc(mk_module());
        let design = mk_design_from_arena(modules, a);
        assert!(!is_duplicate_specialization(design.modules.get(b), &design));
    }

    fn mk_design_from_arena(modules: Arena<ModuleId, Module>, top: ModuleId) -> Design {
        Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        }
    }
}
