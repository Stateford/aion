//! Name binding across instantiation boundaries: named port and
//! parameter/generic associations.
//!
//! The VHDL parser folds basic identifiers to lowercase (they are
//! case-insensitive, IEEE 1076-2008 §15.4.2), while Verilog and SystemVerilog
//! identifiers are case-sensitive and kept verbatim. When either side of an
//! instantiation is VHDL, a named formal (`.Clk(x)`, `Clk => x`) is therefore
//! matched exactly first and then case-insensitively against the child's
//! *basic* names. Extended (`\Clk\`) and escaped (`\Clk `) identifiers are
//! case-sensitive and only ever match exactly. Verilog/SV-to-Verilog/SV
//! bindings stay fully case-sensitive.

use aion_common::{Ident, Interner};
use aion_ir::ConstValue;
use aion_ir::ModuleId;
use aion_source::Span;

use crate::context::ElaborationContext;
use crate::errors;
use crate::registry::ModuleEntry;

/// Returns `true` if `text` is a VHDL extended identifier (`\Foo\`) or a
/// Verilog escaped identifier (`\Foo `), both of which are case-sensitive.
pub(crate) fn is_extended_identifier(text: &str) -> bool {
    text.starts_with('\\')
}

/// The result of matching a named formal against a child's declared names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormalMatch {
    /// The formal binds to this (canonical) declared name.
    Found(Ident),
    /// No declared name matches.
    Missing,
    /// Several declared names differ from the formal only in case.
    Ambiguous,
}

/// Matches `formal` against `candidates`.
///
/// An exact match always wins. When `case_insensitive` is set and `formal` is
/// a basic identifier, a unique basic candidate equal ignoring ASCII case is
/// accepted; two or more such candidates make the match ambiguous.
pub(crate) fn match_formal(
    formal: Ident,
    candidates: &[Ident],
    case_insensitive: bool,
    interner: &Interner,
) -> FormalMatch {
    if candidates.contains(&formal) {
        return FormalMatch::Found(formal);
    }
    let text = interner.resolve(formal);
    if !case_insensitive || is_extended_identifier(text) {
        return FormalMatch::Missing;
    }
    let mut found = None;
    for &cand in candidates {
        let cand_text = interner.resolve(cand);
        if is_extended_identifier(cand_text) || !cand_text.eq_ignore_ascii_case(text) {
            continue;
        }
        match found {
            None => found = Some(cand),
            Some(prev) if prev != cand => return FormalMatch::Ambiguous,
            Some(_) => {}
        }
    }
    found.map_or(FormalMatch::Missing, FormalMatch::Found)
}

/// Resolves a named port association `formal` on the already-elaborated
/// child module `target` to the child's canonical port name.
///
/// With `case_insensitive` unset (Verilog/SV instantiating Verilog/SV) the
/// formal is returned unchanged, preserving the existing behaviour for
/// unknown ports. With it set (either side is VHDL), the formal is matched
/// via [`match_formal`]; an unknown or ambiguous formal emits `E208` /
/// `E201` at `span` and returns `None` so no bogus connection is created.
pub(crate) fn bind_port(
    target: ModuleId,
    formal: Ident,
    case_insensitive: bool,
    span: Span,
    ctx: &ElaborationContext<'_>,
) -> Option<Ident> {
    if !case_insensitive {
        return Some(formal);
    }
    let module = ctx.design.modules.get(target);
    let ports: Vec<Ident> = module.ports.iter().map(|p| p.name).collect();
    match match_formal(formal, &ports, true, ctx.interner) {
        FormalMatch::Found(name) => Some(name),
        FormalMatch::Missing => {
            ctx.sink.emit(errors::error_unknown_port(
                ctx.interner.resolve(formal),
                ctx.interner.resolve(module.name),
                span,
            ));
            None
        }
        FormalMatch::Ambiguous => {
            ctx.sink.emit(errors::error_port_mismatch(
                &format!(
                    "port `{}` is ambiguous on module `{}`: several ports differ only in case",
                    ctx.interner.resolve(formal),
                    ctx.interner.resolve(module.name),
                ),
                span,
            ));
            None
        }
    }
}

/// Returns the parameter/generic names declared by a module entry.
fn param_names(entry: &ModuleEntry<'_>) -> Vec<Ident> {
    match entry {
        ModuleEntry::Verilog(decl) => decl.params.iter().map(|p| p.name).collect(),
        ModuleEntry::Sv(decl) => decl.params.iter().map(|p| p.name).collect(),
        ModuleEntry::Vhdl { entity, .. } => entity
            .generics
            .iter()
            .flat_map(|g| g.decls.iter())
            .flat_map(|d| d.names.iter().copied())
            .collect(),
    }
}

/// Rewrites parameter/generic override names to the child's canonical
/// spelling when either side of the instantiation is VHDL.
///
/// `from_vhdl` is `true` for a VHDL instantiation. Only unique matches are
/// rewritten; unmatched or ambiguous names are left as written, matching the
/// existing treatment of unknown overrides.
pub(crate) fn canonicalize_overrides(
    overrides: &mut [(Ident, ConstValue)],
    entry: Option<&ModuleEntry<'_>>,
    from_vhdl: bool,
    interner: &Interner,
) {
    let Some(entry) = entry else {
        return;
    };
    let case_insensitive = from_vhdl || matches!(entry, ModuleEntry::Vhdl { .. });
    if !case_insensitive {
        return;
    }
    let names = param_names(entry);
    for (name, _) in overrides.iter_mut() {
        if let FormalMatch::Found(canon) = match_formal(*name, &names, true, interner) {
            *name = canon;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_wins_even_case_sensitively() {
        let i = Interner::new();
        let clk = i.get_or_intern("Clk");
        assert_eq!(
            match_formal(clk, &[clk], false, &i),
            FormalMatch::Found(clk)
        );
    }

    #[test]
    fn case_sensitive_mode_never_folds() {
        let i = Interner::new();
        let formal = i.get_or_intern("Clk");
        let port = i.get_or_intern("clk");
        assert_eq!(
            match_formal(formal, &[port], false, &i),
            FormalMatch::Missing
        );
    }

    #[test]
    fn unique_case_variant_matches() {
        let i = Interner::new();
        let formal = i.get_or_intern("Clk");
        let port = i.get_or_intern("clk");
        let other = i.get_or_intern("rst");
        assert_eq!(
            match_formal(formal, &[other, port], true, &i),
            FormalMatch::Found(port)
        );
    }

    #[test]
    fn multiple_case_variants_are_ambiguous() {
        let i = Interner::new();
        let formal = i.get_or_intern("clk");
        let a = i.get_or_intern("Clk");
        let b = i.get_or_intern("CLK");
        assert_eq!(
            match_formal(formal, &[a, b], true, &i),
            FormalMatch::Ambiguous
        );
    }

    #[test]
    fn extended_identifiers_only_match_exactly() {
        let i = Interner::new();
        let ext = i.get_or_intern("\\Clk\\");
        let ext_upper = i.get_or_intern("\\CLK\\");
        let basic = i.get_or_intern("clk");
        // Extended formal never folds.
        assert_eq!(
            match_formal(ext_upper, &[ext], true, &i),
            FormalMatch::Missing
        );
        // Basic formal never binds to an extended candidate.
        assert_eq!(match_formal(basic, &[ext], true, &i), FormalMatch::Missing);
        assert!(is_extended_identifier("\\Clk\\"));
        assert!(!is_extended_identifier("clk"));
    }
}
