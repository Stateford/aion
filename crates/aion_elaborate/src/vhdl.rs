//! VHDL-2008 entity+architecture elaboration.
//!
//! Transforms a parsed entity and architecture pair into an IR
//! [`Module`](aion_ir::module::Module), handling generics, ports, architecture
//! signals, processes, concurrent assignments, and component instantiations.

use std::collections::HashMap;

use aion_common::{ContentHash, Ident, Logic, LogicVec};
use aion_diagnostics::DiagnosticSink;
use aion_ir::arena::Arena;
use aion_ir::cell::{Cell, CellKind, Connection};
use aion_ir::ids::{CellId, ModuleId, ProcessId, SignalId, TypeId};
use aion_ir::module::{Assignment, Module, Parameter};
use aion_ir::port::{Port, PortDirection};
use aion_ir::process::{Edge, EdgeSensitivity, Process, ProcessKind, Sensitivity};
use aion_ir::signal::{Signal, SignalKind};
use aion_ir::ConstValue;
use aion_vhdl_parser::ast::{self as vhdl_ast, PortMode};

use crate::const_eval::{self, ConstEnv};
use crate::context::ElaborationContext;
use crate::errors;
use crate::expr::{lower_vhdl_expr, lower_vhdl_to_signal_ref, SignalEnv};
use crate::ranges::{resolve_vhdl_discrete_range, DeclaredRange, RangeEnv, ResolvedRange};
use crate::registry::ModuleEntry;
use crate::stmt::{
    first_waveform_value, lower_vhdl_conditional_signal_assignment, lower_vhdl_conditional_value,
    lower_vhdl_selected_signal_assignment, lower_vhdl_stmt,
};
use crate::types;

/// Elaborates a VHDL entity+architecture pair into an IR module.
pub fn elaborate_vhdl_entity(
    entity: &vhdl_ast::EntityDecl,
    arch: &vhdl_ast::ArchitectureDecl,
    generic_overrides: &[(Ident, ConstValue)],
    ctx: &mut ElaborationContext<'_>,
) -> ModuleId {
    let mut const_env = ConstEnv::new();
    let mut ir_params = Vec::new();
    let mut sig_env = SignalEnv::new();
    apply_vhdl_generics(
        entity,
        generic_overrides,
        &mut const_env,
        &mut ir_params,
        &mut sig_env,
        ctx,
    );

    let mut signals: Arena<SignalId, Signal> = Arena::new();
    let mut ports = Vec::new();

    // Declared index ranges of generics, ports, objects and types, for
    // range attributes such as `v'range`.
    let mut ranges = RangeEnv::new();
    if let Some(ref generics) = entity.generics {
        for gen in &generics.decls {
            ranges.declare_object(&gen.names, &gen.ty, &const_env, ctx.source_db, ctx.interner);
        }
    }

    elaborate_vhdl_ports(
        entity,
        &const_env,
        &mut ranges,
        &mut signals,
        &mut sig_env,
        &mut ports,
        ctx,
    );

    // Architecture declarations
    for decl in &arch.decls {
        elaborate_vhdl_decl(
            decl,
            None,
            &const_env,
            &mut ranges,
            &mut signals,
            &mut sig_env,
            ctx,
        );
    }

    let mut cells: Arena<CellId, Cell> = Arena::new();
    let mut processes: Arena<ProcessId, Process> = Arena::new();
    let mut assignments = Vec::new();

    for stmt in &arch.stmts {
        elaborate_vhdl_concurrent(
            stmt,
            &const_env,
            &mut ranges,
            &mut signals,
            &mut sig_env,
            &mut cells,
            &mut processes,
            &mut assignments,
            ctx,
        );
    }

    let content_hash = ContentHash::from_bytes(
        &format!(
            "{}:{}:{}",
            ctx.interner.resolve(entity.name),
            ctx.interner.resolve(arch.name),
            generic_overrides
                .iter()
                .map(|(k, v)| format!("{}={:?}", ctx.interner.resolve(*k), v))
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes(),
    );

    let module = Module {
        id: ModuleId::from_raw(0),
        name: entity.name,
        span: entity.span,
        params: ir_params,
        ports,
        signals,
        cells,
        processes,
        assignments,
        clock_domains: Vec::new(),
        content_hash,
    };

    let mid = ctx.design.modules.alloc(module);
    ctx.design.source_map.insert_module(mid, entity.span);
    mid
}

/// Applies entity generics and overrides.
///
/// Each generic is also bound as a constant in `sig_env`, so that
/// expressions can use its name. It lowers with its declared type's width
/// (`std_logic_vector(7 downto 0)` is 8 bits, `boolean` 1); `integer`,
/// `natural` and `positive` generics, and types without a known width, are
/// 32 bits.
fn apply_vhdl_generics(
    entity: &vhdl_ast::EntityDecl,
    overrides: &[(Ident, ConstValue)],
    const_env: &mut ConstEnv,
    ir_params: &mut Vec<Parameter>,
    sig_env: &mut SignalEnv,
    ctx: &mut ElaborationContext<'_>,
) {
    let override_map: HashMap<_, _> = overrides.iter().cloned().collect();
    // Generics are the first declarations of an entity, so no object is in
    // scope yet for a range attribute (`x'range`) in a generic's type; an
    // empty range environment reports any such prefix as unresolvable.
    let no_ranges = RangeEnv::new();

    if let Some(ref generics) = entity.generics {
        for gen in &generics.decls {
            let ty = types::resolve_vhdl_type(
                &gen.ty,
                &no_ranges,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            let width = ctx.design.types.bit_width(ty).unwrap_or(32);
            for &name in &gen.names {
                let value = if let Some(ov) = override_map.get(&name) {
                    ov.clone()
                } else if let Some(ref default) = gen.default {
                    const_eval::eval_vhdl_expr(
                        default,
                        ctx.source_db,
                        ctx.interner,
                        const_env,
                        ctx.sink,
                    )
                    .unwrap_or(ConstValue::Int(0))
                } else {
                    ConstValue::Int(0)
                };
                sig_env.insert_const(name, value.clone(), width);
                const_env.insert(name, value.clone());
                ir_params.push(Parameter {
                    name,
                    ty: TypeId::from_raw(0),
                    value,
                    span: gen.span,
                });
            }
        }
    }
}

/// Elaborates the port list of a VHDL entity, recording each port's
/// declared index ranges in `ranges`.
fn elaborate_vhdl_ports(
    entity: &vhdl_ast::EntityDecl,
    const_env: &ConstEnv,
    ranges: &mut RangeEnv,
    signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    ports: &mut Vec<Port>,
    ctx: &mut ElaborationContext<'_>,
) {
    if let Some(ref port_list) = entity.ports {
        for iface in &port_list.decls {
            let dir = match iface.mode {
                Some(PortMode::In) | None => PortDirection::Input,
                Some(PortMode::Out) | Some(PortMode::Buffer) => PortDirection::Output,
                Some(PortMode::Inout) | Some(PortMode::Linkage) => PortDirection::InOut,
            };
            let ty = types::resolve_vhdl_type(
                &iface.ty,
                ranges,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            let kind = SignalKind::Port;
            ranges.declare_object(
                &iface.names,
                &iface.ty,
                const_env,
                ctx.source_db,
                ctx.interner,
            );

            for &name in &iface.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name,
                    ty,
                    kind,
                    init: None,
                    clock_domain: None,
                    span: iface.span,
                });
                sig_env.insert(name, sid);
                let pid = ctx.alloc_port_id();
                ports.push(Port {
                    id: pid,
                    name,
                    direction: dir,
                    ty,
                    signal: sid,
                    span: iface.span,
                });
            }
        }
    }
}

