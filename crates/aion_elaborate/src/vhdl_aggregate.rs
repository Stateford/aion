//! Lowering of VHDL array aggregates with named and `others` choices.
//!
//! A positional aggregate `('1', '0', b)` is its elements left to right, like
//! a concatenation, and needs no context. An aggregate with choices —
//! `(3 => '1', others => '0')`, `(7 downto 4 => '1', others => '0')`,
//! `(1 | 3 => '1', others => '0')`, `(others => x)` — places each value at an
//! *index*, and which bit an index is depends on the index range the
//! aggregate takes from its context (IEEE 1076-2008 9.3.3.3): the target of
//! the assignment, or the declared subtype of the object whose default value
//! it is. That context is an [`AggregateContext`], recorded for each VHDL
//! object in the [`SignalEnv`].
//!
//! Values are laid out the way every other VHDL array value is lowered: the
//! leftmost element of the index range is the most significant bit. So for
//! `y : std_logic_vector(7 downto 0)`, index `i` is bit `i`, and for
//! `a : std_logic_vector(0 to 7)`, index `0` is bit 7.
//!
//! Where the context gives no index range, or the aggregate uses a form that
//! is not supported, an error is reported instead of building hardware with
//! a guessed bit layout (correctness first). The one exception mirrors the
//! SystemVerilog `'0`: an all-zero fill `(others => '0')` lowers to a 1-bit
//! zero when no width is known, which zero-extension makes exact.

use aion_common::{Interner, Logic, LogicVec};
use aion_diagnostics::DiagnosticSink;
use aion_ir::expr::Expr as IrExpr;
use aion_source::{SourceDb, Span};
use aion_vhdl_parser::ast::{
    self as vhdl_ast, AggregateElement, Choice, NameSuffix, RangeDirection,
};

use crate::const_eval::{self, ConstEnv};
use crate::errors;
use crate::expr::{lower_vhdl_expr, SignalEnv};
use crate::ranges::{self, ResolvedRange};
use crate::vhdl::std_logic_char;

/// The largest index range, in elements, that an aggregate is laid out over
/// (matches the simulator's widest evaluated vector, 16 Mi bits).
const MAX_AGGREGATE_ELEMENTS: u64 = 1 << 24;

/// What the context of a VHDL aggregate (its assignment target, or the
/// object whose default it is) tells the elaborator about its shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateContext {
    /// The total bit width of the target.
    pub width: u32,
    /// The target's index range when it is a one-dimensional array with one
    /// bit per element (`std_logic_vector`, `unsigned`, `bit_vector`, ...),
    /// whose length equals `width`. `None` for other targets, such as
    /// arrays of vectors, which only support uniform fills like
    /// `(others => (others => '0'))`.
    pub bit_range: Option<ResolvedRange>,
}

impl AggregateContext {
    /// Builds the context of an object of `width` bits whose declared index
    /// ranges are `dims` (one range per dimension, as recorded in a
    /// [`ranges::RangeEnv`]). The index range is kept only for a
    /// one-dimensional array with exactly one bit per element.
    pub fn for_object(width: u32, dims: Option<&[ResolvedRange]>) -> Self {
        let bit_range = match dims {
            Some([only]) if only.len() == u64::from(width) => Some(*only),
            _ => None,
        };
        Self { width, bit_range }
    }
}

/// Why an aggregate could not be laid out over an index range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateLayoutError {
    /// A form the elaborator does not support (a non-constant choice, a
    /// named aggregate without `others` whose bit order would come from the
    /// index subtype rather than the target). Reported as E210.
    Unsupported(String),
    /// The aggregate does not fit the index range: an index outside it, an
    /// index given twice, a position with no element, too many positional
    /// elements, or an ill-formed association list. Reported as E205.
    Mismatch(String),
}

