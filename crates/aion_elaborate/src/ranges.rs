//! VHDL discrete-range resolution.
//!
//! Turns a parsed [`DiscreteRange`] — an explicit range (`7 downto 0`), a
//! constrained subtype (`integer range 0 to 3`), a type mark, or a range
//! attribute name (`v'range`, `T'reverse_range`, `m'range(2)`) — into a
//! concrete [`ResolvedRange`] with integer bounds and a direction.
//!
//! Range attributes need the declared index ranges of the objects and types
//! in scope. Those are tracked in a [`RangeEnv`], which the VHDL elaborator
//! fills as it walks port, signal, constant, variable, type and subtype
//! declarations (see [`RangeEnv::declare`]).
//!
//! A range attribute whose prefix cannot be resolved is always reported
//! (`E212`) and yields `None`: picking a guessed range would silently build
//! the wrong hardware.

use std::collections::HashMap;

use aion_common::{Ident, Interner};
use aion_diagnostics::DiagnosticSink;
use aion_source::SourceDb;
use aion_vhdl_parser::ast::{
    self as vhdl_ast, Constraint, DiscreteRange, RangeAttributeKind, RangeConstraint,
    RangeDirection, TypeDef, TypeIndication,
};

use crate::const_eval::{self, ConstEnv};
use crate::errors;

/// A fully evaluated VHDL range: `left to right` or `left downto right`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRange {
    /// The left bound (the first index visited when iterating).
    pub left: i64,
    /// The right bound (the last index visited when iterating).
    pub right: i64,
    /// Whether the range ascends (`to`) or descends (`downto`).
    pub direction: RangeDirection,
}

impl ResolvedRange {
    /// Creates the range `left <direction> right`.
    pub fn new(left: i64, direction: RangeDirection, right: i64) -> Self {
        Self {
            left,
            right,
            direction,
        }
    }

    /// Returns the range as `'reverse_range` would: bounds swapped and the
    /// direction flipped, so `7 downto 0` becomes `0 to 7`.
    pub fn reversed(self) -> Self {
        let direction = match self.direction {
            RangeDirection::To => RangeDirection::Downto,
            RangeDirection::Downto => RangeDirection::To,
        };
        Self::new(self.right, direction, self.left)
    }

    /// Returns `true` for a null range (e.g. `3 to 0` or `0 downto 3`),
    /// which contains no indices.
    pub fn is_empty(&self) -> bool {
        match self.direction {
            RangeDirection::To => self.left > self.right,
            RangeDirection::Downto => self.left < self.right,
        }
    }

    /// Returns the number of indices in the range (0 for a null range).
    pub fn len(&self) -> u64 {
        if self.is_empty() {
            0
        } else {
            self.left.abs_diff(self.right) + 1
        }
    }

    /// Returns the indices of the range in iteration order: ascending for
    /// `to`, descending for `downto`, and nothing for a null range.
    pub fn indices(&self) -> impl Iterator<Item = i64> {
        let (left, right, direction) = (self.left, self.right, self.direction);
        let mut next = (!self.is_empty()).then_some(left);
        std::iter::from_fn(move || {
            let current = next?;
            next = if current == right {
                None
            } else {
                match direction {
                    RangeDirection::To => Some(current + 1),
                    RangeDirection::Downto => Some(current - 1),
                }
            };
            Some(current)
        })
    }
}

/// What the elaborator knows about the index ranges of a declared name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredRange {
    /// An array object or array (sub)type, with one index range per
    /// dimension, in declaration order.
    Array(Vec<ResolvedRange>),
    /// A scalar, record or other non-array object or type. Holds the scalar
    /// range when it is known (e.g. `subtype idx_t is integer range 0 to 3`),
    /// so the type mark can itself be used as a discrete range.
    NotArray(Option<ResolvedRange>),
    /// The name is declared but its index ranges cannot be determined at
    /// elaboration time (unconstrained or unsupported type, non-constant
    /// bounds).
    Unknown,
}