/// Elaborates a VHDL object declaration (signal, constant, variable).
///
/// `scope` is `None` for architecture-level declarations and the enclosing
/// process's scope name for process-local ones. Process-local objects get a
/// hierarchical IR name (`<scope>.<name>`, e.g. `p.t`), so same-named
/// variables of different processes stay distinct in the simulator and the
/// waveform, while `sig_env` still resolves them by their plain name inside
/// the process. Process-local variables use [`SignalKind::Variable`].
///
/// Declared defaults (`:= expr`) become the signal's `init` value; a default
/// that cannot be evaluated is reported with W204 instead of being dropped
/// silently.
///
/// Every object, type and subtype declaration also records its declared
/// index ranges in `ranges` (after the object's own type is resolved), so
/// later range attributes such as `v'range` can be resolved.
fn elaborate_vhdl_decl(
    decl: &vhdl_ast::Declaration,
    scope: Option<&str>,
    const_env: &ConstEnv,
    ranges: &mut RangeEnv,
    signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    ctx: &mut ElaborationContext<'_>,
) {
    let (names, ty, kind, default, span) = match decl {
        vhdl_ast::Declaration::Signal(sd) => {
            (&sd.names, &sd.ty, SignalKind::Wire, &sd.default, sd.span)
        }
        vhdl_ast::Declaration::Constant(cd) => {
            (&cd.names, &cd.ty, SignalKind::Const, &cd.value, cd.span)
        }
        vhdl_ast::Declaration::Variable(vd) => {
            // A shared (architecture-level) variable is visible to every
            // process, like a signal; only process-local ones are private.
            let kind = if scope.is_some() {
                SignalKind::Variable
            } else {
                SignalKind::Reg
            };
            (&vd.names, &vd.ty, kind, &vd.default, vd.span)
        }
        _ => {
            // Other declarations (component, etc.) are not lowered in Phase 0;
            // type and subtype declarations only contribute index ranges.
            ranges.declare(decl, const_env, ctx.source_db, ctx.interner);
            return;
        }
    };

    let ty_ast = ty;
    let ty = types::resolve_vhdl_type(
        ty,
        ranges,
        &mut ctx.design.types,
        const_env,
        ctx.source_db,
        ctx.interner,
        ctx.sink,
    );
    ranges.declare_object(names, ty_ast, const_env, ctx.source_db, ctx.interner);
    let width = ctx.design.types.bit_width(ty);
    let init = default.as_ref().and_then(|expr| {
        let value = eval_vhdl_default(expr, width, const_env, signals, sig_env, ctx);
        if value.is_none() {
            let shown = names
                .first()
                .map(|n| ctx.interner.resolve(*n).to_string())
                .unwrap_or_default();
            ctx.sink
                .emit(errors::warn_default_not_constant(&shown, expr.span()));
        }
        value
    });

    for &name in names {
        let ir_name = match scope {
            Some(scope) => ctx
                .interner
                .get_or_intern(&format!("{scope}.{}", ctx.interner.resolve(name))),
            None => name,
        };
        let sid = signals.alloc(Signal {
            id: SignalId::from_raw(0),
            name: ir_name,
            ty,
            kind,
            init: init.clone(),
            clock_domain: None,
            span,
        });
        sig_env.insert(name, sid);
    }
}

/// Evaluates the default value of a VHDL object declaration to a constant.
///
/// Handles character literals (`'1'`), string literals of `std_logic`
/// characters (`"1010"`), bit-string literals (`x"5A"`), `(others => c)`
/// and positional aggregates, references to previously declared objects
/// that have a constant init (e.g. another constant), and integer constant
/// expressions over generics. Logic values are fitted to `width` (the
/// declared type's bit width, when known): zero-extended or truncated.
///
/// Returns `None` if the expression is not one of these forms; the caller
/// reports that. No diagnostics are emitted here.
fn eval_vhdl_default(
    expr: &vhdl_ast::Expr,
    width: Option<u32>,
    const_env: &ConstEnv,
    signals: &Arena<SignalId, Signal>,
    sig_env: &SignalEnv,
    ctx: &ElaborationContext<'_>,
) -> Option<ConstValue> {
    let fit = |lv: LogicVec| -> ConstValue {
        match width {
            Some(w) if w != lv.width() && w > 0 => {
                let mut out = LogicVec::new(w);
                for i in 0..w.min(lv.width()) {
                    out.set(i, lv.get(i));
                }
                ConstValue::Logic(out)
            }
            _ => ConstValue::Logic(lv),
        }
    };
    match expr {
        vhdl_ast::Expr::Paren { inner, .. }
        | vhdl_ast::Expr::Qualified { expr: inner, .. }
        | vhdl_ast::Expr::TypeConversion { expr: inner, .. } => {
            eval_vhdl_default(inner, width, const_env, signals, sig_env, ctx)
        }
        vhdl_ast::Expr::CharLiteral { span } => {
            let text = ctx.source_db.snippet(*span).trim_matches('\'');
            let mut chars = text.chars();
            let bit = std_logic_char(chars.next()?)?;
            if chars.next().is_some() {
                return None;
            }
            let mut lv = LogicVec::new(1);
            lv.set(0, bit);
            Some(fit(lv))
        }
        vhdl_ast::Expr::StringLiteral { span } => {
            let text = ctx.source_db.snippet(*span);
            let body = text.strip_prefix('"')?.strip_suffix('"')?;
            let bits: Vec<Logic> = body
                .chars()
                .map(std_logic_char)
                .collect::<Option<Vec<_>>>()?;
            if bits.is_empty() {
                return None;
            }
            // Leftmost character is the most significant bit.
            let mut lv = LogicVec::new(bits.len() as u32);
            for (i, bit) in bits.iter().rev().enumerate() {
                lv.set(i as u32, *bit);
            }
            Some(fit(lv))
        }
        vhdl_ast::Expr::BitStringLiteral { span } => {
            let text = ctx.source_db.snippet(*span);
            crate::expr::parse_vhdl_bit_string(text).ok().map(fit)
        }
        vhdl_ast::Expr::Aggregate { elements, .. } => {
            // `(others => c)`: every bit takes the value of `c`.
            if let [only] = elements.as_slice() {
                if matches!(only.choices.as_slice(), [vhdl_ast::Choice::Others(_)]) {
                    let w = width?;
                    let ConstValue::Logic(elem) =
                        eval_vhdl_default(&only.value, Some(1), const_env, signals, sig_env, ctx)?
                    else {
                        return None;
                    };
                    let mut lv = LogicVec::new(w);
                    for i in 0..w {
                        lv.set(i, elem.get(0));
                    }
                    return Some(ConstValue::Logic(lv));
                }
            }
            // Positional aggregate of single bits, leftmost = MSB.
            if elements.iter().any(|e| !e.choices.is_empty()) || elements.is_empty() {
                return None;
            }
            let mut bits = Vec::with_capacity(elements.len());
            for e in elements {
                let ConstValue::Logic(b) =
                    eval_vhdl_default(&e.value, Some(1), const_env, signals, sig_env, ctx)?
                else {
                    return None;
                };
                bits.push(b.get(0));
            }
            let mut lv = LogicVec::new(bits.len() as u32);
            for (i, bit) in bits.iter().rev().enumerate() {
                lv.set(i as u32, *bit);
            }
            Some(fit(lv))
        }
        vhdl_ast::Expr::Name(name)
            if name.parts.is_empty() && !const_env.contains_key(&name.primary) =>
        {
            // A previously declared object with a constant init. It hides a
            // boolean literal of the same name, so it is looked up first.
            if let Some(&sid) = sig_env.get(&name.primary) {
                return match &signals.get(sid).init {
                    Some(ConstValue::Logic(lv)) => Some(fit(lv.clone())),
                    Some(other) => Some(other.clone()),
                    None => None,
                };
            }
            // Otherwise, a boolean literal.
            let b = const_eval::vhdl_boolean_literal(ctx.interner.resolve(name.primary))?;
            Some(fit(LogicVec::from_bool(b)))
        }
        _ => {
            // Integer constant expressions (literals, generics, arithmetic).
            // Evaluate against a scratch sink: failure is reported by the
            // caller as W204, not as a constant-expression error.
            let scratch = aion_diagnostics::DiagnosticSink::new();
            let value =
                const_eval::eval_vhdl_expr(expr, ctx.source_db, ctx.interner, const_env, &scratch)?;
            match (value, width) {
                (ConstValue::Int(v), Some(w)) => {
                    Some(ConstValue::Logic(LogicVec::from_u64(v as u64, w)))
                }
                (other, _) => Some(other),
            }
        }
    }
}

