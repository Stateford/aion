//! C201: Naming violation — module/signal/parameter/constant names vs naming conventions.

use aion_config::{NamingConfig, NamingConvention};
use aion_diagnostics::{Category, Diagnostic, DiagnosticCode, DiagnosticSink, Label, Severity};
use aion_ir::{Design, Module, SignalKind};
use aion_source::Span;

use crate::helpers::is_duplicate_specialization;
use crate::{LintContext, LintRule};

/// A naming convention C201 can enforce, mirroring
/// [`aion_config::NamingConvention`] (which is not `Copy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Snake,
    Camel,
    UpperSnake,
    Pascal,
}

impl Case {
    fn from_config(convention: &NamingConvention) -> Self {
        match convention {
            NamingConvention::SnakeCase => Case::Snake,
            NamingConvention::CamelCase => Case::Camel,
            NamingConvention::UpperSnakeCase => Case::UpperSnake,
            NamingConvention::PascalCase => Case::Pascal,
        }
    }

    /// The spelling used for this convention in `aion.toml`.
    fn label(self) -> &'static str {
        match self {
            Case::Snake => "snake_case",
            Case::Camel => "camelCase",
            Case::UpperSnake => "UPPER_SNAKE_CASE",
            Case::Pascal => "PascalCase",
        }
    }

    fn matches(self, name: &str) -> bool {
        match self {
            Case::Snake => is_snake_case(name),
            Case::Camel => is_camel_case(name),
            Case::UpperSnake => is_upper_snake_case(name),
            Case::Pascal => is_pascal_case(name),
        }
    }

    /// Rewrites `name` to follow this convention (e.g. `dataIn` to `data_in`).
    fn convert(self, name: &str) -> String {
        let words = split_words(name);
        match self {
            Case::Snake => words
                .iter()
                .map(|w| w.to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join("_"),
            Case::UpperSnake => words
                .iter()
                .map(|w| w.to_ascii_uppercase())
                .collect::<Vec<_>>()
                .join("_"),
            Case::Camel => words
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    if i == 0 {
                        w.to_ascii_lowercase()
                    } else {
                        capitalize(w)
                    }
                })
                .collect(),
            Case::Pascal => words.iter().map(|w| capitalize(w)).collect(),
        }
    }
}