/// The declared index ranges of the objects and types in scope, keyed by
/// name. Objects (signals, ports, constants, variables) and types share one
/// namespace, as they do in a VHDL declarative region.
#[derive(Debug, Clone, Default)]
pub struct RangeEnv {
    entries: HashMap<Ident, DeclaredRange>,
}

/// Built-in scalar types whose objects are never arrays.
const SCALAR_TYPES: &[&str] = &[
    "std_logic",
    "std_ulogic",
    "bit",
    "boolean",
    "integer",
    "natural",
    "positive",
    "real",
    "character",
    "time",
];

impl RangeEnv {
    /// Creates an empty environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records (or shadows) what is known about `name`'s index ranges.
    pub fn insert(&mut self, name: Ident, range: DeclaredRange) {
        self.entries.insert(name, range);
    }

    /// Forgets `name` (e.g. when a loop parameter's scope ends and nothing
    /// was shadowed).
    pub fn remove(&mut self, name: Ident) {
        self.entries.remove(&name);
    }

    /// Looks up a declared name; `None` if nothing named `name` is in scope.
    pub fn get(&self, name: Ident) -> Option<&DeclaredRange> {
        self.entries.get(&name)
    }

    /// Records an object of subtype `ty` under each of `names`.
    pub fn declare_object(
        &mut self,
        names: &[Ident],
        ty: &TypeIndication,
        const_env: &ConstEnv,
        source_db: &SourceDb,
        interner: &Interner,
    ) {
        let range = self.classify(ty, const_env, source_db, interner);
        for &name in names {
            self.insert(name, range.clone());
        }
    }

    /// Records the names introduced by a VHDL declaration: objects
    /// (signal, constant, variable) get their subtype's ranges, and type and
    /// subtype declarations record the ranges of the new type. Other
    /// declarations are ignored.
    pub fn declare(
        &mut self,
        decl: &vhdl_ast::Declaration,
        const_env: &ConstEnv,
        source_db: &SourceDb,
        interner: &Interner,
    ) {
        use vhdl_ast::Declaration;
        match decl {
            Declaration::Signal(sd) => {
                self.declare_object(&sd.names, &sd.ty, const_env, source_db, interner)
            }
            Declaration::Constant(cd) => {
                self.declare_object(&cd.names, &cd.ty, const_env, source_db, interner)
            }
            Declaration::Variable(vd) => {
                self.declare_object(&vd.names, &vd.ty, const_env, source_db, interner)
            }
            Declaration::Subtype(st) => {
                let range = self.classify(&st.ty, const_env, source_db, interner);
                self.insert(st.name, range);
            }
            Declaration::Type(td) => {
                let range = match &td.def {
                    TypeDef::Range { constraint, .. } => DeclaredRange::NotArray(eval_quiet(
                        constraint, const_env, source_db, interner,
                    )),
                    TypeDef::Array { indices, .. } => {
                        self.array_ranges(indices, const_env, source_db, interner)
                    }
                    TypeDef::Enumeration { .. } | TypeDef::Record { .. } => {
                        DeclaredRange::NotArray(None)
                    }
                    TypeDef::Incomplete { .. } => DeclaredRange::Unknown,
                };
                self.insert(td.name, range);
            }
            _ => {}
        }
    }