/// Returns the [`AggregateContext`] that the VHDL assignment target `target`
/// gives an aggregate assigned to it, or `None` if it is not known.
///
/// A whole object gives its recorded context. A slice `y(7 downto 4)` with
/// constant bounds, of an object with a recorded index range in the same
/// direction, gives the slice's own range. Anything else (an element
/// `m(i)`, an aggregate target, a record field) gives none.
pub fn vhdl_target_context(
    target: &vhdl_ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
) -> Option<AggregateContext> {
    let vhdl_ast::Expr::Name(name) = target else {
        return None;
    };
    let base = sig_env.aggregate_context(*sig_env.get(&name.primary)?)?;
    match name.parts.as_slice() {
        [] => Some(base),
        [NameSuffix::Slice(rc, _)] => {
            let outer = base.bit_range?;
            // Non-constant bounds just leave the layout unknown; this
            // lookup reports nothing itself.
            let slice = ranges::eval_range_constraint(
                rc,
                sig_env.consts(),
                source_db,
                interner,
                &DiagnosticSink::new(),
            )?;
            let inside = |i| range_position(outer, i).is_some();
            let ok = slice.direction == outer.direction
                && (slice.is_empty() || (inside(slice.left) && inside(slice.right)));
            let len = u32::try_from(slice.len()).ok()?;
            ok.then_some(AggregateContext {
                width: len,
                bit_range: Some(slice),
            })
        }
        _ => None,
    }
}

/// Returns the position (0 = leftmost) of index `i` in `range`, or `None`
/// if `i` is outside it.
fn range_position(range: ResolvedRange, i: i64) -> Option<usize> {
    let inside = match range.direction {
        RangeDirection::To => range.left <= i && i <= range.right,
        RangeDirection::Downto => range.right <= i && i <= range.left,
    };
    inside.then(|| i.abs_diff(range.left) as usize)
}

/// Formats `range` as VHDL source (`7 downto 0`).
fn show_range(range: ResolvedRange) -> String {
    let dir = match range.direction {
        RangeDirection::To => "to",
        RangeDirection::Downto => "downto",
    };
    format!("{} {dir} {}", range.left, range.right)
}

/// Evaluates an aggregate choice to the inclusive span of indices it
/// selects (`None` for a null range). `others` is handled by the caller.
fn choice_span(
    choice: &Choice,
    consts: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
) -> Result<Option<(i64, i64)>, AggregateLayoutError> {
    let scratch = DiagnosticSink::new();
    match choice {
        Choice::Expr(e) => {
            let i = const_eval::eval_vhdl_expr(e, source_db, interner, consts, &scratch)
                .and_then(|v| const_eval::const_to_i64(&v))
                .ok_or_else(|| {
                    AggregateLayoutError::Unsupported(format!(
                        "aggregate choice `{}` that is not a constant integer index",
                        source_db.snippet(e.span())
                    ))
                })?;
            Ok(Some((i, i)))
        }
        Choice::Range(rc) => {
            let r = ranges::eval_range_constraint(rc, consts, source_db, interner, &scratch)
                .ok_or_else(|| {
                    AggregateLayoutError::Unsupported(format!(
                        "aggregate range choice `{}` whose bounds are not constant",
                        source_db.snippet(rc.span)
                    ))
                })?;
            Ok((!r.is_empty()).then(|| (r.left.min(r.right), r.left.max(r.right))))
        }
        Choice::Others(_) => Ok(None),
    }
}