/// Maps a VHDL `std_logic` character literal to a 4-state [`Logic`] value.
///
/// `'0'`/`'L'` are 0, `'1'`/`'H'` are 1, `'Z'` is high-impedance, and
/// `'U'`, `'X'`, `'W'`, `'-'` are unknown. Returns `None` for any other
/// character.
pub(crate) fn std_logic_char(c: char) -> Option<Logic> {
    match c.to_ascii_uppercase() {
        '0' | 'L' => Some(Logic::Zero),
        '1' | 'H' => Some(Logic::One),
        'Z' => Some(Logic::Z),
        'U' | 'X' | 'W' | '-' => Some(Logic::X),
        _ => None,
    }
}

/// Elaborates a VHDL concurrent statement.
#[allow(clippy::too_many_arguments)]
fn elaborate_vhdl_concurrent(
    stmt: &vhdl_ast::ConcurrentStatement,
    const_env: &ConstEnv,
    ranges: &mut RangeEnv,
    signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    cells: &mut Arena<CellId, Cell>,
    processes: &mut Arena<ProcessId, Process>,
    assignments: &mut Vec<Assignment>,
    ctx: &mut ElaborationContext<'_>,
) {
    match stmt {
        vhdl_ast::ConcurrentStatement::Process(ps) => {
            // Process-local declarations (variables, constants) become
            // module signals visible only while lowering this process. A
            // variable keeps its value between activations, like a `reg`;
            // `:=` lowers to a blocking assignment to it.
            // They are named `<label>.<name>` (unlabelled processes use
            // `_process<N>`, which no VHDL identifier can clash with).
            let outer_env = (!ps.decls.is_empty()).then(|| sig_env.clone());
            let outer_ranges = (!ps.decls.is_empty()).then(|| ranges.clone());
            let scope = match ps.label {
                Some(label) => ctx.interner.resolve(label).to_string(),
                None => format!("_process{}", processes.len()),
            };
            for decl in &ps.decls {
                elaborate_vhdl_decl(decl, Some(&scope), const_env, ranges, signals, sig_env, ctx);
            }
            check_vhdl_loop_ranges(&ps.stmts, ranges, const_env, ctx);

            // Detect sequential processes by scanning for rising_edge/falling_edge
            let (kind, sensitivity) =
                detect_vhdl_process_kind(&ps.sensitivity, &ps.stmts, sig_env, ctx.interner);
            let ir_stmts: Vec<_> = ps
                .stmts
                .iter()
                .map(|s| lower_vhdl_stmt(s, sig_env, ctx.source_db, ctx.interner, ctx.sink))
                .collect();
            let body = if ir_stmts.len() == 1 {
                ir_stmts.into_iter().next().unwrap()
            } else {
                aion_ir::stmt::Statement::Block {
                    stmts: ir_stmts,
                    span: ps.span,
                }
            };
            processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: ps.label,
                kind,
                body,
                sensitivity,
                span: ps.span,
            });
            if let Some(env) = outer_env {
                *sig_env = env;
            }
            if let Some(env) = outer_ranges {
                *ranges = env;
            }
        }
        vhdl_ast::ConcurrentStatement::SignalAssignment(sa) => {
            let target = lower_vhdl_to_signal_ref(&sa.target, sig_env, ctx.interner, ctx.sink);
            let value = if let Some(wf) = sa.waveforms.first() {
                lower_vhdl_expr(&wf.value, sig_env, ctx.source_db, ctx.interner, ctx.sink)
            } else {
                aion_ir::expr::Expr::Literal(aion_common::LogicVec::all_zero(1))
            };
            assignments.push(Assignment {
                target,
                value,
                span: sa.span,
            });
        }
        vhdl_ast::ConcurrentStatement::ConditionalSignalAssignment(ca) => {
            elaborate_vhdl_conditional_assignment(ca, sig_env, processes, assignments, ctx);
        }
        vhdl_ast::ConcurrentStatement::SelectedSignalAssignment(sa) => {
            // Equivalent to `process (all) begin case sel is ... end case;
            // end process;` (IEEE 1076-2008 11.6).
            let body = lower_vhdl_selected_signal_assignment(
                sa,
                sig_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: sa.label,
                kind: ProcessKind::Combinational,
                body,
                sensitivity: Sensitivity::All,
                span: sa.span,
            });
        }
        vhdl_ast::ConcurrentStatement::ComponentInstantiation(ci) => {
            elaborate_vhdl_component(ci, signals, sig_env, cells, ctx);
        }
        vhdl_ast::ConcurrentStatement::ForGenerate(fg) => {
            // The generate body is not expanded in Phase 0, but its range is
            // still resolved so an unresolvable `x'range` is reported.
            resolve_loop_range(&fg.range, ranges, const_env, ctx);
        }
        vhdl_ast::ConcurrentStatement::IfGenerate(_) => {
            // Generate statements: not fully elaborated in Phase 0
        }
        vhdl_ast::ConcurrentStatement::Assert(_) => {}
        vhdl_ast::ConcurrentStatement::Error(_) => {}
    }
}