/// Uppercases the first letter of `word` and lowercases the rest.
fn capitalize(word: &str) -> String {
    word.chars()
        .enumerate()
        .map(|(i, c)| {
            if i == 0 {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Splits an identifier into words at underscores and case changes:
/// `dataIn` → `data`, `In`; `HTTPServer_id` → `HTTP`, `Server`, `id`.
fn split_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if c.is_ascii_uppercase() && !current.is_empty() && i > 0 {
            let prev = chars[i - 1];
            let next_is_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_is_lower)
            {
                words.push(std::mem::take(&mut current));
            }
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Detects names that violate the naming conventions configured in the
/// `[lint.naming]` section of `aion.toml`.
///
/// Each category is checked only when a convention is configured for it:
///
/// - `module`: module and entity names;
/// - `signal`: wires, registers, latches, ports and VHDL variables;
/// - `parameter`: parameters and generics;
/// - `constant`: constants (`SignalKind::Const`).
///
/// With no `[lint.naming]` section the rule reports nothing. Leading
/// underscores are ignored (`_unused` is checked as `unused`), names that are
/// not plain identifiers (escaped/extended identifiers) are skipped, and the
/// spelling is taken from the source text, so VHDL identifiers (which the
/// parser folds to lowercase) are checked as written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NamingViolation {
    /// Convention for module names.
    module: Option<Case>,
    /// Convention for signal and port names.
    signal: Option<Case>,
    /// Convention for parameter names.
    parameter: Option<Case>,
    /// Convention for constant names.
    constant: Option<Case>,
}

impl NamingViolation {
    /// Creates the rule from the `[lint.naming]` configuration.
    ///
    /// `None`, or a section with no conventions set, yields a rule that checks
    /// nothing (the same as [`NamingViolation::default`]).
    pub fn new(config: Option<&NamingConfig>) -> Self {
        let Some(config) = config else {
            return Self::default();
        };
        Self {
            module: config.module.as_ref().map(Case::from_config),
            signal: config.signal.as_ref().map(Case::from_config),
            parameter: config.parameter.as_ref().map(Case::from_config),
            constant: config.constant.as_ref().map(Case::from_config),
        }
    }

    /// Checks `name` (a `kind` such as "signal") against `case` and reports
    /// a violation at `span`.
    fn check_name(
        &self,
        kind: &str,
        name: &str,
        case: Option<Case>,
        span: Span,
        sink: &DiagnosticSink,
    ) {
        let Some(case) = case else {
            return;
        };
        let core = name.trim_start_matches('_');
        if core.is_empty() || !core.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return;
        }
        if case.matches(core) {
            return;
        }
        let prefix = &name[..name.len() - core.len()];
        let suggestion = format!("{prefix}{}", case.convert(core));
        let mut diag = Diagnostic::warning(
            self.code(),
            format!(
                "{kind} name `{name}` does not follow the {} convention",
                case.label()
            ),
            span,
        )
        .with_label(Label::primary(
            span,
            format!("expected a {} name", case.label()),
        ))
        .with_note(format!(
            "the {kind} naming convention is set by `[lint.naming]` in aion.toml"
        ));
        if suggestion != name && case.matches(suggestion.trim_start_matches('_')) {
            diag = diag.with_help(format!("rename it to `{suggestion}`"));
        }
        sink.emit(diag);
    }
}

impl LintRule for NamingViolation {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode::new(Category::Convention, 201)
    }

    fn name(&self) -> &str {
        "naming-violation"
    }

    fn description(&self) -> &str {
        "name violates naming convention"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check_module(
        &self,
        module: &Module,
        design: &Design,
        ctx: &LintContext<'_>,
        sink: &DiagnosticSink,
    ) {
        if *self == Self::default() || is_duplicate_specialization(module, design) {
            return;
        }

        let module_name = ctx.source_spelling(module.name, module.span);
        self.check_name("module", &module_name, self.module, module.span, sink);

        for param in &module.params {
            let name = ctx.source_spelling(param.name, param.span);
            self.check_name("parameter", &name, self.parameter, param.span, sink);
        }

        for (_sig_id, signal) in module.signals.iter() {
            let name = ctx.source_spelling(signal.name, signal.span);
            if signal.kind == SignalKind::Const {
                self.check_name("constant", &name, self.constant, signal.span, sink);
            } else {
                self.check_name("signal", &name, self.signal, signal.span, sink);
            }
        }
    }
}

/// Checks if a name string follows snake_case convention.
pub fn is_snake_case(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    // Must start with lowercase or underscore
    let first = name.chars().next().unwrap();
    if !first.is_ascii_lowercase() && first != '_' {
        return false;
    }
    // Only lowercase, digits, and underscores allowed
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Checks if a name string follows UPPER_SNAKE_CASE convention.
pub fn is_upper_snake_case(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    name.chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Checks if a name string follows camelCase convention.
pub fn is_camel_case(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_lowercase() {
        return false;
    }
    // No underscores allowed
    !name.contains('_')
}

/// Checks if a name string follows PascalCase convention.
pub fn is_pascal_case(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_uppercase() {
        return false;
    }
    !name.contains('_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::test_util::empty_ctx;
    use aion_common::{ContentHash, Ident, Interner};
    use aion_ir::*;
    use aion_source::{SourceDb, Span};

    fn mk_module() -> Module {
        Module {
            id: ModuleId::from_raw(0),
            name: Ident::from_raw(0),
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: ContentHash::from_bytes(&[]),
        }
    }

    fn mk_design(module: Module) -> Design {
        let mut modules = Arena::new();
        let top = modules.alloc(module);
        Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        }
    }

    #[test]
    fn snake_case_valid() {
        assert!(is_snake_case("my_signal"));
        assert!(is_snake_case("a"));
        assert!(is_snake_case("data_in_0"));
        assert!(is_snake_case("_unused"));
    }

    #[test]
    fn snake_case_invalid() {
        assert!(!is_snake_case("MySignal"));
        assert!(!is_snake_case("dataIn"));
        assert!(!is_snake_case("DATA_IN"));
    }

    #[test]
    fn upper_snake_case_valid() {
        assert!(is_upper_snake_case("MY_PARAM"));
        assert!(is_upper_snake_case("WIDTH"));
        assert!(is_upper_snake_case("DATA_WIDTH_32"));
    }

    #[test]
    fn upper_snake_case_invalid() {
        assert!(!is_upper_snake_case("my_param"));
        assert!(!is_upper_snake_case("MyParam"));
    }

    #[test]
    fn camel_case_valid() {
        assert!(is_camel_case("mySignal"));
        assert!(is_camel_case("dataIn"));
    }

    #[test]
    fn camel_case_invalid() {
        assert!(!is_camel_case("MySignal"));
        assert!(!is_camel_case("my_signal"));
    }

    #[test]
    fn pascal_case_valid() {
        assert!(is_pascal_case("MyModule"));
        assert!(is_pascal_case("DataProcessor"));
    }

    #[test]
    fn pascal_case_invalid() {
        assert!(!is_pascal_case("myModule"));
        assert!(!is_pascal_case("My_Module"));
    }

    #[test]
    fn no_naming_config_reports_nothing() {
        let module = mk_module();
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        NamingViolation::default().check_module(
            design.modules.get(design.top),
            &design,
            &empty_ctx(),
            &sink,
        );
        assert!(sink.take_all().is_empty());
        assert_eq!(NamingViolation::new(None), NamingViolation::default());
        let empty = NamingConfig {
            module: None,
            signal: None,
            parameter: None,
            constant: None,
        };
        assert_eq!(
            NamingViolation::new(Some(&empty)),
            NamingViolation::default()
        );
    }

    fn all_snake_upper() -> NamingViolation {
        NamingViolation::new(Some(&NamingConfig {
            module: Some(NamingConvention::SnakeCase),
            signal: Some(NamingConvention::SnakeCase),
            parameter: Some(NamingConvention::UpperSnakeCase),
            constant: Some(NamingConvention::UpperSnakeCase),
        }))
    }

    fn mk_signal(interner: &Interner, name: &str, kind: SignalKind, span: Span) -> Signal {
        Signal {
            id: SignalId::from_raw(0),
            name: interner.get_or_intern(name),
            ty: TypeId::from_raw(0),
            kind,
            init: None,
            clock_domain: None,
            span,
        }
    }

    /// Runs `rule` on a module built by `build` with a fresh interner and an
    /// empty source database, returning the diagnostics' messages.
    fn run(rule: NamingViolation, build: impl Fn(&Interner, &mut Module)) -> Vec<Diagnostic> {
        let interner = Interner::new();
        let db = SourceDb::new();
        let mut module = mk_module();
        module.name = interner.get_or_intern("top");
        build(&interner, &mut module);
        let design = mk_design(module);
        let ctx = LintContext::new(&interner, &db);
        let sink = DiagnosticSink::new();
        rule.check_module(design.modules.get(design.top), &design, &ctx, &sink);
        sink.take_all()
    }

    #[test]
    fn violations_in_every_category_are_named() {
        let diags = run(all_snake_upper(), |i, m| {
            m.name = i.get_or_intern("TopModule");
            m.signals
                .alloc(mk_signal(i, "dataIn", SignalKind::Wire, Span::DUMMY));
            m.signals
                .alloc(mk_signal(i, "max_count", SignalKind::Const, Span::DUMMY));
            m.params.push(Parameter {
                name: i.get_or_intern("width"),
                ty: TypeId::from_raw(0),
                value: ConstValue::Int(8),
                span: Span::DUMMY,
            });
        });
        let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            msgs,
            vec![
                "module name `TopModule` does not follow the snake_case convention",
                "parameter name `width` does not follow the UPPER_SNAKE_CASE convention",
                "signal name `dataIn` does not follow the snake_case convention",
                "constant name `max_count` does not follow the UPPER_SNAKE_CASE convention",
            ]
        );
        assert!(diags.iter().all(|d| d.code.to_string() == "C201"));
        assert!(diags[0]
            .help
            .iter()
            .any(|h| h == "rename it to `top_module`"));
        assert!(diags[1].help.iter().any(|h| h == "rename it to `WIDTH`"));
        assert!(diags[2].help.iter().any(|h| h == "rename it to `data_in`"));
        assert!(diags[3]
            .help
            .iter()
            .any(|h| h == "rename it to `MAX_COUNT`"));
    }

    #[test]
    fn conforming_names_are_silent() {
        let diags = run(all_snake_upper(), |i, m| {
            m.signals
                .alloc(mk_signal(i, "data_in", SignalKind::Port, Span::DUMMY));
            m.signals
                .alloc(mk_signal(i, "_spare", SignalKind::Wire, Span::DUMMY));
            m.signals
                .alloc(mk_signal(i, "MAX", SignalKind::Const, Span::DUMMY));
            m.params.push(Parameter {
                name: i.get_or_intern("DATA_WIDTH"),
                ty: TypeId::from_raw(0),
                value: ConstValue::Int(8),
                span: Span::DUMMY,
            });
        });
        assert!(diags.is_empty(), "{diags:#?}");
    }

    #[test]
    fn unconfigured_categories_are_not_checked() {
        let rule = NamingViolation::new(Some(&NamingConfig {
            module: None,
            signal: Some(NamingConvention::CamelCase),
            parameter: None,
            constant: None,
        }));
        let diags = run(rule, |i, m| {
            m.name = i.get_or_intern("Bad_Module");
            m.signals
                .alloc(mk_signal(i, "data_in", SignalKind::Reg, Span::DUMMY));
            m.signals
                .alloc(mk_signal(i, "dataOut", SignalKind::Reg, Span::DUMMY));
        });
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].message,
            "signal name `data_in` does not follow the camelCase convention"
        );
        assert!(diags[0].help.iter().any(|h| h == "rename it to `dataIn`"));
    }

    #[test]
    fn escaped_and_scoped_names() {
        let diags = run(all_snake_upper(), |i, m| {
            // Escaped identifiers cannot follow a convention: skipped.
            m.signals
                .alloc(mk_signal(i, "\\bus-a ", SignalKind::Wire, Span::DUMMY));
            // VHDL process variables are scoped `proc.var`; only `var` counts.
            m.signals.alloc(mk_signal(
                i,
                "p_main.tmpVal",
                SignalKind::Variable,
                Span::DUMMY,
            ));
        });
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].message,
            "signal name `tmpVal` does not follow the snake_case convention"
        );
    }

    #[test]
    fn vhdl_spelling_is_taken_from_source() {
        // The VHDL parser interns `DATA_WIDTH` as `data_width`; the rule must
        // check the spelling the user wrote.
        let interner = Interner::new();
        let mut db = SourceDb::new();
        let text = "entity Top is generic (DATA_WIDTH : integer := 8); end;";
        let fid = db.add_source("top.vhd", text.to_string());
        let mut module = mk_module();
        module.name = interner.get_or_intern("top");
        module.span = Span::new(fid, 0, text.len() as u32);
        let start = text.find("DATA_WIDTH").unwrap() as u32;
        module.params.push(Parameter {
            name: interner.get_or_intern("data_width"),
            ty: TypeId::from_raw(0),
            value: ConstValue::Int(8),
            span: Span::new(fid, start, start + 10),
        });
        let design = mk_design(module);
        let ctx = LintContext::new(&interner, &db);
        let sink = DiagnosticSink::new();
        all_snake_upper().check_module(design.modules.get(design.top), &design, &ctx, &sink);
        let diags = sink.take_all();
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].message,
            "module name `Top` does not follow the snake_case convention"
        );
    }

    #[test]
    fn engine_uses_naming_config() {
        let config = aion_config::LintConfig {
            naming: Some(NamingConfig {
                module: None,
                signal: Some(NamingConvention::SnakeCase),
                parameter: None,
                constant: None,
            }),
            ..Default::default()
        };
        let engine = crate::LintEngine::new(&config);
        let interner = Interner::new();
        let db = SourceDb::new();
        let mut module = mk_module();
        module.name = interner.get_or_intern("top");
        module.signals.alloc(mk_signal(
            &interner,
            "badName",
            SignalKind::Port,
            Span::DUMMY,
        ));
        let design = mk_design(module);
        let sink = DiagnosticSink::new();
        engine.run(&design, &LintContext::new(&interner, &db), &sink);
        assert!(sink
            .take_all()
            .iter()
            .any(|d| d.code.to_string() == "C201" && d.message.contains("`badName`")));
    }

    #[test]
    fn word_splitting_and_conversion() {
        assert_eq!(split_words("HTTPServer_id"), vec!["HTTP", "Server", "id"]);
        assert_eq!(split_words("data2Out"), vec!["data2", "Out"]);
        assert_eq!(split_words("__"), Vec::<String>::new());
        assert_eq!(Case::Pascal.convert("data_in"), "DataIn");
        assert_eq!(Case::Camel.convert("DATA_IN"), "dataIn");
        assert_eq!(Case::UpperSnake.convert("dataIn"), "DATA_IN");
        assert_eq!(Case::Snake.convert("HTTPServer"), "http_server");
    }
}