    /// Works out what a subtype indication says about index ranges.
    ///
    /// Evaluation is quiet: a bound that is not constant just makes the
    /// result [`DeclaredRange::Unknown`]. The type resolver reports the
    /// problem for the declaration itself, and a later `'range` use of the
    /// object reports that its range is unknown.
    fn classify(
        &self,
        ty: &TypeIndication,
        const_env: &ConstEnv,
        source_db: &SourceDb,
        interner: &Interner,
    ) -> DeclaredRange {
        match &ty.constraint {
            Some(Constraint::Index(indices, _)) => {
                self.array_ranges(indices, const_env, source_db, interner)
            }
            Some(Constraint::Range(rc)) => {
                DeclaredRange::NotArray(eval_quiet(rc, const_env, source_db, interner))
            }
            None => {
                let Some(&mark) = ty.type_mark.parts.last() else {
                    return DeclaredRange::Unknown;
                };
                if let Some(known) = self.get(mark) {
                    return known.clone();
                }
                let name = interner.resolve(mark).to_ascii_lowercase();
                if SCALAR_TYPES.contains(&name.as_str()) {
                    DeclaredRange::NotArray(None)
                } else {
                    DeclaredRange::Unknown
                }
            }
        }
    }

    /// Evaluates the index ranges of an array declaration, quietly.
    fn array_ranges(
        &self,
        indices: &[DiscreteRange],
        const_env: &ConstEnv,
        source_db: &SourceDb,
        interner: &Interner,
    ) -> DeclaredRange {
        let quiet = DiagnosticSink::new();
        let ranges: Option<Vec<_>> = indices
            .iter()
            .map(|dr| resolve_vhdl_discrete_range(dr, self, const_env, source_db, interner, &quiet))
            .collect();
        match ranges {
            Some(ranges) if !ranges.is_empty() => DeclaredRange::Array(ranges),
            _ => DeclaredRange::Unknown,
        }
    }
}

/// Evaluates a range constraint without reporting failures.
fn eval_quiet(
    rc: &RangeConstraint,
    const_env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
) -> Option<ResolvedRange> {
    eval_range_constraint(rc, const_env, source_db, interner, &DiagnosticSink::new())
}