/// Elaborates a concurrent conditional signal assignment
/// (`t <= a when c1 else b;`), choosing the cheapest faithful IR form:
///
/// - **Clocked** (`q <= d when rising_edge(clk);`, optionally preceded by
///   asynchronous branches such as `'0' when rst = '1' else`): a sequential
///   process whose `If` chain is edge-sensitive to the clock, plus
///   both-edge sensitivity to every signal read by the conditions *before*
///   the clock edge (the asynchronous-priority branches).
/// - **Complete mux** (final unconditional `else`, no `unaffected`): a
///   continuous assignment of a nested ternary.
/// - **Otherwise** (no final `else`, or `unaffected` branches): a
///   combinational `process (all)` whose `If` chain leaves the target
///   unchanged on the uncovered paths — the latch the VHDL code describes.
/// - **All `unaffected`**: nothing at all.
fn elaborate_vhdl_conditional_assignment(
    ca: &vhdl_ast::ConditionalSignalAssignment,
    sig_env: &SignalEnv,
    processes: &mut Arena<ProcessId, Process>,
    assignments: &mut Vec<Assignment>,
    ctx: &mut ElaborationContext<'_>,
) {
    if ca
        .alternatives
        .iter()
        .all(|a| first_waveform_value(&a.waveform).is_none())
    {
        return;
    }

    // Edge detection over the conditions, in priority order.
    let mut edges = Vec::new();
    let mut async_reads = Vec::new();
    for alt in &ca.alternatives {
        let Some(cond) = &alt.condition else { break };
        collect_edge_calls_expr(cond, sig_env, ctx.interner, &mut edges);
        if !edges.is_empty() {
            // First clocked alternative; everything before it is async.
            break;
        }
        collect_vhdl_expr_signals(cond, sig_env, &mut async_reads);
    }

    if !edges.is_empty() {
        for sid in async_reads {
            if !edges.iter().any(|e| e.signal == sid) {
                edges.push(EdgeSensitivity {
                    signal: sid,
                    edge: Edge::Both,
                });
            }
        }
        let body = lower_vhdl_conditional_signal_assignment(
            ca,
            sig_env,
            ctx.source_db,
            ctx.interner,
            ctx.sink,
        );
        processes.alloc(Process {
            id: ProcessId::from_raw(0),
            name: ca.label,
            kind: ProcessKind::Sequential,
            body,
            sensitivity: Sensitivity::EdgeList(edges),
            span: ca.span,
        });
        return;
    }

    if let Some(value) = lower_vhdl_conditional_value(
        &ca.alternatives,
        sig_env,
        ctx.source_db,
        ctx.interner,
        ctx.sink,
    ) {
        let target = lower_vhdl_to_signal_ref(&ca.target, sig_env, ctx.interner, ctx.sink);
        assignments.push(Assignment {
            target,
            value,
            span: ca.span,
        });
        return;
    }

    let body = lower_vhdl_conditional_signal_assignment(
        ca,
        sig_env,
        ctx.source_db,
        ctx.interner,
        ctx.sink,
    );
    processes.alloc(Process {
        id: ProcessId::from_raw(0),
        name: ca.label,
        kind: ProcessKind::Combinational,
        body,
        sensitivity: Sensitivity::All,
        span: ca.span,
    });
}

/// Collects (without duplicates) every signal of `sig_env` that a VHDL
/// expression reads, including signals passed as function-call or index
/// arguments.
fn collect_vhdl_expr_signals(expr: &vhdl_ast::Expr, sig_env: &SignalEnv, out: &mut Vec<SignalId>) {
    match expr {
        vhdl_ast::Expr::Name(name) => {
            if let Some(&sid) = sig_env.get(&name.primary) {
                if !out.contains(&sid) {
                    out.push(sid);
                }
            }
            for part in &name.parts {
                match part {
                    vhdl_ast::NameSuffix::Index(args, _) => {
                        for a in args {
                            collect_vhdl_expr_signals(a, sig_env, out);
                        }
                    }
                    vhdl_ast::NameSuffix::Attribute(_, Some(arg), _) => {
                        collect_vhdl_expr_signals(arg, sig_env, out);
                    }
                    _ => {}
                }
            }
        }
        vhdl_ast::Expr::Binary { left, right, .. } => {
            collect_vhdl_expr_signals(left, sig_env, out);
            collect_vhdl_expr_signals(right, sig_env, out);
        }
        vhdl_ast::Expr::Unary { operand: inner, .. }
        | vhdl_ast::Expr::Paren { inner, .. }
        | vhdl_ast::Expr::Qualified { expr: inner, .. }
        | vhdl_ast::Expr::TypeConversion { expr: inner, .. } => {
            collect_vhdl_expr_signals(inner, sig_env, out);
        }
        vhdl_ast::Expr::FunctionCall { name, args, .. } => {
            collect_vhdl_expr_signals(name, sig_env, out);
            for a in &args.elements {
                collect_vhdl_expr_signals(&a.actual, sig_env, out);
            }
        }
        vhdl_ast::Expr::Attribute { prefix, arg, .. } => {
            collect_vhdl_expr_signals(prefix, sig_env, out);
            if let Some(a) = arg {
                collect_vhdl_expr_signals(a, sig_env, out);
            }
        }
        vhdl_ast::Expr::Aggregate { elements, .. } => {
            for e in elements {
                collect_vhdl_expr_signals(&e.value, sig_env, out);
            }
        }
        _ => {}
    }
}

/// Resolves the discrete range of a `for` loop or `for ... generate`.
///
/// Range attributes (`v'range`, `v'reverse_range`) are resolved against
/// `ranges` and an unresolvable prefix is reported (`E212`). Explicit ranges
/// and type marks are evaluated quietly: their bounds may name objects the
/// constant evaluator does not track yet (e.g. architecture constants), and
/// since loop and generate bodies are not unrolled in Phase 0, failing to
/// evaluate them is not an error here.
fn resolve_loop_range(
    range: &vhdl_ast::DiscreteRange,
    ranges: &RangeEnv,
    const_env: &ConstEnv,
    ctx: &ElaborationContext<'_>,
) -> Option<ResolvedRange> {
    let quiet = DiagnosticSink::new();
    let sink = match range {
        vhdl_ast::DiscreteRange::Attribute(_) => ctx.sink,
        _ => &quiet,
    };
    resolve_vhdl_discrete_range(range, ranges, const_env, ctx.source_db, ctx.interner, sink)
}

