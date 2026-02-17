//! SystemVerilog-2017 module elaboration.
//!
//! Transforms a parsed [`SvModuleDecl`](aion_sv_parser::ast::SvModuleDecl) into
//! an IR [`Module`](aion_ir::module::Module), handling SV-specific constructs like
//! `always_comb`, `always_ff`, variable declarations, and compound assignments.

use std::collections::HashMap;

use aion_common::{ContentHash, Ident};
use aion_ir::arena::Arena;
use aion_ir::cell::{Cell, CellKind, Connection};
use aion_ir::ids::{CellId, ModuleId, ProcessId, SignalId, TypeId};
use aion_ir::module::{Assignment, Module, Parameter};
use aion_ir::port::{Port, PortDirection};
use aion_ir::process::{Edge, EdgeSensitivity, Process, ProcessKind, Sensitivity};
use aion_ir::signal::{Signal, SignalKind};
use aion_ir::stmt::Statement as IrStmt;
use aion_ir::ConstValue;
use aion_sv_parser::ast::{self as sv_ast, Direction};

use crate::const_eval::{self, ConstEnv};
use crate::context::ElaborationContext;
use crate::errors;
use crate::expr::{lower_sv_expr, lower_sv_to_signal_ref, SignalEnv};
use crate::registry::ModuleEntry;
use crate::stmt::lower_sv_stmt;
use crate::types;

