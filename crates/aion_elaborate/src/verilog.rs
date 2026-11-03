//! Verilog-2005 module elaboration.
//!
//! Transforms a parsed [`ModuleDecl`](aion_verilog_parser::ast::ModuleDecl) into
//! an IR [`Module`](aion_ir::module::Module), resolving ports, signals, assignments,
//! processes, and instantiations.

use std::collections::HashMap;

use aion_common::{ContentHash, Ident, Interner};
use aion_diagnostics::DiagnosticSink;
use aion_ir::arena::Arena;
use aion_ir::cell::{Cell, CellKind, Connection};
use aion_ir::ids::{CellId, ModuleId, ProcessId, SignalId, TypeId};
use aion_ir::module::{Assignment, Module, Parameter};
use aion_ir::port::{Port, PortDirection};
use aion_ir::process::{Edge, EdgeSensitivity, Process, ProcessKind, Sensitivity};
use aion_ir::signal::{Signal, SignalKind};
use aion_ir::stmt::Statement as IrStmt;
use aion_ir::ConstValue;
use aion_source::{SourceDb, Span};
use aion_verilog_parser::ast::{self as v_ast, Direction};

use crate::const_eval::{self, ConstEnv};
use crate::context::ElaborationContext;
use crate::errors;
use crate::expr::{lower_to_signal_ref, lower_verilog_expr, SignalEnv};
use crate::registry::ModuleEntry;
use crate::stmt::{lower_verilog_looping_always, lower_verilog_stmt};
use crate::types;