/// Lays out the elements of a VHDL array aggregate over the index range
/// `range`.
///
/// Returns the value expression of every element position of `range`, from
/// left to right. Positional elements fill the leftmost positions in order;
/// each named choice (an index, a range `7 downto 4`, or `|`-separated
/// alternatives of these, with constant bounds evaluated over the
/// constants `consts`) selects the positions of its indices; a final
/// `others` fills every position still empty.
///
/// A named aggregate without `others` is laid out only over an ascending
/// (or one-element) range: its own index range then runs from its smallest
/// to its largest choice in the ascending direction of the index subtype
/// (IEEE 1076-2008 9.3.3.3), which matches the target only in that case.
pub fn vhdl_aggregate_layout<'a>(
    elements: &'a [AggregateElement],
    range: ResolvedRange,
    consts: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
) -> Result<Vec<&'a vhdl_ast::Expr>, AggregateLayoutError> {
    use AggregateLayoutError::{Mismatch, Unsupported};
    let n = range.len();
    if n == 0 || n > MAX_AGGREGATE_ELEMENTS {
        return Err(Unsupported(format!(
            "aggregate over the index range {} ({n} elements)",
            show_range(range)
        )));
    }
    let n = n as usize;
    let is_others = |e: &AggregateElement| e.choices.iter().any(|c| matches!(c, Choice::Others(_)));

    let others = match elements.iter().position(is_others) {
        Some(pos) if pos + 1 != elements.len() || elements[pos].choices.len() != 1 => {
            return Err(Mismatch(
                "`others` must be the only choice of the last element of an aggregate".into(),
            ));
        }
        Some(pos) => Some(&elements[pos].value),
        None => None,
    };
    let rest = &elements[..elements.len() - usize::from(others.is_some())];
    let positional = rest.iter().take_while(|e| e.choices.is_empty()).count();
    if rest[positional..].iter().any(|e| e.choices.is_empty()) {
        return Err(Mismatch(
            "positional association after named association in an aggregate".into(),
        ));
    }
    if positional > 0 && positional < rest.len() {
        return Err(Mismatch(
            "an aggregate cannot mix positional and named associations \
             (except for a final `others`)"
                .into(),
        ));
    }
    if positional > n {
        return Err(Mismatch(format!(
            "aggregate has {positional} positional elements but the target has {n} \
             (index range {})",
            show_range(range)
        )));
    }
    let named = rest.len() - positional;
    if named > 0 && others.is_none() && n > 1 && range.direction == RangeDirection::Downto {
        return Err(Unsupported(format!(
            "named aggregate without `others` assigned to the descending index range {}: \
             its elements would be ordered by the ascending index subtype, not by the \
             target's indices (add `others => ...` or use positional association)",
            show_range(range)
        )));
    }

    let mut slots: Vec<Option<&vhdl_ast::Expr>> = vec![None; n];
    for (slot, e) in slots.iter_mut().zip(&rest[..positional]) {
        *slot = Some(&e.value);
    }
    for element in &rest[positional..] {
        for choice in &element.choices {
            let Some((lo, hi)) = choice_span(choice, consts, source_db, interner)? else {
                continue;
            };
            for i in [lo, hi] {
                if range_position(range, i).is_none() {
                    return Err(Mismatch(format!(
                        "aggregate index {i} is outside the target's index range {}",
                        show_range(range)
                    )));
                }
            }
            for i in lo..=hi {
                let slot = &mut slots[range_position(range, i).unwrap_or_default()];
                if slot.is_some() {
                    return Err(Mismatch(format!(
                        "aggregate index {i} is associated more than once"
                    )));
                }
                *slot = Some(&element.value);
            }
        }
    }
    slots
        .into_iter()
        .zip(range.indices())
        .map(|(slot, i)| {
            slot.or(others).ok_or_else(|| {
                Mismatch(format!(
                    "aggregate has no element for index {i} of the target's index range {} \
                     (add an `others` choice)",
                    show_range(range)
                ))
            })
        })
        .collect()
}

/// Returns the constant single bit an aggregate element value gives: a
/// `std_logic` character literal, possibly parenthesized.
fn const_bit(expr: &vhdl_ast::Expr, source_db: &SourceDb) -> Option<Logic> {
    match expr {
        vhdl_ast::Expr::CharLiteral { span } => {
            let text = source_db.snippet(*span).trim_matches('\'');
            let mut chars = text.chars();
            let bit = std_logic_char(chars.next()?)?;
            chars.next().is_none().then_some(bit)
        }
        vhdl_ast::Expr::Paren { inner, .. } => const_bit(inner, source_db),
        _ => None,
    }
}