/// Resolves the range of every `for` loop in a sequential statement list,
/// recursing into `if`, `case` and loop bodies, so that range attributes
/// with an unresolvable prefix are reported (`E212`).
///
/// While a loop body is walked, the loop parameter shadows any outer object
/// of the same name (it is a scalar, so `i'range` on it is rejected rather
/// than silently resolving to an outer array `i`).
fn check_vhdl_loop_ranges(
    stmts: &[vhdl_ast::SequentialStatement],
    ranges: &mut RangeEnv,
    const_env: &ConstEnv,
    ctx: &ElaborationContext<'_>,
) {
    use vhdl_ast::SequentialStatement as S;
    for stmt in stmts {
        match stmt {
            S::ForLoop(fl) => {
                let resolved = resolve_loop_range(&fl.range, ranges, const_env, ctx);
                let shadowed = ranges.get(fl.var).cloned();
                ranges.insert(fl.var, DeclaredRange::NotArray(resolved));
                check_vhdl_loop_ranges(&fl.stmts, ranges, const_env, ctx);
                match shadowed {
                    Some(prev) => ranges.insert(fl.var, prev),
                    None => ranges.remove(fl.var),
                }
            }
            S::If(is) => {
                check_vhdl_loop_ranges(&is.then_stmts, ranges, const_env, ctx);
                for branch in &is.elsif_branches {
                    check_vhdl_loop_ranges(&branch.stmts, ranges, const_env, ctx);
                }
                check_vhdl_loop_ranges(&is.else_stmts, ranges, const_env, ctx);
            }
            S::Case(cs) => {
                for alt in &cs.alternatives {
                    check_vhdl_loop_ranges(&alt.stmts, ranges, const_env, ctx);
                }
            }
            S::WhileLoop(wl) => check_vhdl_loop_ranges(&wl.stmts, ranges, const_env, ctx),
            S::Loop(lp) => check_vhdl_loop_ranges(&lp.stmts, ranges, const_env, ctx),
            _ => {}
        }
    }
}

/// Maps VHDL sensitivity list to IR Sensitivity.
fn map_vhdl_sensitivity(sens: &vhdl_ast::SensitivityList, sig_env: &SignalEnv) -> Sensitivity {
    match sens {
        vhdl_ast::SensitivityList::All => Sensitivity::All,
        vhdl_ast::SensitivityList::None => Sensitivity::All,
        vhdl_ast::SensitivityList::List(names) => {
            let sigs: Vec<_> = names
                .iter()
                .filter_map(|sn| {
                    let name = sn.parts.last()?;
                    sig_env.get(name).copied()
                })
                .collect();
            if sigs.is_empty() {
                Sensitivity::All
            } else {
                Sensitivity::SignalList(sigs)
            }
        }
    }
}

/// Determines the process kind and sensitivity for a VHDL process.
///
/// Scans the process body statements for `rising_edge` / `falling_edge` calls.
/// When found, returns `ProcessKind::Sequential` with an `EdgeList` sensitivity,
/// matching the approach used for SystemVerilog `always_ff` blocks.
fn detect_vhdl_process_kind(
    sens_list: &vhdl_ast::SensitivityList,
    stmts: &[vhdl_ast::SequentialStatement],
    sig_env: &SignalEnv,
    interner: &aion_common::Interner,
) -> (ProcessKind, Sensitivity) {
    // Scan statements for rising_edge/falling_edge calls
    let mut edges = Vec::new();
    for stmt in stmts {
        collect_edge_calls(stmt, sig_env, interner, &mut edges);
    }

    if !edges.is_empty() {
        // Sequential process — build EdgeList from detected edges
        // Also add any other sensitivity list signals as async reset candidates
        if let vhdl_ast::SensitivityList::List(names) = sens_list {
            for sn in names {
                if let Some(&sig_name) = sn.parts.last() {
                    if let Some(&sid) = sig_env.get(&sig_name) {
                        // Add signals not already in the edge list as async resets (negedge)
                        let already_listed = edges.iter().any(|e| e.signal == sid);
                        if !already_listed {
                            edges.push(EdgeSensitivity {
                                signal: sid,
                                edge: Edge::Both,
                            });
                        }
                    }
                }
            }
        }
        (ProcessKind::Sequential, Sensitivity::EdgeList(edges))
    } else {
        // No edge calls found — use original sensitivity mapping
        let sensitivity = map_vhdl_sensitivity(sens_list, sig_env);
        (ProcessKind::Combinational, sensitivity)
    }
}

/// Recursively scans VHDL sequential statements for `rising_edge` / `falling_edge` calls.
fn collect_edge_calls(
    stmt: &vhdl_ast::SequentialStatement,
    sig_env: &SignalEnv,
    interner: &aion_common::Interner,
    edges: &mut Vec<EdgeSensitivity>,
) {
    match stmt {
        vhdl_ast::SequentialStatement::If(if_stmt) => {
            collect_edge_calls_expr(&if_stmt.condition, sig_env, interner, edges);
            for s in &if_stmt.then_stmts {
                collect_edge_calls(s, sig_env, interner, edges);
            }
            for elsif in &if_stmt.elsif_branches {
                collect_edge_calls_expr(&elsif.condition, sig_env, interner, edges);
                for s in &elsif.stmts {
                    collect_edge_calls(s, sig_env, interner, edges);
                }
            }
            for s in &if_stmt.else_stmts {
                collect_edge_calls(s, sig_env, interner, edges);
            }
        }
        vhdl_ast::SequentialStatement::Case(case_stmt) => {
            for alt in &case_stmt.alternatives {
                for s in &alt.stmts {
                    collect_edge_calls(s, sig_env, interner, edges);
                }
            }
        }
        // VHDL-2008 `q <= d when rising_edge(clk);` inside a process.
        vhdl_ast::SequentialStatement::ConditionalSignalAssignment(ca) => {
            for cond in ca.alternatives.iter().filter_map(|a| a.condition.as_ref()) {
                collect_edge_calls_expr(cond, sig_env, interner, edges);
            }
        }
        vhdl_ast::SequentialStatement::ConditionalVariableAssignment(cv) => {
            for cond in cv.alternatives.iter().filter_map(|a| a.condition.as_ref()) {
                collect_edge_calls_expr(cond, sig_env, interner, edges);
            }
        }
        _ => {}
    }
}

/// Checks a VHDL expression for `rising_edge(sig)` or `falling_edge(sig)` calls.
fn collect_edge_calls_expr(
    expr: &vhdl_ast::Expr,
    sig_env: &SignalEnv,
    interner: &aion_common::Interner,
    edges: &mut Vec<EdgeSensitivity>,
) {
    match expr {
        vhdl_ast::Expr::Name(name) => {
            let primary_text = interner.resolve(name.primary).to_lowercase();
            if (primary_text == "rising_edge" || primary_text == "falling_edge")
                && !name.parts.is_empty()
            {
                if let vhdl_ast::NameSuffix::Index(args, _) = &name.parts[0] {
                    if let Some(vhdl_ast::Expr::Name(arg_name)) = args.first() {
                        if let Some(&sid) = sig_env.get(&arg_name.primary) {
                            let edge = if primary_text == "rising_edge" {
                                Edge::Posedge
                            } else {
                                Edge::Negedge
                            };
                            let already = edges.iter().any(|e| e.signal == sid);
                            if !already {
                                edges.push(EdgeSensitivity { signal: sid, edge });
                            }
                        }
                    }
                }
            }
        }
        vhdl_ast::Expr::FunctionCall { name, .. } => {
            // Also handle the case where the parser uses FunctionCall variant
            collect_edge_calls_expr(name, sig_env, interner, edges);
        }
        vhdl_ast::Expr::Binary { left, right, .. } => {
            collect_edge_calls_expr(left, sig_env, interner, edges);
            collect_edge_calls_expr(right, sig_env, interner, edges);
        }
        vhdl_ast::Expr::Unary { operand, .. } => {
            collect_edge_calls_expr(operand, sig_env, interner, edges);
        }
        vhdl_ast::Expr::Paren { inner, .. } => {
            collect_edge_calls_expr(inner, sig_env, interner, edges);
        }
        _ => {}
    }
}