/// Elaborates a Verilog module declaration into an IR module.
///
/// Resolves parameters, creates ports and signals, lowers always/initial blocks
/// to processes, and handles instantiations recursively.
pub fn elaborate_verilog_module(
    decl: &v_ast::ModuleDecl,
    param_overrides: &[(Ident, ConstValue)],
    ctx: &mut ElaborationContext<'_>,
) -> ModuleId {
    // 1. Build const env from parameter declarations and overrides
    let mut const_env = ConstEnv::new();
    let mut ir_params = Vec::new();
    apply_verilog_params(decl, param_overrides, &mut const_env, &mut ir_params, ctx);

    // 2. Allocate module in the design
    let mut signals: Arena<SignalId, Signal> = Arena::new();
    let mut sig_env = SignalEnv::new();
    let mut ports = Vec::new();

    // 3. Elaborate ports
    elaborate_verilog_ports(
        decl,
        &const_env,
        &mut signals,
        &mut sig_env,
        &mut ports,
        ctx,
    );

    // 4. Walk module items
    let mut cells: Arena<CellId, Cell> = Arena::new();
    let mut processes: Arena<ProcessId, Process> = Arena::new();
    let mut assignments = Vec::new();

    for item in &decl.items {
        elaborate_verilog_item(
            item,
            &const_env,
            &mut signals,
            &mut sig_env,
            &mut cells,
            &mut processes,
            &mut assignments,
            ctx,
        );
    }

    // 5. Create module and allocate in design
    let content_hash = ContentHash::from_bytes(
        &format!(
            "{}:{}",
            ctx.interner.resolve(decl.name),
            param_overrides
                .iter()
                .map(|(k, v)| format!("{}={:?}", ctx.interner.resolve(*k), v))
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes(),
    );

    let module = Module {
        id: ModuleId::from_raw(0), // will be set by arena
        name: decl.name,
        span: decl.span,
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
    ctx.design.source_map.insert_module(mid, decl.span);
    mid
}

/// Applies parameter declarations and overrides to build the const env.
fn apply_verilog_params(
    decl: &v_ast::ModuleDecl,
    overrides: &[(Ident, ConstValue)],
    const_env: &mut ConstEnv,
    ir_params: &mut Vec<Parameter>,
    ctx: &mut ElaborationContext<'_>,
) {
    let override_map: HashMap<_, _> = overrides.iter().cloned().collect();

    // Header (`#(...)`) parameters first, then body `parameter`/`localparam`
    // declarations in source order, so a later one can use an earlier one
    // (`localparam A = 1, B = A + 1;`). A body `parameter` can be overridden
    // only when the module has no parameter port list; otherwise it acts as a
    // `localparam` (IEEE 1364-2005 12.2, IEEE 1800-2017 6.20.1). Overriding a
    // non-overridable parameter is reported as E214.
    // A `localparam` in the `#(...)` list is local too.
    let header = decl.params.iter().map(|p| (p, !p.local));
    let body = decl.items.iter().filter_map(|item| match item {
        v_ast::ModuleItem::ParameterDecl(p) | v_ast::ModuleItem::LocalparamDecl(p) => {
            Some((p, !p.local && decl.params.is_empty()))
        }
        _ => None,
    });

    for (param, overridable) in header.chain(body) {
        let name = param.name;
        // Overriding a local parameter is illegal: report it instead of
        // silently keeping the declared value.
        if !overridable && override_map.contains_key(&name) {
            ctx.sink.emit(errors::error_param_not_overridable(
                ctx.interner.resolve(name),
                param.span,
            ));
        }
        let value = if let Some(ov) = override_map.get(&name).filter(|_| overridable) {
            ov.clone()
        } else if let Some(ref value) = param.value {
            const_eval::eval_verilog_expr(value, ctx.source_db, ctx.interner, const_env, ctx.sink)
                .unwrap_or(ConstValue::Int(0))
        } else {
            ConstValue::Int(0)
        };
        const_env.insert(name, value.clone());
        ir_params.push(Parameter {
            name,
            ty: TypeId::from_raw(0),
            value,
            span: param.span,
        });
    }
}

/// Elaborates the port list of a Verilog module.
fn elaborate_verilog_ports(
    decl: &v_ast::ModuleDecl,
    const_env: &ConstEnv,
    signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    ports: &mut Vec<Port>,
    ctx: &mut ElaborationContext<'_>,
) {
    for port_decl in &decl.ports {
        let dir = match port_decl.direction {
            Direction::Input => PortDirection::Input,
            Direction::Output => PortDirection::Output,
            Direction::Inout => PortDirection::InOut,
        };
        let ty = types::resolve_verilog_net_type(
            port_decl.net_type.as_ref(),
            port_decl.range.as_ref(),
            port_decl.signed,
            &mut ctx.design.types,
            const_env,
            ctx.source_db,
            ctx.interner,
            ctx.sink,
        );
        let kind = SignalKind::Port;
        for &name in &port_decl.names {
            let sid = signals.alloc(Signal {
                id: SignalId::from_raw(0),
                name,
                ty,
                kind,
                init: None,
                clock_domain: None,
                span: port_decl.span,
            });
            sig_env.insert(name, sid);
            let pid = ctx.alloc_port_id();
            ports.push(Port {
                id: pid,
                name,
                direction: dir,
                ty,
                signal: sid,
                span: port_decl.span,
            });
        }
    }
}

/// Elaborates a single Verilog module item.
#[allow(clippy::too_many_arguments)]
fn elaborate_verilog_item(
    item: &v_ast::ModuleItem,
    const_env: &ConstEnv,
    signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    cells: &mut Arena<CellId, Cell>,
    processes: &mut Arena<ProcessId, Process>,
    assignments: &mut Vec<Assignment>,
    ctx: &mut ElaborationContext<'_>,
) {
    match item {
        v_ast::ModuleItem::NetDecl(net) => {
            if let Some(delay) = &net.delay {
                warn_net_delay(ctx, "net declaration", delay.span);
            }
            let ty = types::resolve_verilog_net_type(
                Some(&net.net_type),
                net.range.as_ref(),
                net.signed,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            for dn in &net.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name: dn.name,
                    ty,
                    kind: SignalKind::Wire,
                    init: None,
                    clock_domain: None,
                    span: dn.span,
                });
                sig_env.insert(dn.name, sid);
            }
        }
        v_ast::ModuleItem::RegDecl(reg) => {
            let ty = types::resolve_verilog_type(
                reg.range.as_ref(),
                reg.signed,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            for dn in &reg.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name: dn.name,
                    ty,
                    kind: SignalKind::Reg,
                    init: None,
                    clock_domain: None,
                    span: dn.span,
                });
                sig_env.insert(dn.name, sid);
            }
        }
        v_ast::ModuleItem::IntegerDecl(idecl) => {
            let ty = ctx.design.types.intern(aion_ir::types::Type::Integer);
            for dn in &idecl.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name: dn.name,
                    ty,
                    kind: SignalKind::Reg,
                    init: None,
                    clock_domain: None,
                    span: dn.span,
                });
                sig_env.insert(dn.name, sid);
            }
        }
        v_ast::ModuleItem::RealDecl(rdecl) => {
            let ty = ctx.design.types.intern(aion_ir::types::Type::Real);
            for dn in &rdecl.names {
                let sid = signals.alloc(Signal {
                    id: SignalId::from_raw(0),
                    name: dn.name,
                    ty,
                    kind: SignalKind::Reg,
                    init: None,
                    clock_domain: None,
                    span: dn.span,
                });
                sig_env.insert(dn.name, sid);
            }
        }
        v_ast::ModuleItem::ParameterDecl(_) | v_ast::ModuleItem::LocalparamDecl(_) => {
            // Evaluated into the const env by `apply_verilog_params`.
        }
        v_ast::ModuleItem::PortDecl(_) => {
            // Non-ANSI port declarations — handled by elaborate_verilog_ports
        }
        v_ast::ModuleItem::ContinuousAssign(ca) => {
            if let Some(delay) = &ca.delay {
                warn_net_delay(ctx, "continuous assignment", delay.span);
            }
            let target =
                lower_to_signal_ref(&ca.target, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            let value =
                lower_verilog_expr(&ca.value, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            assignments.push(Assignment {
                target,
                value,
                span: ca.span,
            });
        }
        v_ast::ModuleItem::AlwaysBlock(ab) => {
            let (kind, sensitivity, body_stmt) = analyze_verilog_always(
                &ab.body,
                ab.span,
                sig_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            let _pid = processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: None,
                kind,
                body: body_stmt,
                sensitivity,
                span: ab.span,
            });
        }
        v_ast::ModuleItem::InitialBlock(ib) => {
            let body = lower_verilog_stmt(&ib.body, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            let _pid = processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: None,
                kind: ProcessKind::Initial,
                body,
                // An initial block runs once at time 0 and then resumes only
                // from its own delays: it is sensitive to nothing.
                sensitivity: Sensitivity::SignalList(Vec::new()),
                span: ib.span,
            });
        }
        v_ast::ModuleItem::Instantiation(inst) => {
            elaborate_verilog_instantiation(inst, sig_env, cells, ctx);
        }
        v_ast::ModuleItem::GenerateBlock(_) => {
            // Generate blocks need full elaboration — emit unsupported for Phase 0
        }
        v_ast::ModuleItem::GateInst(_)
        | v_ast::ModuleItem::GenvarDecl(_)
        | v_ast::ModuleItem::FunctionDecl(_)
        | v_ast::ModuleItem::TaskDecl(_)
        | v_ast::ModuleItem::DefparamDecl(_) => {
            // Not elaborated in Phase 0
        }
        v_ast::ModuleItem::Error(_) => {}
    }
}