/// Returns the bit that every element of a uniform fill sets: an aggregate
/// made only of `others => v`, where `v` is a constant bit or itself a
/// uniform fill (`(others => (others => '0'))` for an array of vectors).
pub fn uniform_fill_bit(elements: &[AggregateElement], source_db: &SourceDb) -> Option<Logic> {
    let [only] = elements else { return None };
    if !matches!(only.choices.as_slice(), [Choice::Others(_)]) {
        return None;
    }
    let mut value = &only.value;
    while let vhdl_ast::Expr::Paren { inner, .. } = value {
        value = inner;
    }
    match value {
        vhdl_ast::Expr::Aggregate { elements, .. } => uniform_fill_bit(elements, source_db),
        other => const_bit(other, source_db),
    }
}

/// Builds a `width`-bit vector with every bit set to `bit`.
fn filled(bit: Logic, width: u32) -> LogicVec {
    let mut lv = LogicVec::new(width);
    for i in 0..width {
        lv.set(i, bit);
    }
    lv
}

/// Lowers a VHDL aggregate that has named or `others` choices, in the
/// context `ctx` (see the [module documentation](self)).
///
/// - A uniform fill (`(others => '1')`, `(others => (others => '0'))`)
///   becomes a literal of the context width. Without a context, only the
///   all-zero fill is lowered (to a 1-bit zero, exact under zero-extension).
/// - Otherwise the context must give an index range; the elements are laid
///   out over it with [`vhdl_aggregate_layout`]. Constant bits are merged
///   into literals, and a non-constant value covering consecutive positions
///   (`(others => en)`) becomes a replication.
///
/// Everything else is an error: E210 for an unknown index range or an
/// unsupported form, E205 for an aggregate that does not fit the range.
pub fn lower_vhdl_aggregate(
    elements: &[AggregateElement],
    span: Span,
    ctx: Option<AggregateContext>,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    let poison = || IrExpr::Literal(LogicVec::all_zero(1));
    if let Some(bit) = uniform_fill_bit(elements, source_db) {
        match ctx {
            Some(ctx) if ctx.width > 0 => return IrExpr::Literal(filled(bit, ctx.width)),
            _ if bit == Logic::Zero => return IrExpr::Literal(LogicVec::all_zero(1)),
            _ => {}
        }
    }
    let Some(range) = ctx.and_then(|c| c.bit_range) else {
        sink.emit(errors::error_unsupported(
            "aggregate with named or `others` choices whose index range cannot be \
             determined; it is supported as the value (or a conditional alternative) of an \
             assignment to, or the default value of, a whole one-dimensional array of bits \
             with a constant index range, or a constant slice of one",
            span,
        ));
        return poison();
    };
    let slots = match vhdl_aggregate_layout(elements, range, sig_env.consts(), source_db, interner)
    {
        Ok(slots) => slots,
        Err(AggregateLayoutError::Unsupported(msg)) => {
            sink.emit(errors::error_unsupported(&msg, span));
            return poison();
        }
        Err(AggregateLayoutError::Mismatch(msg)) => {
            sink.emit(errors::error_type_mismatch(&msg, span));
            return poison();
        }
    };

    // Left to right is MSB first, as in a concatenation.
    let mut parts: Vec<IrExpr> = Vec::new();
    let mut bits: Vec<Logic> = Vec::new();
    let flush = |bits: &mut Vec<Logic>, parts: &mut Vec<IrExpr>| {
        if bits.is_empty() {
            return;
        }
        let mut lv = LogicVec::new(bits.len() as u32);
        for (i, bit) in bits.iter().rev().enumerate() {
            lv.set(i as u32, *bit);
        }
        parts.push(IrExpr::Literal(lv));
        bits.clear();
    };
    let mut k = 0;
    while k < slots.len() {
        let value = slots[k];
        let run = slots[k..]
            .iter()
            .take_while(|v| std::ptr::eq(**v, value))
            .count();
        match const_bit(value, source_db) {
            Some(bit) => bits.extend(std::iter::repeat_n(bit, run)),
            None => {
                flush(&mut bits, &mut parts);
                let lowered = lower_vhdl_expr(value, sig_env, source_db, interner, sink);
                parts.push(if run == 1 {
                    lowered
                } else {
                    IrExpr::Repeat {
                        expr: Box::new(lowered),
                        count: run as u32,
                        span: value.span(),
                    }
                });
            }
        }
        k += run;
    }
    flush(&mut bits, &mut parts);
    if parts.len() == 1 {
        parts.pop().unwrap_or_else(poison)
    } else {
        IrExpr::Concat(parts)
    }
}