/// Evaluates both bounds of an explicit range constraint to integers.
///
/// Bounds that are not constant are reported by the constant evaluator
/// (`E209`) and yield `None`.
pub fn eval_range_constraint(
    rc: &RangeConstraint,
    const_env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> Option<ResolvedRange> {
    let left = const_eval::eval_vhdl_expr(&rc.left, source_db, interner, const_env, sink)?;
    let right = const_eval::eval_vhdl_expr(&rc.right, source_db, interner, const_env, sink)?;
    Some(ResolvedRange::new(
        const_eval::const_to_i64(&left)?,
        rc.direction,
        const_eval::const_to_i64(&right)?,
    ))
}

/// Resolves a VHDL discrete range to concrete bounds and direction.
///
/// - `L to R` / `L downto R`: both bounds are constant-evaluated (failures
///   are reported as `E209` by the constant evaluator).
/// - `T range L to R`: the range constraint is evaluated the same way.
/// - A bare type mark `T`: the scalar range recorded for `T` in `ranges`,
///   if any. Returns `None` without a diagnostic otherwise (e.g. an
///   enumeration type, which has no integer range).
/// - `x'range` / `x'reverse_range` (optionally `x'range(N)`): the `N`th
///   (default first) index range of the array object or type `x`, reversed
///   for `'reverse_range`. Every failure — unknown prefix, non-array prefix,
///   unknown range, bad dimension, unsupported prefix form — is reported as
///   `E212`.
/// - An [`DiscreteRange::Error`] node returns `None` silently; the parser
///   already reported it.
pub fn resolve_vhdl_discrete_range(
    range: &DiscreteRange,
    ranges: &RangeEnv,
    const_env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> Option<ResolvedRange> {
    match range {
        DiscreteRange::Range(rc) => eval_range_constraint(rc, const_env, source_db, interner, sink),
        DiscreteRange::TypeIndication(ti) => match &ti.constraint {
            Some(Constraint::Range(rc)) => {
                eval_range_constraint(rc, const_env, source_db, interner, sink)
            }
            Some(Constraint::Index(..)) => None,
            None => match ranges.get(*ti.type_mark.parts.last()?) {
                Some(DeclaredRange::NotArray(Some(r))) => Some(*r),
                _ => None,
            },
        },
        DiscreteRange::Attribute(attr) => {
            resolve_range_attribute(attr, ranges, const_env, source_db, interner, sink)
        }
        DiscreteRange::Error(_) => None,
    }
}

/// Resolves `prefix'range[(N)]` / `prefix'reverse_range[(N)]`, reporting
/// every failure as `E212`.
fn resolve_range_attribute(
    attr: &vhdl_ast::RangeAttributeName,
    ranges: &RangeEnv,
    const_env: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> Option<ResolvedRange> {
    let attr_name = match attr.kind {
        RangeAttributeKind::Range => "range",
        RangeAttributeKind::ReverseRange => "reverse_range",
    };
    let prefix_text = source_db.snippet(attr.prefix.span).to_string();
    let fail = |reason: &str| {
        sink.emit(errors::error_unresolved_range_attribute(
            attr_name,
            &prefix_text,
            reason,
            attr.span,
        ));
        None
    };

    if !attr.prefix.parts.is_empty() {
        return fail(
            "only a simple name of a signal, port, constant, variable or type is supported \
             as the prefix",
        );
    }
    let dims =
        match ranges.get(attr.prefix.primary) {
            None => return fail(
                "no signal, port, constant, variable or type with this name is declared in scope",
            ),
            Some(DeclaredRange::NotArray(_)) => return fail("the prefix is not an array"),
            Some(DeclaredRange::Unknown) => {
                return fail("its index range cannot be determined at elaboration time")
            }
            Some(DeclaredRange::Array(dims)) => dims,
        };

    let dimension = match &attr.dimension {
        None => 1,
        Some(expr) => {
            let value = const_eval::eval_vhdl_expr(expr, source_db, interner, const_env, sink)?;
            match const_eval::const_to_i64(&value) {
                Some(n) => n,
                None => return fail("the dimension argument is not an integer constant"),
            }
        }
    };
    let Some(range) = usize::try_from(dimension)
        .ok()
        .and_then(|d| d.checked_sub(1))
        .and_then(|i| dims.get(i))
    else {
        return fail(&format!(
            "dimension {dimension} does not exist; the prefix has {} dimension(s)",
            dims.len()
        ));
    };

    Some(match attr.kind {
        RangeAttributeKind::Range => *range,
        RangeAttributeKind::ReverseRange => range.reversed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_diagnostics::Diagnostic;
    use vhdl_ast::{
        ArchitectureDecl, ConcurrentStatement, DesignUnit, DesignUnitKind, SequentialStatement,
        VhdlDesignFile,
    };

    use RangeDirection::{Downto, To};

    fn parse(src: &str) -> (VhdlDesignFile, SourceDb, Interner) {
        let interner = Interner::new();
        let mut source_db = SourceDb::new();
        let sink = DiagnosticSink::new();
        let file_id = source_db.add_source("t.vhd", src.to_string());
        let ast = aion_vhdl_parser::parse_file(file_id, &source_db, &interner, &sink);
        assert!(!sink.has_errors(), "parse errors: {:?}", sink.diagnostics());
        (ast, source_db, interner)
    }

    fn architecture(ast: &VhdlDesignFile) -> &ArchitectureDecl {
        ast.units
            .iter()
            .find_map(|u| match u {
                DesignUnit::ContextUnit {
                    unit: DesignUnitKind::Architecture(a),
                    ..
                } => Some(a),
                _ => None,
            })
            .expect("architecture")
    }

    /// Declares the architecture's declarations into a fresh [`RangeEnv`]
    /// (generic `N` = 4) the way the elaborator does, then resolves the
    /// range of every for-loop in the first process and every for-generate,
    /// in source order. Returns the resolved ranges and the diagnostics.
    fn resolve_loops(decls: &str, body: &str) -> (Vec<Option<ResolvedRange>>, Vec<Diagnostic>) {
        let src = format!("architecture rtl of t is\n{decls}\nbegin\n{body}\nend architecture;");
        let (ast, source_db, interner) = parse(&src);
        let arch = architecture(&ast);
        let mut const_env = ConstEnv::new();
        const_env.insert(interner.get_or_intern("n"), aion_ir::ConstValue::Int(4));
        let mut env = RangeEnv::new();
        for decl in &arch.decls {
            env.declare(decl, &const_env, &source_db, &interner);
        }
        let sink = DiagnosticSink::new();
        let mut out = Vec::new();
        for stmt in &arch.stmts {
            match stmt {
                ConcurrentStatement::Process(p) => {
                    for s in &p.stmts {
                        if let SequentialStatement::ForLoop(fl) = s {
                            out.push(resolve_vhdl_discrete_range(
                                &fl.range, &env, &const_env, &source_db, &interner, &sink,
                            ));
                        }
                    }
                }
                ConcurrentStatement::ForGenerate(fg) => out.push(resolve_vhdl_discrete_range(
                    &fg.range, &env, &const_env, &source_db, &interner, &sink,
                )),
                _ => {}
            }
        }
        (out, sink.take_all())
    }

    fn indices(r: Option<ResolvedRange>) -> Vec<i64> {
        r.expect("range resolved").indices().collect()
    }

    #[test]
    fn indices_follow_direction() {
        let down = ResolvedRange::new(3, Downto, 0);
        assert_eq!(down.indices().collect::<Vec<_>>(), vec![3, 2, 1, 0]);
        let up = ResolvedRange::new(-1, To, 2);
        assert_eq!(up.indices().collect::<Vec<_>>(), vec![-1, 0, 1, 2]);
        assert_eq!(down.len(), 4);
        assert_eq!(up.len(), 4);
        let single = ResolvedRange::new(5, To, 5);
        assert_eq!(single.indices().collect::<Vec<_>>(), vec![5]);
    }

    #[test]
    fn null_ranges_are_empty() {
        let null_up = ResolvedRange::new(3, To, 0);
        let null_down = ResolvedRange::new(0, Downto, 3);
        assert!(null_up.is_empty() && null_down.is_empty());
        assert_eq!(null_up.len(), 0);
        assert_eq!(null_up.indices().count(), 0);
        assert_eq!(null_down.indices().count(), 0);
    }

    #[test]
    fn reversed_swaps_bounds_and_direction() {
        let r = ResolvedRange::new(7, Downto, 0);
        assert_eq!(r.reversed(), ResolvedRange::new(0, To, 7));
        assert_eq!(r.reversed().reversed(), r);
        // Iteration order is exactly the opposite.
        let fwd: Vec<_> = r.indices().collect();
        let mut back: Vec<_> = r.reversed().indices().collect();
        back.reverse();
        assert_eq!(fwd, back);
    }

    #[test]
    fn for_loop_over_signal_range_and_reverse_range() {
        let (ranges, diags) = resolve_loops(
            "signal v : std_logic_vector(3 downto 0);\nsignal w : std_logic_vector(1 to N);",
            "process begin\n\
               for i in v'range loop null; end loop;\n\
               for i in v'reverse_range loop null; end loop;\n\
               for i in w'range loop null; end loop;\n\
               for i in w'reverse_range loop null; end loop;\n\
             end process;",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(indices(ranges[0]), vec![3, 2, 1, 0]);
        assert_eq!(indices(ranges[1]), vec![0, 1, 2, 3]);
        assert_eq!(indices(ranges[2]), vec![1, 2, 3, 4]);
        assert_eq!(indices(ranges[3]), vec![4, 3, 2, 1]);
    }

    #[test]
    fn for_generate_over_range_and_reverse_range() {
        let (ranges, diags) = resolve_loops(
            "constant K : std_logic_vector(5 downto 2) := \"0000\";",
            "g1 : for i in K'range generate end generate;\n\
             g2 : for i in K'reverse_range generate end generate;",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(indices(ranges[0]), vec![5, 4, 3, 2]);
        assert_eq!(indices(ranges[1]), vec![2, 3, 4, 5]);
    }

    #[test]
    fn type_range_attributes_and_dimensions() {
        let (ranges, diags) = resolve_loops(
            "subtype word_t is std_logic_vector(7 downto 4);\n\
             type mem_t is array (0 to 2, 3 downto 1) of std_logic;\n\
             signal m : mem_t;",
            "process begin\n\
               for i in word_t'range loop null; end loop;\n\
               for i in mem_t'range(1) loop null; end loop;\n\
               for i in m'range(2) loop null; end loop;\n\
               for i in m'reverse_range(2) loop null; end loop;\n\
             end process;",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(indices(ranges[0]), vec![7, 6, 5, 4]);
        assert_eq!(indices(ranges[1]), vec![0, 1, 2]);
        assert_eq!(indices(ranges[2]), vec![3, 2, 1]);
        assert_eq!(indices(ranges[3]), vec![1, 2, 3]);
    }

    #[test]
    fn explicit_and_subtype_ranges_resolve() {
        let (ranges, diags) = resolve_loops(
            "subtype idx_t is integer range 0 to 2;",
            "process begin\n\
               for i in N - 1 downto 0 loop null; end loop;\n\
               for i in natural range 1 to 2 loop null; end loop;\n\
               for i in idx_t loop null; end loop;\n\
             end process;",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(indices(ranges[0]), vec![3, 2, 1, 0]);
        assert_eq!(indices(ranges[1]), vec![1, 2]);
        assert_eq!(indices(ranges[2]), vec![0, 1, 2]);
    }

    fn single_e212(decls: &str, range: &str) -> Diagnostic {
        let (ranges, diags) = resolve_loops(
            decls,
            &format!("process begin for i in {range} loop null; end loop; end process;"),
        );
        assert_eq!(ranges, vec![None], "range must not resolve");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, errors::E212);
        diags.into_iter().next().unwrap()
    }

    #[test]
    fn unknown_prefix_is_reported() {
        let d = single_e212("", "nope'range");
        assert!(d.message.contains("`nope'range`"), "{}", d.message);
        assert!(d.message.contains("no signal"), "{}", d.message);
    }

    #[test]
    fn scalar_prefix_is_reported() {
        let d = single_e212("signal s : std_logic;", "s'reverse_range");
        assert!(d.message.contains("not an array"), "{}", d.message);
    }

    #[test]
    fn unknown_index_range_is_reported() {
        let d = single_e212("signal u : std_logic_vector(M downto 0);", "u'range");
        assert!(d.message.contains("cannot be determined"), "{}", d.message);
    }

    #[test]
    fn bad_dimension_is_reported() {
        let d = single_e212("signal v : std_logic_vector(3 downto 0);", "v'range(2)");
        assert!(d.message.contains("dimension 2"), "{}", d.message);
        let d = single_e212("signal v : std_logic_vector(3 downto 0);", "v'range(0)");
        assert!(d.message.contains("dimension 0"), "{}", d.message);
    }

    #[test]
    fn compound_prefix_is_reported() {
        let d = single_e212("signal v : std_logic_vector(3 downto 0);", "v(1)'range");
        assert!(d.message.contains("simple name"), "{}", d.message);
    }

    #[test]
    fn inner_declarations_shadow_outer_ones() {
        let (ast, source_db, interner) = parse(
            "architecture rtl of t is\n\
               signal v : std_logic_vector(3 downto 0);\n\
               signal v : std_logic;\n\
             begin end architecture;",
        );
        let mut env = RangeEnv::new();
        for decl in &architecture(&ast).decls {
            env.declare(decl, &ConstEnv::new(), &source_db, &interner);
        }
        let v = interner.get_or_intern("v");
        assert_eq!(env.get(v), Some(&DeclaredRange::NotArray(None)));
    }
}