/// Elaborates a SystemVerilog module declaration into an IR module.
pub fn elaborate_sv_module(
    decl: &sv_ast::SvModuleDecl,
    param_overrides: &[(Ident, ConstValue)],
    ctx: &mut ElaborationContext<'_>,
) -> ModuleId {
    let mut const_env = ConstEnv::new();
    let mut ir_params = Vec::new();
    apply_sv_params(decl, param_overrides, &mut const_env, &mut ir_params, ctx);

    let mut signals: Arena<SignalId, Signal> = Arena::new();
    let mut sig_env = SignalEnv::new();
    let mut ports = Vec::new();

    elaborate_sv_ports(
        decl,
        &const_env,
        &mut signals,
        &mut sig_env,
        &mut ports,
        ctx,
    );

    let mut cells: Arena<CellId, Cell> = Arena::new();
    let mut processes: Arena<ProcessId, Process> = Arena::new();
    let mut assignments = Vec::new();

    for item in &decl.items {
        elaborate_sv_item(
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
        id: ModuleId::from_raw(0),
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

/// Applies parameter declarations and overrides.
fn apply_sv_params(
    decl: &sv_ast::SvModuleDecl,
    overrides: &[(Ident, ConstValue)],
    const_env: &mut ConstEnv,
    ir_params: &mut Vec<Parameter>,
    ctx: &mut ElaborationContext<'_>,
) {
    let override_map: HashMap<_, _> = overrides.iter().cloned().collect();

    for param in &decl.params {
        let name = param.name;
        let value = if let Some(ov) = override_map.get(&name) {
            ov.clone()
        } else if let Some(ref value) = param.value {
            const_eval::eval_sv_expr(value, ctx.source_db, ctx.interner, const_env, ctx.sink)
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

/// Elaborates the port list of a SystemVerilog module.
fn elaborate_sv_ports(
    decl: &sv_ast::SvModuleDecl,
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
        let ty = types::resolve_sv_type(
            &port_decl.port_type,
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

/// Elaborates a single SystemVerilog module item.
#[allow(clippy::too_many_arguments)]
fn elaborate_sv_item(
    item: &sv_ast::ModuleItem,
    const_env: &ConstEnv,
    signals: &mut Arena<SignalId, Signal>,
    sig_env: &mut SignalEnv,
    cells: &mut Arena<CellId, Cell>,
    processes: &mut Arena<ProcessId, Process>,
    assignments: &mut Vec<Assignment>,
    ctx: &mut ElaborationContext<'_>,
) {
    match item {
        sv_ast::ModuleItem::NetDecl(net) => {
            let ty = types::resolve_sv_range(
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
        sv_ast::ModuleItem::RegDecl(reg) => {
            let ty = types::resolve_sv_range(
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
        sv_ast::ModuleItem::VarDecl(vd) => {
            let ty = types::resolve_sv_var_type(
                &vd.var_type,
                vd.range.as_ref(),
                vd.signed,
                &mut ctx.design.types,
                const_env,
                ctx.source_db,
                ctx.interner,
                ctx.sink,
            );
            for dn in &vd.names {
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
        sv_ast::ModuleItem::IntegerDecl(idecl) => {
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
        sv_ast::ModuleItem::RealDecl(rdecl) => {
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
        sv_ast::ModuleItem::ParameterDecl(_) | sv_ast::ModuleItem::LocalparamDecl(_) => {}
        sv_ast::ModuleItem::PortDecl(_) => {}
        sv_ast::ModuleItem::ContinuousAssign(ca) => {
            let target =
                lower_sv_to_signal_ref(&ca.target, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            let value = lower_sv_expr(&ca.value, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            assignments.push(Assignment {
                target,
                value,
                span: ca.span,
            });
        }
        sv_ast::ModuleItem::AlwaysBlock(ab) => {
            let (kind, sensitivity, body_stmt) =
                analyze_sv_always(&ab.body, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: None,
                kind,
                body: body_stmt,
                sensitivity,
                span: ab.span,
            });
        }
        sv_ast::ModuleItem::AlwaysComb(ac) => {
            let body = lower_sv_stmt(&ac.body, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: None,
                kind: ProcessKind::Combinational,
                body,
                sensitivity: Sensitivity::All,
                span: ac.span,
            });
        }
        sv_ast::ModuleItem::AlwaysFf(af) => {
            let sensitivity = map_sv_sensitivity(&af.sensitivity, sig_env);
            let body = lower_sv_stmt(&af.body, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: None,
                kind: ProcessKind::Sequential,
                body,
                sensitivity,
                span: af.span,
            });
        }
        sv_ast::ModuleItem::AlwaysLatch(al) => {
            let body = lower_sv_stmt(&al.body, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: None,
                kind: ProcessKind::Latched,
                body,
                sensitivity: Sensitivity::All,
                span: al.span,
            });
        }
        sv_ast::ModuleItem::InitialBlock(ib) => {
            let body = lower_sv_stmt(&ib.body, sig_env, ctx.source_db, ctx.interner, ctx.sink);
            processes.alloc(Process {
                id: ProcessId::from_raw(0),
                name: None,
                kind: ProcessKind::Initial,
                body,
                sensitivity: Sensitivity::All,
                span: ib.span,
            });
        }
        sv_ast::ModuleItem::Instantiation(inst) => {
            elaborate_sv_instantiation(inst, sig_env, cells, ctx);
        }
        sv_ast::ModuleItem::GenerateBlock(_)
        | sv_ast::ModuleItem::GateInst(_)
        | sv_ast::ModuleItem::GenvarDecl(_)
        | sv_ast::ModuleItem::FunctionDecl(_)
        | sv_ast::ModuleItem::TaskDecl(_)
        | sv_ast::ModuleItem::DefparamDecl(_)
        | sv_ast::ModuleItem::TypedefDecl(_)
        | sv_ast::ModuleItem::Import(_)
        | sv_ast::ModuleItem::Assertion(_)
        | sv_ast::ModuleItem::ModportDecl(_) => {}
        sv_ast::ModuleItem::Error(_) => {}
    }
}

/// Analyzes an SV `always` block to determine ProcessKind and sensitivity.
fn analyze_sv_always(
    body: &sv_ast::Statement,
    sig_env: &SignalEnv,
    source_db: &aion_source::SourceDb,
    interner: &aion_common::Interner,
    sink: &aion_diagnostics::DiagnosticSink,
) -> (ProcessKind, Sensitivity, IrStmt) {
    if let sv_ast::Statement::EventControl {
        sensitivity, body, ..
    } = body
    {
        let (kind, sens) = map_sv_always_sensitivity(sensitivity, sig_env);
        let ir_body = lower_sv_stmt(body, sig_env, source_db, interner, sink);
        (kind, sens, ir_body)
    } else {
        let ir_body = lower_sv_stmt(body, sig_env, source_db, interner, sink);
        (ProcessKind::Combinational, Sensitivity::All, ir_body)
    }
}

/// Maps an SV sensitivity list from an `always` block to ProcessKind and Sensitivity.
fn map_sv_always_sensitivity(
    sens: &sv_ast::SensitivityList,
    sig_env: &SignalEnv,
) -> (ProcessKind, Sensitivity) {
    match sens {
        sv_ast::SensitivityList::Star => (ProcessKind::Combinational, Sensitivity::All),
        sv_ast::SensitivityList::List(items) => {
            let has_edge = items.iter().any(|i| i.edge.is_some());
            if has_edge {
                let edges: Vec<_> = items
                    .iter()
                    .filter_map(|item| {
                        let sig_name = extract_sv_signal_name(&item.signal)?;
                        let sid = sig_env.get(&sig_name).copied()?;
                        let edge = match item.edge {
                            Some(sv_ast::EdgeKind::Posedge) => Edge::Posedge,
                            Some(sv_ast::EdgeKind::Negedge) => Edge::Negedge,
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
                        let sig_name = extract_sv_signal_name(&item.signal)?;
                        sig_env.get(&sig_name).copied()
                    })
                    .collect();
                (ProcessKind::Combinational, Sensitivity::SignalList(sigs))
            }
        }
    }
}

/// Maps an SV sensitivity list to IR Sensitivity.
fn map_sv_sensitivity(sens: &sv_ast::SensitivityList, sig_env: &SignalEnv) -> Sensitivity {
    match sens {
        sv_ast::SensitivityList::Star => Sensitivity::All,
        sv_ast::SensitivityList::List(items) => {
            let edges: Vec<_> = items
                .iter()
                .filter_map(|item| {
                    let sig_name = extract_sv_signal_name(&item.signal)?;
                    let sid = sig_env.get(&sig_name).copied()?;
                    let edge = match item.edge {
                        Some(sv_ast::EdgeKind::Posedge) => Edge::Posedge,
                        Some(sv_ast::EdgeKind::Negedge) => Edge::Negedge,
                        None => Edge::Both,
                    };
                    Some(EdgeSensitivity { signal: sid, edge })
                })
                .collect();
            Sensitivity::EdgeList(edges)
        }
    }
}

/// Extracts signal name from an SV expression.
fn extract_sv_signal_name(expr: &sv_ast::Expr) -> Option<Ident> {
    match expr {
        sv_ast::Expr::Identifier { name, .. } => Some(*name),
        _ => None,
    }
}

/// Elaborates an SV module instantiation.
fn elaborate_sv_instantiation(
    inst: &sv_ast::Instantiation,
    sig_env: &SignalEnv,
    cells: &mut Arena<CellId, Cell>,
    ctx: &mut ElaborationContext<'_>,
) {
    let module_name = inst.module_name;
    let param_overrides: Vec<(Ident, ConstValue)> = inst
        .param_overrides
        .iter()
        .filter_map(|conn| {
            let formal = conn.formal?;
            let actual = conn.actual.as_ref()?;
            let val = const_eval::eval_sv_expr(
                actual,
                ctx.source_db,
                ctx.interner,
                &Default::default(),
                ctx.sink,
            )?;
            Some((formal, val))
        })
        .collect();

    if let Some(mid) = ctx.check_cache(module_name, &param_overrides) {
        for instance in &inst.instances {
            let connections = build_sv_connections(&instance.connections, sig_env, mid, ctx);
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

    if !ctx.push_elab_stack(module_name, inst.span) {
        for instance in &inst.instances {
            cells.alloc(Cell {
                id: CellId::from_raw(0),
                name: instance.name,
                kind: CellKind::BlackBox { port_names: vec![] },
                connections: vec![],
                span: instance.span,
            });
        }
        return;
    }

    let mid = match ctx.registry.lookup(module_name) {
        Some(ModuleEntry::Verilog(sub_decl)) => {
            let mid = crate::verilog::elaborate_verilog_module(sub_decl, &param_overrides, ctx);
            ctx.insert_cache(module_name, &param_overrides, mid);
            mid
        }
        Some(ModuleEntry::Sv(sub_decl)) => {
            let mid = elaborate_sv_module(sub_decl, &param_overrides, ctx);
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
            for instance in &inst.instances {
                cells.alloc(Cell {
                    id: CellId::from_raw(0),
                    name: instance.name,
                    kind: CellKind::BlackBox { port_names: vec![] },
                    connections: vec![],
                    span: instance.span,
                });
            }
            return;
        }
    };
    ctx.pop_elab_stack();

    for instance in &inst.instances {
        let connections = build_sv_connections(&instance.connections, sig_env, mid, ctx);
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

/// Builds IR connections from SV port connections, looking up actual port
/// directions from the target module.
fn build_sv_connections(
    connections: &[sv_ast::Connection],
    sig_env: &SignalEnv,
    target_module: ModuleId,
    ctx: &ElaborationContext<'_>,
) -> Vec<Connection> {
    connections
        .iter()
        .filter_map(|conn| {
            let formal = conn.formal?;
            let signal = if let Some(ref actual) = conn.actual {
                lower_sv_to_signal_ref(actual, sig_env, ctx.source_db, ctx.interner, ctx.sink)
            } else {
                return None;
            };
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
    fn elaborate_empty_sv_module() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("empty");
        let decl = sv_ast::SvModuleDecl {
            name,
            port_style: sv_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![],
            end_label: None,
            span: Span::DUMMY,
        };
        let file = sv_ast::SvSourceFile {
            items: vec![sv_ast::SvItem::Module(decl.clone())],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &files, &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_sv_module(&decl, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].ports.len(), 0);
    }

    #[test]
    fn elaborate_sv_with_always_comb() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("test");
        let decl = sv_ast::SvModuleDecl {
            name,
            port_style: sv_ast::PortStyle::Empty,
            params: vec![],
            ports: vec![],
            port_names: vec![],
            items: vec![sv_ast::ModuleItem::AlwaysComb(sv_ast::AlwaysCombBlock {
                body: sv_ast::Statement::Null { span: Span::DUMMY },
                span: Span::DUMMY,
            })],
            end_label: None,
            span: Span::DUMMY,
        };
        let file = sv_ast::SvSourceFile {
            items: vec![sv_ast::SvItem::Module(decl.clone())],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &files, &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_sv_module(&decl, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].processes.len(), 1);
    }

    #[test]
    fn elaborate_sv_with_logic_port() {
        let (interner, source_db, sink) = setup();
        let name = interner.get_or_intern("test");
        let clk = interner.get_or_intern("clk");
        let decl = sv_ast::SvModuleDecl {
            name,
            port_style: sv_ast::PortStyle::Ansi,
            params: vec![],
            ports: vec![sv_ast::SvPortDecl {
                direction: Direction::Input,
                port_type: sv_ast::SvPortType::Var(sv_ast::VarType::Logic),
                signed: false,
                range: None,
                names: vec![clk],
                span: Span::DUMMY,
            }],
            port_names: vec![],
            items: vec![],
            end_label: None,
            span: Span::DUMMY,
        };
        let file = sv_ast::SvSourceFile {
            items: vec![sv_ast::SvItem::Module(decl.clone())],
            span: Span::DUMMY,
        };
        let files = [file];
        let reg = ModuleRegistry::from_parsed_design(&[], &files, &[], &interner, &sink);
        let mut ctx = ElaborationContext::new(&reg, &interner, &source_db, &sink);
        let mid = elaborate_sv_module(&decl, &[], &mut ctx);
        assert_eq!(ctx.design.modules[mid].ports.len(), 1);
        assert_eq!(ctx.design.modules[mid].signals.len(), 1);
    }
}