/// Lowers `value`, the value of a VHDL signal or variable assignment whose
/// target is `target`.
///
/// Unlike [`lower_vhdl_expr`], an aggregate with named or `others` choices
/// (possibly parenthesized) is laid out over the index range of the target
/// (see [`vhdl_target_context`] and [`lower_vhdl_aggregate`]).
pub fn lower_vhdl_assign_value(
    value: &vhdl_ast::Expr,
    target: &vhdl_ast::Expr,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    let ctx = vhdl_target_context(target, sig_env, source_db, interner);
    lower_vhdl_sized(value, ctx, sig_env, source_db, interner, sink)
}

/// Lowers `expr` in the aggregate context `ctx` (see
/// [`lower_vhdl_assign_value`]).
pub fn lower_vhdl_sized(
    expr: &vhdl_ast::Expr,
    ctx: Option<AggregateContext>,
    sig_env: &SignalEnv,
    source_db: &SourceDb,
    interner: &Interner,
    sink: &DiagnosticSink,
) -> IrExpr {
    match expr {
        vhdl_ast::Expr::Paren { inner, .. } => {
            lower_vhdl_sized(inner, ctx, sig_env, source_db, interner, sink)
        }
        vhdl_ast::Expr::Aggregate { elements, span }
            if elements.iter().any(|e| !e.choices.is_empty()) =>
        {
            lower_vhdl_aggregate(elements, *span, ctx, sig_env, source_db, interner, sink)
        }
        _ => lower_vhdl_expr(expr, sig_env, source_db, interner, sink),
    }
}