/// Evaluates the actual of one `generic map` association.
///
/// Architecture-level constants are stored as signals with a constant init,
/// not in the constant environment, so a bare name that is a declared object
/// is resolved here first: a constant yields its init value, and any other
/// object (a signal, or a constant whose value is unknown) is reported as
/// E209, since a generic needs a value at elaboration time. This keeps a
/// declared object from being mistaken for the boolean literal `true`/`false`.
/// Every other actual goes through [`const_eval::eval_vhdl_expr`].
fn eval_vhdl_generic_actual(
    actual: &vhdl_ast::Expr,
    signals: &Arena<SignalId, Signal>,
    sig_env: &SignalEnv,
    ctx: &ElaborationContext<'_>,
) -> Option<ConstValue> {
    if let vhdl_ast::Expr::Name(name) = actual {
        let declared = sig_env.get(&name.primary).filter(|_| name.parts.is_empty());
        if let Some(&sid) = declared {
            let sig = signals.get(sid);
            if sig.kind == SignalKind::Const {
                if let Some(init) = &sig.init {
                    return Some(init.clone());
                }
            }
            ctx.sink.emit(errors::error_param_not_const(
                &format!(
                    "`{}` is not a constant with a known value",
                    ctx.interner.resolve(name.primary)
                ),
                name.span,
            ));
            return None;
        }
    }
    const_eval::eval_vhdl_expr(
        actual,
        ctx.source_db,
        ctx.interner,
        sig_env.consts(),
        ctx.sink,
    )
}

/// Elaborates a VHDL component instantiation.
fn elaborate_vhdl_component(
    ci: &vhdl_ast::ComponentInstantiation,
    signals: &Arena<SignalId, Signal>,
    sig_env: &SignalEnv,
    cells: &mut Arena<CellId, Cell>,
    ctx: &mut ElaborationContext<'_>,
) {
    let module_name = match &ci.unit {
        vhdl_ast::InstantiatedUnit::Component(sn) => {
            if let Some(&name) = sn.parts.last() {
                name
            } else {
                return;
            }
        }
        vhdl_ast::InstantiatedUnit::Entity(sn, _) => {
            if let Some(&name) = sn.parts.last() {
                name
            } else {
                return;
            }
        }
    };
    // VHDL names are case-insensitive: bind to a Verilog/SV module that
    // differs only in case when the folded name has no exact match.
    let module_name = ctx.registry.canonical_vhdl_name(module_name, ctx.interner);

    // Build generic overrides
    let mut generic_overrides: Vec<(Ident, ConstValue)> = ci
        .generic_map
        .as_ref()
        .map(|gm| {
            gm.elements
                .iter()
                .filter_map(|elem| {
                    let formal_name = extract_vhdl_formal(&elem.formal)?;
                    let val = eval_vhdl_generic_actual(&elem.actual, signals, sig_env, ctx)?;
                    Some((formal_name, val))
                })
                .collect()
        })
        .unwrap_or_default();
    // The folded generic names must bind to a case-sensitive Verilog/SV child.
    crate::binding::canonicalize_overrides(
        &mut generic_overrides,
        ctx.registry.lookup(module_name).as_ref(),
        true,
        ctx.interner,
    );

    if let Some(mid) = ctx.check_cache(module_name, &generic_overrides) {
        let connections = build_vhdl_port_connections(ci, sig_env, mid, ctx);
        cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: ci.label,
            kind: CellKind::Instance {
                module: mid,
                params: generic_overrides,
            },
            connections,
            span: ci.span,
        });
        return;
    }

    if !ctx.push_elab_stack(module_name, ci.span) {
        cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: ci.label,
            kind: CellKind::BlackBox { port_names: vec![] },
            connections: vec![],
            span: ci.span,
        });
        return;
    }

    let mid = match ctx.registry.lookup(module_name) {
        Some(ModuleEntry::Vhdl {
            entity,
            architecture,
        }) => {
            let mid = elaborate_vhdl_entity(entity, architecture, &generic_overrides, ctx);
            ctx.insert_cache(module_name, &generic_overrides, mid);
            mid
        }
        Some(ModuleEntry::Verilog(sub_decl)) => {
            let mid = crate::verilog::elaborate_verilog_module(sub_decl, &generic_overrides, ctx);
            ctx.insert_cache(module_name, &generic_overrides, mid);
            mid
        }
        Some(ModuleEntry::Sv(sub_decl)) => {
            let mid = crate::sv::elaborate_sv_module(sub_decl, &generic_overrides, ctx);
            ctx.insert_cache(module_name, &generic_overrides, mid);
            mid
        }
        None => {
            ctx.sink.emit(errors::error_unknown_module(
                ctx.interner.resolve(module_name),
                ci.span,
            ));
            ctx.pop_elab_stack();
            cells.alloc(Cell {
                id: CellId::from_raw(0),
                name: ci.label,
                kind: CellKind::BlackBox { port_names: vec![] },
                connections: vec![],
                span: ci.span,
            });
            return;
        }
    };
    ctx.pop_elab_stack();

    let connections = build_vhdl_port_connections(ci, sig_env, mid, ctx);
    cells.alloc(Cell {
        id: CellId::from_raw(0),
        name: ci.label,
        kind: CellKind::Instance {
            module: mid,
            params: generic_overrides,
        },
        connections,
        span: ci.span,
    });
}

/// Extracts a formal name from a VHDL association formal expression.
fn extract_vhdl_formal(formal: &Option<vhdl_ast::Expr>) -> Option<Ident> {
    match formal {
        Some(vhdl_ast::Expr::Name(name)) => Some(name.primary),
        _ => None,
    }
}