/// Reports a delay on a net declaration or continuous assignment as ignored
/// (W206).
///
/// Delays on nets, continuous assignments, and gates do not affect
/// synthesized hardware, and the IR models connections as zero-delay, so the
/// delay is dropped; the warning keeps that visible for simulation users.
/// Gate instances are not elaborated yet, so gate delays are not reported.
pub(crate) fn warn_net_delay(ctx: &ElaborationContext<'_>, what: &str, span: Span) {
    ctx.sink.emit(errors::warn_net_delay_ignored(what, span));
}

/// Analyzes a Verilog always block to determine ProcessKind and sensitivity.
///
/// `always @(...) body` maps its event control to the process sensitivity.
/// An `always` block without a top-level event control (e.g.
/// `always #5 clk = ~clk;`) is a looping process: its body is wrapped in a
/// `forever` loop (see [`lower_verilog_looping_always`]).
fn analyze_verilog_always(
    body: &v_ast::Statement,
    always_span: Span,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> (ProcessKind, Sensitivity, IrStmt) {
    // Check if the top-level statement is an EventControl
    if let v_ast::Statement::EventControl {
        sensitivity, body, ..
    } = body
    {
        let (kind, sens) = map_verilog_sensitivity(sensitivity, sig_env);
        let ir_body = lower_verilog_stmt(body, sig_env, source_db, interner, sink);
        (kind, sens, ir_body)
    } else {
        let ir_body =
            lower_verilog_looping_always(body, always_span, sig_env, source_db, interner, sink);
        (ProcessKind::Combinational, Sensitivity::All, ir_body)
    }
}

/// Maps a Verilog sensitivity list to IR ProcessKind and Sensitivity.
fn map_verilog_sensitivity(
    sens: &v_ast::SensitivityList,
    sig_env: &SignalEnv,
) -> (ProcessKind, Sensitivity) {
    match sens {
        v_ast::SensitivityList::Star => (ProcessKind::Combinational, Sensitivity::All),
        v_ast::SensitivityList::List(items) => {
            let has_edge = items.iter().any(|i| i.edge.is_some());
            if has_edge {
                let edges: Vec<_> = items
                    .iter()
                    .filter_map(|item| {
                        let sig_name = extract_signal_name(&item.signal)?;
                        let sid = sig_env.get(&sig_name).copied()?;
                        let edge = match item.edge {
                            Some(v_ast::EdgeKind::Posedge) => Edge::Posedge,
                            Some(v_ast::EdgeKind::Negedge) => Edge::Negedge,
                            None => Edge::Both,
                        };
                        Some(EdgeSensitivity { signal: sid, edge })
                    })
                    .collect();
                (ProcessKind::Sequential, Sensitivity::EdgeList(edges))
            } else {
                let sigs: Vec<_> = items
                    .iter()
                    .filter_map(|item| {
                        let sig_name = extract_signal_name(&item.signal)?;
                        sig_env.get(&sig_name).copied()
                    })
                    .collect();
                (ProcessKind::Combinational, Sensitivity::SignalList(sigs))
            }
        }
    }
}

/// Extracts the signal name from a simple identifier expression.
fn extract_signal_name(expr: &v_ast::Expr) -> Option<Ident> {
    match expr {
        v_ast::Expr::Identifier { name, .. } => Some(*name),
        _ => None,
    }
}

/// Elaborates a Verilog module instantiation.
fn elaborate_verilog_instantiation(
    inst: &v_ast::Instantiation,
    sig_env: &SignalEnv,
    cells: &mut Arena<CellId, Cell>,
    ctx: &mut ElaborationContext<'_>,
) {
    // Resolve the instantiated module (a VHDL entity matches case-insensitively)
    let module_name = ctx.registry.canonical_name(inst.module_name, ctx.interner);

    // Build parameter overrides
    let mut param_overrides: Vec<(Ident, ConstValue)> = inst
        .param_overrides
        .iter()
        .filter_map(|conn| {
            let formal = conn.formal?;
            let actual = conn.actual.as_ref()?;
            let val = const_eval::eval_verilog_expr(
                actual,
                ctx.source_db,
                ctx.interner,
                &Default::default(),
                ctx.sink,
            )?;
            Some((formal, val))
        })
        .collect();
    // A VHDL child's generics are case-insensitive (stored folded).
    let target_is_vhdl = ctx.registry.is_vhdl_entity(module_name);
    crate::binding::canonicalize_overrides(
        &mut param_overrides,
        ctx.registry.lookup(module_name).as_ref(),
        false,
        ctx.interner,
    );

    // Check cache first
    if let Some(mid) = ctx.check_cache(module_name, &param_overrides) {
        // Create instance cells for each instance
        for instance in &inst.instances {
            let connections =
                build_verilog_connections(&instance.connections, sig_env, mid, target_is_vhdl, ctx);
            cells.alloc(Cell {
                id: CellId::from_raw(0),
                name: instance.name,
                kind: CellKind::Instance {
                    module: mid,
                    params: param_overrides.clone(),
                },
                connections,
                span: instance.span,
            });
        }
        return;
    }

    // Try to elaborate the sub-module
    if !ctx.push_elab_stack(module_name, inst.span) {
        // Cycle detected — create black box
        for instance in &inst.instances {
            cells.alloc(Cell {
                id: CellId::from_raw(0),
                name: instance.name,
                kind: CellKind::BlackBox {
                    port_names: Vec::new(),
                },
                connections: Vec::new(),
                span: instance.span,
            });
        }
        return;
    }

    let mid = match ctx.registry.lookup(module_name) {
        Some(ModuleEntry::Verilog(sub_decl)) => {
            let mid = elaborate_verilog_module(sub_decl, &param_overrides, ctx);
            ctx.insert_cache(module_name, &param_overrides, mid);
            mid
        }
        Some(ModuleEntry::Sv(sub_decl)) => {
            let mid = crate::sv::elaborate_sv_module(sub_decl, &param_overrides, ctx);
            ctx.insert_cache(module_name, &param_overrides, mid);
            mid
        }
        Some(ModuleEntry::Vhdl {
            entity,
            architecture,
        }) => {
            let mid =
                crate::vhdl::elaborate_vhdl_entity(entity, architecture, &param_overrides, ctx);
            ctx.insert_cache(module_name, &param_overrides, mid);
            mid
        }
        None => {
            ctx.sink.emit(errors::error_unknown_module(
                ctx.interner.resolve(module_name),
                inst.span,
            ));
            ctx.pop_elab_stack();
            // Create black box
            for instance in &inst.instances {
                cells.alloc(Cell {
                    id: CellId::from_raw(0),
                    name: instance.name,
                    kind: CellKind::BlackBox {
                        port_names: Vec::new(),
                    },
                    connections: Vec::new(),
                    span: instance.span,
                });
            }
            return;
        }
    };
    ctx.pop_elab_stack();

    for instance in &inst.instances {
        let connections =
            build_verilog_connections(&instance.connections, sig_env, mid, target_is_vhdl, ctx);
        cells.alloc(Cell {
            id: CellId::from_raw(0),
            name: instance.name,
            kind: CellKind::Instance {
                module: mid,
                params: param_overrides.clone(),
            },
            connections,
            span: instance.span,
        });
    }
}

/// Builds IR connections from Verilog port connections, looking up actual port
/// directions from the target module.
fn build_verilog_connections(
    connections: &[v_ast::Connection],
    sig_env: &SignalEnv,
    target_module: ModuleId,
    target_is_vhdl: bool,
    ctx: &ElaborationContext<'_>,
) -> Vec<Connection> {
    connections
        .iter()
        .filter_map(|conn| {
            // Unconnected ports (`.p()`) produce no connection.
            let actual = conn.actual.as_ref()?;
            // A VHDL child's (folded) port names match case-insensitively.
            let formal = crate::binding::bind_port(
                target_module,
                conn.formal?,
                target_is_vhdl,
                conn.span,
                ctx,
            )?;
            let signal =
                lower_to_signal_ref(actual, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            let direction = lookup_port_direction(target_module, formal, ctx);
            Some(Connection {
                port_name: formal,
                direction,
                signal,
            })
        })
        .collect()
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
    fn elaborate_empty_module() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("empty");
        let decl = v_ast::ModuleDecl {
            name,
            port_style: v_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![],
            span: Span::DUMMY,
        };
        let file = v_ast::VerilogSourceFile {
            items: vec![v_ast::VerilogItem::Module(decl.clone())],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&files, &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_verilog_module(&decl, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].ports.len(), 0);
        assert_eq!(ctx.design.modules[mid].signals.len(), 0);
    }

    #[test]
    fn elaborate_module_with_ports() {
        let (interner, source_db, sink) = setup();
        let mod_name = interner.get_or_intern("counter");
        let clk = interner.get_or_intern("clk");
        let q = interner.get_or_intern("q");

        let decl = v_ast::ModuleDecl {
            name: mod_name,
            port_style: v_ast::PortStyle::Ansi,
            params: vec![],
            ports: vec![
                v_ast::PortDecl {
                    direction: Direction::Input,
                    net_type: None,
                    signed: false,
                    range: None,
                    names: vec![clk],
                    span: Span::DUMMY,
                },
                v_ast::PortDecl {
                    direction: Direction::Output,
                    net_type: None,
                    signed: false,
                    range: None,
                    names: vec![q],
                    span: Span::DUMMY,
                },
            ],
            port_names: vec![],
            items: vec![],
            span: Span::DUMMY,
        };
        let file = v_ast::VerilogSourceFile {
            items: vec![v_ast::VerilogItem::Module(decl.clone())],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&files, &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_verilog_module(&decl, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].ports.len(), 2);
        assert_eq!(ctx.design.modules[mid].signals.len(), 2);
    }

    #[test]
    fn elaborate_module_with_wire_reg() {
        let (interner, source_db, sink) = setup();
        let mod_name = interner.get_or_intern("test");
        let w = interner.get_or_intern("w");
        let r = interner.get_or_intern("r");

        let decl = v_ast::ModuleDecl {
            name: mod_name,
            port_style: v_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![
                v_ast::ModuleItem::NetDecl(v_ast::NetDecl {
                    net_type: v_ast::NetType::Wire,
                    signed: false,
                    range: None,
                    delay: None,
                    names: vec![v_ast::DeclName {
                        name: w,
                        dimensions: vec![],
                        init: None,
                        span: Span::DUMMY,
                    }],
                    span: Span::DUMMY,
                }),
                v_ast::ModuleItem::RegDecl(v_ast::RegDecl {
                    signed: false,
                    range: None,
                    names: vec![v_ast::DeclName {
                        name: r,
                        dimensions: vec![],
                        init: None,
                        span: Span::DUMMY,
                    }],
                    span: Span::DUMMY,
                }),
            ],
            span: Span::DUMMY,
        };
        let file = v_ast::VerilogSourceFile {
            items: vec![v_ast::VerilogItem::Module(decl.clone())],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&files, &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_verilog_module(&decl, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].signals.len(), 2);
    }

    #[test]
    fn elaborate_continuous_assign() {
        let (interner, source_db, sink) = setup();
        let mod_name = interner.get_or_intern("test");
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");

        let decl = v_ast::ModuleDecl {
            name: mod_name,
            port_style: v_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![
                v_ast::ModuleItem::NetDecl(v_ast::NetDecl {
                    net_type: v_ast::NetType::Wire,
                    signed: false,
                    range: None,
                    delay: None,
                    names: vec![
                        v_ast::DeclName {
                            name: a,
                            dimensions: vec![],
                            init: None,
                            span: Span::DUMMY,
                        },
                        v_ast::DeclName {
                            name: b,
                            dimensions: vec![],
                            init: None,
                            span: Span::DUMMY,
                        },
                    ],
                    span: Span::DUMMY,
                }),
                v_ast::ModuleItem::ContinuousAssign(v_ast::ContinuousAssign {
                    delay: None,
                    target: v_ast::Expr::Identifier {
                        name: a,
                        span: Span::DUMMY,
                    },
                    value: v_ast::Expr::Identifier {
                        name: b,
                        span: Span::DUMMY,
                    },
                    span: Span::DUMMY,
                }),
            ],
            span: Span::DUMMY,
        };
        let file = v_ast::VerilogSourceFile {
            items: vec![v_ast::VerilogItem::Module(decl.clone())],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&files, &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_verilog_module(&decl, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].assignments.len(), 1);
    }

    #[test]
    fn elaborate_always_block() {
        let (interner, source_db, sink) = setup();
        let mod_name = interner.get_or_intern("test");
        let clk = interner.get_or_intern("clk");

        let decl = v_ast::ModuleDecl {
            name: mod_name,
            port_style: v_ast::PortStyle::Ansi,
            params: vec![],
            ports: vec![v_ast::PortDecl {
                direction: Direction::Input,
                net_type: None,
                signed: false,
                range: None,
                names: vec![clk],
                span: Span::DUMMY,
            }],
            port_names: vec![],
            items: vec![v_ast::ModuleItem::AlwaysBlock(v_ast::AlwaysBlock {
                body: v_ast::Statement::EventControl {
                    sensitivity: v_ast::SensitivityList::List(vec![v_ast::SensitivityItem {
                        edge: Some(v_ast::EdgeKind::Posedge),
                        signal: v_ast::Expr::Identifier {
                            name: clk,
                            span: Span::DUMMY,
                        },
                        span: Span::DUMMY,
                    }]),
                    body: Box::new(v_ast::Statement::Null { span: Span::DUMMY }),
                    span: Span::DUMMY,
                },
                span: Span::DUMMY,
            })],
            span: Span::DUMMY,
        };
        let file = v_ast::VerilogSourceFile {
            items: vec![v_ast::VerilogItem::Module(decl.clone())],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&files, &[], &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_verilog_module(&decl, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].processes.len(), 1);
    }
}