/// Evaluates a VHDL aggregate with named or `others` choices that is the
/// default value of an object with index range `range` to a constant, with
/// each element value evaluated to a single bit by `bit`.
///
/// Returns `None` if the aggregate cannot be laid out over `range` or an
/// element is not constant; the caller reports that (W204).
pub fn eval_vhdl_aggregate_default(
    elements: &[AggregateElement],
    range: ResolvedRange,
    consts: &ConstEnv,
    source_db: &SourceDb,
    interner: &Interner,
    mut bit: impl FnMut(&vhdl_ast::Expr) -> Option<Logic>,
) -> Option<LogicVec> {
    let slots = vhdl_aggregate_layout(elements, range, consts, source_db, interner).ok()?;
    let width = slots.len() as u32;
    let mut lv = LogicVec::new(width);
    for (k, value) in slots.into_iter().enumerate() {
        lv.set(width - 1 - k as u32, bit(value)?);
    }
    Some(lv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_ir::ids::SignalId;

    /// A parsed `<target> <= <value>;` with everything needed to lower it.
    struct Fixture {
        source_db: SourceDb,
        interner: Interner,
        sink: DiagnosticSink,
        target: vhdl_ast::Expr,
        value: vhdl_ast::Expr,
    }

    /// Parses the concurrent assignment `<target> <= <value>;`.
    fn parse(target: &str, value: &str) -> Fixture {
        let src = format!(
            "entity t is end entity;\narchitecture a of t is\nbegin\n  \
             {target} <= {value};\nend architecture;\n"
        );
        let mut source_db = SourceDb::new();
        let interner = Interner::new();
        let sink = DiagnosticSink::new();
        let file = source_db.add_source("t.vhd", src);
        let design = aion_vhdl_parser::parse_file(file, &source_db, &interner, &sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let sa = design
            .units
            .iter()
            .find_map(|u| match u {
                vhdl_ast::DesignUnit::ContextUnit {
                    unit: vhdl_ast::DesignUnitKind::Architecture(arch),
                    ..
                } => arch.stmts.iter().find_map(|s| match s {
                    vhdl_ast::ConcurrentStatement::SignalAssignment(sa) => Some(sa.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .expect("a concurrent signal assignment");
        Fixture {
            source_db,
            interner,
            sink,
            target: sa.target,
            value: sa.waveforms[0].value.clone(),
        }
    }

    impl Fixture {
        /// The elements of the parsed value, which must be an aggregate.
        fn elements(&self) -> &[AggregateElement] {
            match &self.value {
                vhdl_ast::Expr::Aggregate { elements, .. } => elements,
                other => panic!("not an aggregate: {other:?}"),
            }
        }

        /// A scope where `y` is signal 0 with context `ctx`.
        fn env(&self, ctx: Option<AggregateContext>) -> SignalEnv {
            let mut env = SignalEnv::new();
            let sid = SignalId::from_raw(0);
            env.insert(self.interner.get_or_intern("y"), sid);
            if let Some(ctx) = ctx {
                env.insert_aggregate_context(sid, ctx);
            }
            env
        }

        /// Lowers the value as assigned to the target, `y` having `ctx`.
        fn lower(&self, ctx: Option<AggregateContext>) -> IrExpr {
            let env = self.env(ctx);
            lower_vhdl_assign_value(
                &self.value,
                &self.target,
                &env,
                &self.source_db,
                &self.interner,
                &self.sink,
            )
        }

        /// Lays the aggregate out over `range`, giving each slot's source.
        fn layout(&self, range: ResolvedRange) -> Result<Vec<String>, AggregateLayoutError> {
            vhdl_aggregate_layout(
                self.elements(),
                range,
                &ConstEnv::new(),
                &self.source_db,
                &self.interner,
            )
            .map(|slots| {
                slots
                    .iter()
                    .map(|e| self.source_db.snippet(e.span()).to_string())
                    .collect()
            })
        }

        /// The codes of every diagnostic emitted so far.
        fn codes(&self) -> Vec<u16> {
            self.sink
                .diagnostics()
                .iter()
                .map(|d| d.code.number)
                .collect()
        }
    }

    fn downto(left: i64, right: i64) -> ResolvedRange {
        ResolvedRange::new(left, RangeDirection::Downto, right)
    }

    fn to(left: i64, right: i64) -> ResolvedRange {
        ResolvedRange::new(left, RangeDirection::To, right)
    }

    /// The context of a one-dimensional bit vector over `range`.
    fn vector(range: ResolvedRange) -> Option<AggregateContext> {
        Some(AggregateContext {
            width: range.len() as u32,
            bit_range: Some(range),
        })
    }

    /// Renders a literal MSB first; panics on anything else.
    fn literal(e: &IrExpr) -> String {
        match e {
            IrExpr::Literal(lv) => format!("{lv}"),
            other => panic!("not a literal: {other:?}"),
        }
    }

    #[test]
    fn context_keeps_the_range_only_for_one_bit_elements() {
        let r = downto(7, 0);
        assert_eq!(
            AggregateContext::for_object(8, Some(&[r])).bit_range,
            Some(r)
        );
        // An array of four bytes: 32 bits over four indices.
        assert_eq!(
            AggregateContext::for_object(32, Some(&[to(0, 3)])).bit_range,
            None
        );
        assert_eq!(AggregateContext::for_object(8, None).bit_range, None);
        assert_eq!(AggregateContext::for_object(8, None).width, 8);
    }

    #[test]
    fn layout_maps_indices_through_the_range_direction() {
        let f = parse("y", "(3 => '1', 1 | 0 => 'Z', others => '0')");
        assert_eq!(
            f.layout(downto(3, 0)).unwrap(),
            vec!["'1'", "'0'", "'Z'", "'Z'"]
        );
        assert_eq!(
            f.layout(to(0, 3)).unwrap(),
            vec!["'Z'", "'Z'", "'0'", "'1'"]
        );
        let f = parse("y", "(5 downto 4 => '1', others => '0')");
        assert_eq!(
            f.layout(downto(7, 2)).unwrap(),
            vec!["'0'", "'0'", "'1'", "'1'", "'0'", "'0'"]
        );
    }

    #[test]
    fn layout_rejects_aggregates_that_do_not_fit() {
        use AggregateLayoutError::{Mismatch, Unsupported};
        let mismatch =
            |value: &str, range| matches!(parse("y", value).layout(range), Err(Mismatch(_)));
        assert!(mismatch("(4 => '1', others => '0')", downto(3, 0)));
        assert!(mismatch(
            "(1 => '1', 1 => '0', others => '0')",
            downto(3, 0)
        ));
        assert!(mismatch("(0 => '1', 1 => '0')", to(0, 3)));
        assert!(mismatch("('1', '0', '1', others => '0')", downto(1, 0)));
        assert!(mismatch("('1', 1 => '0')", to(0, 1)));
        assert!(matches!(
            parse("y", "(1 => '1', 0 => '0')").layout(downto(1, 0)),
            Err(Unsupported(_))
        ));
        assert!(matches!(
            parse("y", "(k => '1', others => '0')").layout(downto(1, 0)),
            Err(Unsupported(_))
        ));
        // A complete named aggregate over an ascending range is fine.
        assert_eq!(
            parse("y", "(1 => '1', 0 => '0')").layout(to(0, 1)).unwrap(),
            vec!["'0'", "'1'"]
        );
    }

    #[test]
    fn uniform_fill_bit_sees_through_nesting() {
        let f = parse("y", "(others => (others => '1'))");
        assert_eq!(
            uniform_fill_bit(f.elements(), &f.source_db),
            Some(Logic::One)
        );
        let f = parse("y", "(others => a)");
        assert_eq!(uniform_fill_bit(f.elements(), &f.source_db), None);
        let f = parse("y", "(0 => '1', others => '0')");
        assert_eq!(uniform_fill_bit(f.elements(), &f.source_db), None);
    }

    #[test]
    fn lowers_against_the_target_context() {
        let f = parse("y", "(3 => '1', others => '0')");
        assert_eq!(literal(&f.lower(vector(downto(7, 0)))), "00001000");
        // Ascending: index 3 is the fourth position from the left (MSB).
        assert_eq!(literal(&f.lower(vector(to(0, 7)))), "00010000");
        let f = parse("y", "(others => '1')");
        assert_eq!(literal(&f.lower(vector(downto(3, 0)))), "1111");
        // A uniform fill only needs the width.
        let ctx = AggregateContext {
            width: 16,
            bit_range: None,
        };
        assert_eq!(literal(&f.lower(Some(ctx))), "1111111111111111");
        assert!(f.codes().is_empty());
    }

    #[test]
    fn non_constant_values_are_replicated_or_concatenated() {
        let f = parse("y", "(0 => '1', others => a)");
        let mut env = f.env(vector(downto(3, 0)));
        env.insert(f.interner.get_or_intern("a"), SignalId::from_raw(1));
        let e = lower_vhdl_assign_value(
            &f.value,
            &f.target,
            &env,
            &f.source_db,
            &f.interner,
            &f.sink,
        );
        let IrExpr::Concat(parts) = e else {
            panic!("expected a concat, got {e:?}");
        };
        assert!(matches!(&parts[0], IrExpr::Repeat { count: 3, .. }));
        assert_eq!(literal(&parts[1]), "1");
        assert!(f.codes().is_empty());
    }

    #[test]
    fn unknown_context_is_an_error_except_for_a_zero_fill() {
        let f = parse("y", "(others => '0')");
        assert_eq!(literal(&f.lower(None)), "0");
        assert!(f.codes().is_empty());
        let f = parse("y", "(others => '1')");
        f.lower(None);
        assert_eq!(f.codes(), vec![210]);
        let f = parse("y", "(3 => '1', others => '0')");
        f.lower(None);
        assert_eq!(f.codes(), vec![210]);
        let f = parse("y", "(9 => '1', others => '0')");
        f.lower(vector(downto(3, 0)));
        assert_eq!(f.codes(), vec![205]);
    }

    #[test]
    fn slice_targets_give_their_own_range() {
        let f = parse("y(5 downto 2)", "(4 => '1', others => '0')");
        assert_eq!(literal(&f.lower(vector(downto(7, 0)))), "0100");
        let env = f.env(vector(downto(7, 0)));
        assert_eq!(
            vhdl_target_context(&f.target, &env, &f.source_db, &f.interner),
            vector(downto(5, 2))
        );
        // A slice in the wrong direction gives no context.
        let f = parse("y(2 to 5)", "(others => '1')");
        let env = f.env(vector(downto(7, 0)));
        assert_eq!(
            vhdl_target_context(&f.target, &env, &f.source_db, &f.interner),
            None
        );
        // An element target gives none either.
        let f = parse("y(3)", "'1'");
        let env = f.env(vector(downto(7, 0)));
        assert_eq!(
            vhdl_target_context(&f.target, &env, &f.source_db, &f.interner),
            None
        );
    }

    #[test]
    fn sized_lowering_sees_through_parentheses_only() {
        let f = parse("y", "((others => '1'))");
        let e = lower_vhdl_sized(
            &f.value,
            vector(downto(1, 0)),
            &f.env(None),
            &f.source_db,
            &f.interner,
            &f.sink,
        );
        assert_eq!(literal(&e), "11");
        // A positional aggregate is a plain concatenation.
        let f = parse("y", "('1', '0')");
        let e = lower_vhdl_sized(
            &f.value,
            None,
            &f.env(None),
            &f.source_db,
            &f.interner,
            &f.sink,
        );
        assert!(matches!(e, IrExpr::Concat(ref p) if p.len() == 2), "{e:?}");
    }

    #[test]
    fn default_values_evaluate_bit_by_bit() {
        let consts = ConstEnv::new();
        let f = parse("y", "(1 => '1', others => '0')");
        let lv = eval_vhdl_aggregate_default(
            f.elements(),
            downto(3, 0),
            &consts,
            &f.source_db,
            &f.interner,
            |e| const_bit(e, &f.source_db),
        );
        assert_eq!(lv.map(|v| format!("{v}")), Some("0010".to_string()));
        // A value that is not a constant bit makes the default unknown.
        let f = parse("y", "(1 => a, others => '0')");
        assert!(eval_vhdl_aggregate_default(
            f.elements(),
            downto(3, 0),
            &consts,
            &f.source_db,
            &f.interner,
            |e| const_bit(e, &f.source_db),
        )
        .is_none());
    }

    #[test]
    fn signal_env_records_aggregate_contexts() {
        let mut env = SignalEnv::new();
        let sid = SignalId::from_raw(4);
        assert_eq!(env.aggregate_context(sid), None);
        let ctx = vector(to(0, 3)).unwrap();
        env.insert_aggregate_context(sid, ctx);
        assert_eq!(env.aggregate_context(sid), Some(ctx));
    }
}