/// Builds IR connections from VHDL port map, looking up actual port directions
/// from the target module.
fn build_vhdl_port_connections(
    ci: &vhdl_ast::ComponentInstantiation,
    sig_env: &SignalEnv,
    target_module: ModuleId,
    ctx: &ElaborationContext<'_>,
) -> Vec<Connection> {
    ci.port_map
        .as_ref()
        .map(|pm| {
            pm.elements
                .iter()
                .filter_map(|elem| {
                    // Folded VHDL formals bind case-insensitively to a
                    // Verilog/SV child's ports (exact match first).
                    let formal_name = crate::binding::bind_port(
                        target_module,
                        extract_vhdl_formal(&elem.formal)?,
                        true,
                        elem.span,
                        ctx,
                    )?;
                    let signal =
                        lower_vhdl_to_signal_ref(&elem.actual, sig_env, ctx.interner, ctx.sink);
                    let direction = lookup_port_direction(target_module, formal_name, ctx);
                    Some(Connection {
                        port_name: formal_name,
                        direction,
                        signal,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Looks up the direction of a port in the target module by name.
///
/// Returns `PortDirection::Input` as fallback if the port is not found.
fn lookup_port_direction(
    target: ModuleId,
    port_name: Ident,
    ctx: &ElaborationContext<'_>,
) -> PortDirection {
    let module = ctx.design.modules.get(target);
    module
        .ports
        .iter()
        .find(|p| p.name == port_name)
        .map(|p| p.direction)
        .unwrap_or(PortDirection::Input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_common::Interner;
    use aion_diagnostics::DiagnosticSink;
    use aion_source::{SourceDb, Span};

    use crate::registry::ModuleRegistry;

    fn setup() -> (Interner, SourceDb, DiagnosticSink) {
        (Interner::new(), SourceDb::new(), DiagnosticSink::new())
    }

    #[test]
    fn elaborate_empty_vhdl_entity() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("empty");
        let arch_name = interner.get_or_intern("rtl");

        let entity = vhdl_ast::EntityDecl {
            name,
            generics: None,
            ports: None,
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let arch = vhdl_ast::ArchitectureDecl {
            name: arch_name,
            entity_name: name,
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let file = vhdl_ast::VhdlDesignFile {
            units: vec![
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Entity(entity.clone()),
                    span: Span::DUMMY,
                },
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Architecture(arch.clone()),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &files, &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_vhdl_entity(&entity, &arch, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].ports.len(), 0);
    }

    #[test]
    fn elaborate_vhdl_entity_with_ports() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("counter");
        let arch_name = interner.get_or_intern("rtl");
        let clk = interner.get_or_intern("clk");
        let std_logic = interner.get_or_intern("std_logic");

        let entity = vhdl_ast::EntityDecl {
            name,
            generics: None,
            ports: Some(vhdl_ast::PortClause {
                decls: vec![vhdl_ast::InterfaceDecl {
                    names: vec![clk],
                    mode: Some(PortMode::In),
                    ty: vhdl_ast::TypeIndication {
                        type_mark: vhdl_ast::SelectedName {
                            parts: vec![std_logic],
                            span: Span::DUMMY,
                        },
                        constraint: None,
                        span: Span::DUMMY,
                    },
                    default: None,
                    span: Span::DUMMY,
                }],
                span: Span::DUMMY,
            }),
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let arch = vhdl_ast::ArchitectureDecl {
            name: arch_name,
            entity_name: name,
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let file = vhdl_ast::VhdlDesignFile {
            units: vec![
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Entity(entity.clone()),
                    span: Span::DUMMY,
                },
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Architecture(arch.clone()),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &files, &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_vhdl_entity(&entity, &arch, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].ports.len(), 1);
        assert_eq!(ctx.design.modules[mid].signals.len(), 1);
    }

    #[test]
    fn elaborate_vhdl_arch_with_signal() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("test");
        let arch_name = interner.get_or_intern("rtl");
        let sig = interner.get_or_intern("internal_sig");
        let std_logic = interner.get_or_intern("std_logic");

        let entity = vhdl_ast::EntityDecl {
            name,
            generics: None,
            ports: None,
            decls: vec![],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let arch = vhdl_ast::ArchitectureDecl {
            name: arch_name,
            entity_name: name,
            decls: vec![vhdl_ast::Declaration::Signal(vhdl_ast::SignalDecl {
                names: vec![sig],
                ty: vhdl_ast::TypeIndication {
                    type_mark: vhdl_ast::SelectedName {
                        parts: vec![std_logic],
                        span: Span::DUMMY,
                    },
                    constraint: None,
                    span: Span::DUMMY,
                },
                default: None,
                span: Span::DUMMY,
            })],
            stmts: vec![],
            span: Span::DUMMY,
        };
        let file = vhdl_ast::VhdlDesignFile {
            units: vec![
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Entity(entity.clone()),
                    span: Span::DUMMY,
                },
                vhdl_ast::DesignUnit::ContextUnit {
                    context: vec![],
                    unit: vhdl_ast::DesignUnitKind::Architecture(arch.clone()),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &[], &files, &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_vhdl_entity(&entity, &arch, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].signals.len(), 1);
    }

    #[test]
    fn detect_sequential_process_with_rising_edge() {
        let (interner, _source_db, _sink) = setup();
        let clk = interner.get_or_intern("clk");
        let rising_edge = interner.get_or_intern("rising_edge");

        let mut sig_env = SignalEnv::new();
        let clk_sid = SignalId::from_raw(0);
        sig_env.insert(clk, clk_sid);

        let sensitivity = vhdl_ast::SensitivityList::List(vec![vhdl_ast::SelectedName {
            parts: vec![clk],
            span: Span::DUMMY,
        }]);

        // Build a process body with: if rising_edge(clk) then ... end if;
        let stmts = vec![vhdl_ast::SequentialStatement::If(vhdl_ast::IfStatement {
            label: None,
            condition: vhdl_ast::Expr::Name(vhdl_ast::Name {
                primary: rising_edge,
                parts: vec![vhdl_ast::NameSuffix::Index(
                    vec![vhdl_ast::Expr::Name(vhdl_ast::Name {
                        primary: clk,
                        parts: vec![],
                        span: Span::DUMMY,
                    })],
                    Span::DUMMY,
                )],
                span: Span::DUMMY,
            }),
            then_stmts: vec![],
            elsif_branches: vec![],
            else_stmts: vec![],
            span: Span::DUMMY,
        })];

        let (kind, sens) = detect_vhdl_process_kind(&sensitivity, &stmts, &sig_env, &interner);
        assert!(
            matches!(kind, ProcessKind::Sequential),
            "process with rising_edge should be Sequential"
        );
        assert!(
            matches!(sens, Sensitivity::EdgeList(ref edges) if !edges.is_empty()),
            "should have EdgeList sensitivity"
        );
        if let Sensitivity::EdgeList(edges) = sens {
            assert_eq!(edges[0].signal, clk_sid);
            assert!(matches!(edges[0].edge, Edge::Posedge));
        }
    }

    #[test]
    fn detect_combinational_process_without_edges() {
        let (interner, _source_db, _sink) = setup();
        let a = interner.get_or_intern("a");

        let mut sig_env = SignalEnv::new();
        sig_env.insert(a, SignalId::from_raw(0));

        let sensitivity = vhdl_ast::SensitivityList::List(vec![vhdl_ast::SelectedName {
            parts: vec![a],
            span: Span::DUMMY,
        }]);

        // Empty body — no rising_edge/falling_edge calls
        let stmts = vec![vhdl_ast::SequentialStatement::Null { span: Span::DUMMY }];

        let (kind, _sens) = detect_vhdl_process_kind(&sensitivity, &stmts, &sig_env, &interner);
        assert!(
            matches!(kind, ProcessKind::Combinational),
            "process without edge calls should be Combinational"
        );
    }

    // ========================================================================
    // Range attributes (`x'range`, `x'reverse_range`)
    // ========================================================================

    /// Parses and elaborates `src` (top entity `t`), returning the design,
    /// the interner, the source text and every diagnostic.
    fn elab_vhdl(
        src: &str,
    ) -> (
        aion_ir::Design,
        Interner,
        String,
        Vec<aion_diagnostics::Diagnostic>,
    ) {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source("t.vhd", src.to_string());
        let parsed = crate::ParsedDesign {
            verilog_files: vec![],
            sv_files: vec![],
            vhdl_files: vec![aion_vhdl_parser::parse_file(
                file_id, &source_db, &interner, &sink,
            )],
        };
        let config: aion_config::ProjectConfig =
            toml::from_str("[project]\nname = \"t\"\nversion = \"0.1.0\"\ntop = \"t\"\n").unwrap();
        let design = crate::elaborate(&parsed, &config, &source_db, &interner, &sink).unwrap();
        (design, interner, src.to_string(), sink.diagnostics())
    }

    /// Wraps an architecture body (declarations, `begin`, statements) of
    /// entity `t`, which has ports `a : std_logic_vector(7 downto 0)` and
    /// `y : std_logic`.
    fn range_src(arch_body: &str) -> String {
        format!(
            "library ieee;\nuse ieee.std_logic_1164.all;\nuse ieee.numeric_std.all;\n\
             entity t is port (a : in std_logic_vector(7 downto 0); y : out std_logic);\n\
             end entity;\narchitecture rtl of t is\n{arch_body}\nend architecture;"
        )
    }

    fn e212_snippets(src: &str, diags: &[aion_diagnostics::Diagnostic]) -> Vec<String> {
        diags
            .iter()
            .filter(|d| d.code == errors::E212)
            .map(|d| src[d.primary_span.start as usize..d.primary_span.end as usize].to_string())
            .collect()
    }

    #[test]
    fn vhdl_range_attribute_sizes_vector_signals() {
        let (design, interner, _, diags) = elab_vhdl(&range_src(
            "signal b : std_logic_vector(a'range);\n\
             signal c : std_logic_vector(a'reverse_range);\n\
             subtype nib_t is std_logic_vector(3 downto 0);\n\
             signal d : unsigned(nib_t'range);\n\
             signal e : std_logic_vector(d'reverse_range);\n\
             begin\n y <= a(0);",
        ));
        assert!(diags.is_empty(), "{diags:?}");
        let module = &design.modules[design.top];
        let width = |name: &str| {
            let sig = module
                .signals
                .iter()
                .map(|(_, s)| s)
                .find(|s| interner.resolve(s.name) == name)
                .unwrap_or_else(|| panic!("no signal {name}"));
            design.types.bit_width(sig.ty)
        };
        assert_eq!(width("b"), Some(8));
        assert_eq!(width("c"), Some(8));
        assert_eq!(width("d"), Some(4));
        assert_eq!(width("e"), Some(4));
    }

    #[test]
    fn vhdl_for_loops_and_generates_over_range_attributes_elaborate_cleanly() {
        let (_, _, _, diags) = elab_vhdl(&range_src(
            "type mem_t is array (0 to 3, 7 downto 0) of std_logic;\n\
             constant K : std_logic_vector(5 downto 2) := \"0000\";\n\
             begin\n\
             p : process (a)\n\
               variable tmp : std_logic_vector(2 downto 0);\n\
             begin\n\
               for i in a'range loop null; end loop;\n\
               for i in a'reverse_range loop null; end loop;\n\
               for i in tmp'range loop null; end loop;\n\
               if a(0) = '1' then\n\
                 for i in mem_t'range(2) loop\n\
                   for j in K'reverse_range loop null; end loop;\n\
                 end loop;\n\
               end if;\n\
               y <= a(0);\n\
             end process;\n\
             g1 : for i in a'range generate end generate;\n\
             g2 : for i in K'reverse_range generate end generate;",
        ));
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn vhdl_unresolved_range_attribute_prefix_is_reported() {
        let (design, interner, src, diags) = elab_vhdl(&range_src(
            "signal b : std_logic_vector(nope'range);\n\
             begin\n\
             process (a) begin\n\
               case a(0) is\n\
                 when '1' => for i in missing'reverse_range loop null; end loop;\n\
                 when others => null;\n\
               end case;\n\
               y <= a(0);\n\
             end process;\n\
             g : for i in y'range generate end generate;",
        ));
        assert_eq!(
            e212_snippets(&src, &diags),
            vec!["nope'range", "missing'reverse_range", "y'range"],
            "{diags:?}"
        );
        let y_diag = diags
            .iter()
            .find(|d| d.message.contains("`y'range`"))
            .unwrap();
        assert!(
            y_diag.message.contains("not an array"),
            "{}",
            y_diag.message
        );
        // The vector sized by the unresolved attribute is an error type, not
        // a guessed width.
        let module = &design.modules[design.top];
        let b = module
            .signals
            .iter()
            .map(|(_, s)| s)
            .find(|s| interner.resolve(s.name) == "b")
            .unwrap();
        assert_eq!(*design.types.get(b.ty), aion_ir::types::Type::Error);
    }

    #[test]
    fn vhdl_loop_parameter_shadows_outer_array_in_range_attribute() {
        let (_, _, src, diags) = elab_vhdl(&range_src(
            "signal i : std_logic_vector(3 downto 0);\n\
             begin\n\
             process (a) begin\n\
               for i in 0 to 1 loop\n\
                 for j in i'range loop null; end loop;\n\
               end loop;\n\
               for k in i'range loop null; end loop;\n\
               y <= a(0);\n\
             end process;",
        ));
        // Inside the loop `i` is the integer loop parameter, not the signal;
        // after the loop the signal is visible again.
        assert_eq!(e212_snippets(&src, &diags), vec!["i'range"], "{diags:?}");
        let pos = src.find("for j in i'range").unwrap() + "for j in ".len();
        let d = diags.iter().find(|d| d.code == errors::E212).unwrap();
        assert_eq!(d.primary_span.start as usize, pos);
        assert!(d.message.contains("not an array"), "{}", d.message);
    }

    #[test]
    fn vhdl_process_local_range_does_not_leak_to_other_processes() {
        let (_, _, src, diags) = elab_vhdl(&range_src(
            "begin\n\
             p1 : process (a)\n\
               variable tmp : std_logic_vector(2 downto 0);\n\
             begin\n\
               for i in tmp'range loop null; end loop;\n\
               y <= a(0);\n\
             end process;\n\
             p2 : process (a) begin\n\
               for i in tmp'reverse_range loop null; end loop;\n\
               y <= a(0);\n\
             end process;",
        ));
        assert_eq!(
            e212_snippets(&src, &diags),
            vec!["tmp'reverse_range"],
            "{diags:?}"
        );
    }
}
