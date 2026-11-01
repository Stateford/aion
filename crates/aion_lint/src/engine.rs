//! Lint engine that manages rule registration, configuration, and execution.
//!
//! The `LintEngine` accepts a `LintConfig` to control which rules are denied,
//! allowed, or warned, then iterates over all modules in the design running
//! each enabled rule.
//!
//! Every rule invocation is isolated with [`std::panic::catch_unwind`]: a
//! rule that panics on some module is reported as an internal error
//! ([`LINT_INTERNAL_ERROR`]) and the remaining rules and modules still run.

use std::any::Any;
use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};

use aion_config::LintConfig;
use aion_diagnostics::{Diagnostic, DiagnosticSink, Severity};
use aion_ir::{Design, Module};

use crate::rules::register_builtin_rules;
use crate::{LintRule, LINT_INTERNAL_ERROR};

/// The lint engine that orchestrates running lint rules on a design.
///
/// Rules are registered at construction time. The engine respects the
/// `LintConfig` to suppress rules (allow), promote rules to errors (deny),
/// or keep them at their default severity (warn).
pub struct LintEngine {
    /// All registered lint rules.
    rules: Vec<Box<dyn LintRule>>,
    /// Rule names that should be promoted to error severity.
    denied: HashSet<String>,
    /// Rule names that should be suppressed (not reported).
    allowed: HashSet<String>,
}

impl LintEngine {
    /// Creates a new lint engine configured by the given `LintConfig`.
    ///
    /// All builtin rules are registered automatically. Rules listed in
    /// `config.deny` are promoted to error severity, and rules listed
    /// in `config.allow` are suppressed entirely.
    pub fn new(config: &LintConfig) -> Self {
        let denied: HashSet<String> = config.deny.iter().cloned().collect();
        let allowed: HashSet<String> = config.allow.iter().cloned().collect();

        let mut engine = Self {
            rules: Vec::new(),
            denied,
            allowed,
        };

        register_builtin_rules(&mut engine);
        engine
    }

    /// Creates a new lint engine with default configuration (no overrides).
    pub fn with_defaults() -> Self {
        Self::new(&LintConfig::default())
    }

    /// Registers a lint rule with the engine.
    pub fn register(&mut self, rule: Box<dyn LintRule>) {
        self.rules.push(rule);
    }

    /// Returns the number of registered rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Runs all enabled lint rules on every module in the design.
    ///
    /// For each module, each registered rule is checked. If the rule's name
    /// is in the `allowed` set, it is skipped. If it's in the `denied` set,
    /// emitted diagnostics are promoted to error severity. Otherwise the
    /// rule's default severity is used.
    ///
    /// Each rule invocation is isolated: if a rule panics while checking a
    /// module, the panic is caught, any diagnostics the rule emitted before
    /// panicking are kept, and an error with code [`LINT_INTERNAL_ERROR`]
    /// (`E199`) naming the rule and carrying the panic message is emitted at
    /// the module's span. Linting then continues with the next rule.
    pub fn run(&self, design: &Design, sink: &DiagnosticSink) {
        for (_mod_id, module) in design.modules.iter() {
            for rule in &self.rules {
                let rule_name = rule.name().to_string();

                // Skip allowed rules
                if self.allowed.contains(&rule_name) {
                    continue;
                }

                // Use a temporary sink to capture and possibly modify diagnostics
                let temp_sink = DiagnosticSink::new();
                // Rules only read the design and append to `temp_sink`, so a
                // caught panic cannot leave shared state half-updated; it is
                // therefore sound to assert unwind safety here.
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                    rule.check_module(module, design, &temp_sink);
                }));

                // Transfer diagnostics, adjusting severity if denied
                let is_denied = self.denied.contains(&rule_name);
                for mut diag in temp_sink.take_all() {
                    if is_denied {
                        diag.severity = Severity::Error;
                    }
                    sink.emit(diag);
                }

                if let Err(payload) = outcome {
                    sink.emit(rule_panic_diagnostic(
                        rule.as_ref(),
                        module,
                        payload.as_ref(),
                    ));
                }
            }
        }
    }

    /// Returns the names of all registered rules.
    pub fn rule_names(&self) -> Vec<&str> {
        self.rules.iter().map(|r| r.name()).collect()
    }

    /// Creates a diagnostic with optional severity override based on config.
    ///
    /// If the rule is in the `denied` set, the diagnostic is created as an error.
    /// Otherwise, it uses the provided default severity.
    pub fn make_diagnostic(
        &self,
        rule: &dyn LintRule,
        message: impl Into<String>,
        span: aion_source::Span,
    ) -> Diagnostic {
        let severity = if self.denied.contains(rule.name()) {
            Severity::Error
        } else {
            rule.default_severity()
        };
        match severity {
            Severity::Error => Diagnostic::error(rule.code(), message, span),
            _ => Diagnostic::warning(rule.code(), message, span),
        }
    }
}

/// Builds the internal-error diagnostic reported when `rule` panics while
/// checking `module`.
fn rule_panic_diagnostic(rule: &dyn LintRule, module: &Module, payload: &dyn Any) -> Diagnostic {
    let reason = panic_message(payload);
    Diagnostic::error(
        LINT_INTERNAL_ERROR,
        format!(
            "internal error: lint rule `{}` ({}) panicked: {reason}",
            rule.name(),
            rule.code()
        ),
        module.span,
    )
    .with_note("this is a bug in Aion, not in your design; please report it")
    .with_note(format!(
        "rule `{}` did not finish checking this module, so its results may be incomplete; other rules still ran",
        rule.name()
    ))
    .with_help(format!(
        "add \"{}\" to `allow` in the `[lint]` section of aion.toml to skip this rule until it is fixed",
        rule.name()
    ))
}

/// Extracts a human-readable message from a panic payload.
///
/// `panic!` with a literal produces a `&'static str` payload and a formatted
/// `panic!` produces a `String`; anything else gets a generic description.
fn panic_message(payload: &dyn Any) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_diagnostics::{Category, DiagnosticCode};
    use aion_ir::*;
    use aion_source::Span;

    struct DummyRule;
    impl LintRule for DummyRule {
        fn code(&self) -> DiagnosticCode {
            DiagnosticCode::new(Category::Warning, 999)
        }
        fn name(&self) -> &str {
            "dummy-rule"
        }
        fn description(&self) -> &str {
            "a test rule"
        }
        fn default_severity(&self) -> Severity {
            Severity::Warning
        }
        fn check_module(&self, _module: &Module, _design: &Design, sink: &DiagnosticSink) {
            sink.emit(Diagnostic::warning(
                self.code(),
                "dummy warning",
                Span::DUMMY,
            ));
        }
    }

    fn mk_empty_design() -> Design {
        let mut modules = Arena::new();
        let top = modules.alloc(Module {
            id: ModuleId::from_raw(0),
            name: aion_common::Ident::from_raw(0),
            span: Span::DUMMY,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Arena::new(),
            cells: Arena::new(),
            processes: Arena::new(),
            assignments: Vec::new(),
            clock_domains: Vec::new(),
            content_hash: aion_common::ContentHash::from_bytes(&[]),
        });
        Design {
            modules,
            top,
            types: TypeDb::new(),
            source_map: SourceMap::new(),
        }
    }

    #[test]
    fn engine_registers_builtin_rules() {
        let engine = LintEngine::with_defaults();
        assert_eq!(engine.rule_count(), 15);
    }

    #[test]
    fn engine_custom_rule() {
        let mut engine = LintEngine::with_defaults();
        let initial_count = engine.rule_count();
        engine.register(Box::new(DummyRule));
        assert_eq!(engine.rule_count(), initial_count + 1);
    }

    #[test]
    fn engine_run_emits_diagnostics() {
        let config = LintConfig::default();
        let mut engine = LintEngine::new(&config);
        engine.register(Box::new(DummyRule));
        let design = mk_empty_design();
        let sink = DiagnosticSink::new();
        engine.run(&design, &sink);
        let diags = sink.take_all();
        // DummyRule fires once per module, there's 1 module
        assert!(diags.iter().any(|d| d.message == "dummy warning"));
    }

    #[test]
    fn engine_allow_suppresses_rule() {
        let config = LintConfig {
            deny: Vec::new(),
            allow: vec!["dummy-rule".to_string()],
            warn: Vec::new(),
            naming: None,
        };
        let mut engine = LintEngine::new(&config);
        engine.register(Box::new(DummyRule));
        let design = mk_empty_design();
        let sink = DiagnosticSink::new();
        engine.run(&design, &sink);
        let diags = sink.take_all();
        assert!(
            !diags.iter().any(|d| d.message == "dummy warning"),
            "allowed rule should be suppressed"
        );
    }

    #[test]
    fn engine_deny_promotes_severity() {
        let config = LintConfig {
            deny: vec!["dummy-rule".to_string()],
            allow: Vec::new(),
            warn: Vec::new(),
            naming: None,
        };
        let mut engine = LintEngine::new(&config);
        engine.register(Box::new(DummyRule));
        let design = mk_empty_design();
        let sink = DiagnosticSink::new();
        engine.run(&design, &sink);
        let diags = sink.take_all();
        let dummy_diags: Vec<_> = diags
            .iter()
            .filter(|d| d.message == "dummy warning")
            .collect();
        assert!(!dummy_diags.is_empty());
        assert_eq!(dummy_diags[0].severity, Severity::Error);
    }

    #[test]
    fn engine_rule_names() {
        let engine = LintEngine::with_defaults();
        let names = engine.rule_names();
        assert!(names.contains(&"unused-signal"));
        assert!(names.contains(&"multiple-drivers"));
        assert!(names.contains(&"naming-violation"));
    }

    #[test]
    fn make_diagnostic_default_severity() {
        let engine = LintEngine::with_defaults();
        let rule = DummyRule;
        let diag = engine.make_diagnostic(&rule, "test", Span::DUMMY);
        assert_eq!(diag.severity, Severity::Warning);
    }

    #[test]
    fn make_diagnostic_denied_becomes_error() {
        let config = LintConfig {
            deny: vec!["dummy-rule".to_string()],
            allow: Vec::new(),
            warn: Vec::new(),
            naming: None,
        };
        let engine = LintEngine::new(&config);
        let rule = DummyRule;
        let diag = engine.make_diagnostic(&rule, "test", Span::DUMMY);
        assert_eq!(diag.severity, Severity::Error);
    }

    // ---- Rule panic isolation (issue #38) ----

    /// A rule that emits one warning and then panics with a formatted message
    /// (a `String` payload), simulating an arithmetic-overflow bug.
    struct PanickingRule;
    impl LintRule for PanickingRule {
        fn code(&self) -> DiagnosticCode {
            DiagnosticCode::new(Category::Warning, 998)
        }
        fn name(&self) -> &str {
            "panicking-rule"
        }
        fn description(&self) -> &str {
            "a test rule that always panics"
        }
        fn default_severity(&self) -> Severity {
            Severity::Warning
        }
        fn check_module(&self, _module: &Module, _design: &Design, sink: &DiagnosticSink) {
            sink.emit(Diagnostic::warning(
                self.code(),
                "emitted before panic",
                Span::DUMMY,
            ));
            let what = "subtract";
            panic!("attempt to {what} with overflow");
        }
    }

    /// A rule that panics with a `&'static str` payload.
    struct StrPanickingRule;
    impl LintRule for StrPanickingRule {
        fn code(&self) -> DiagnosticCode {
            DiagnosticCode::new(Category::Warning, 997)
        }
        fn name(&self) -> &str {
            "str-panicking-rule"
        }
        fn description(&self) -> &str {
            "a test rule that panics with a static message"
        }
        fn default_severity(&self) -> Severity {
            Severity::Warning
        }
        fn check_module(&self, _module: &Module, _design: &Design, _sink: &DiagnosticSink) {
            panic!("static boom");
        }
    }

    fn internal_errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
        diags
            .iter()
            .filter(|d| d.code == LINT_INTERNAL_ERROR)
            .collect()
    }

    #[test]
    fn panicking_rule_is_isolated_and_reported() {
        let mut engine = LintEngine::with_defaults();
        engine.register(Box::new(PanickingRule));
        // Registered after the panicking rule: must still run.
        engine.register(Box::new(DummyRule));
        let design = mk_empty_design();
        let sink = DiagnosticSink::new();
        engine.run(&design, &sink);
        let diags = sink.take_all();

        assert!(
            diags.iter().any(|d| d.message == "dummy warning"),
            "rules after a panicking rule must still run"
        );
        let internal = internal_errors(&diags);
        assert_eq!(internal.len(), 1, "{diags:#?}");
        let ice = internal[0];
        assert_eq!(ice.severity, Severity::Error);
        assert_eq!(ice.code.to_string(), "E199");
        assert!(ice.message.contains("panicking-rule"), "{}", ice.message);
        assert!(ice.message.contains("W998"), "{}", ice.message);
        assert!(
            ice.message.contains("attempt to subtract with overflow"),
            "panic message must be part of the diagnostic: {}",
            ice.message
        );
        assert_eq!(ice.primary_span, design.modules.get(design.top).span);
        assert!(ice.help.iter().any(|h| h.contains("panicking-rule")));
        assert!(
            sink.has_errors(),
            "an internal error must count as an error"
        );
    }

    #[test]
    fn panicking_rule_keeps_diagnostics_emitted_before_panic() {
        let mut engine = LintEngine::with_defaults();
        engine.register(Box::new(PanickingRule));
        let design = mk_empty_design();
        let sink = DiagnosticSink::new();
        engine.run(&design, &sink);
        let diags = sink.take_all();
        assert!(diags.iter().any(|d| d.message == "emitted before panic"));
    }

    #[test]
    fn panicking_rule_runs_on_every_module() {
        let mut design = mk_empty_design();
        let second = design.modules.get(design.top).clone();
        design.modules.alloc(second);
        let mut engine = LintEngine::with_defaults();
        engine.register(Box::new(StrPanickingRule));
        engine.register(Box::new(DummyRule));
        let sink = DiagnosticSink::new();
        engine.run(&design, &sink);
        let diags = sink.take_all();
        let internal = internal_errors(&diags);
        assert_eq!(internal.len(), 2, "one internal error per module");
        assert!(internal[0].message.contains("static boom"));
        let dummy = diags
            .iter()
            .filter(|d| d.message == "dummy warning")
            .count();
        assert_eq!(dummy, 2, "the healthy rule must run on both modules");
    }

    #[test]
    fn allowed_panicking_rule_is_not_run() {
        let config = LintConfig {
            deny: Vec::new(),
            allow: vec!["panicking-rule".to_string()],
            warn: Vec::new(),
            naming: None,
        };
        let mut engine = LintEngine::new(&config);
        engine.register(Box::new(PanickingRule));
        let design = mk_empty_design();
        let sink = DiagnosticSink::new();
        engine.run(&design, &sink);
        assert!(internal_errors(&sink.take_all()).is_empty());
    }

    #[test]
    fn panic_message_handles_payload_kinds() {
        assert_eq!(panic_message(&"literal"), "literal");
        assert_eq!(panic_message(&String::from("formatted")), "formatted");
        assert_eq!(panic_message(&42u32), "unknown panic payload");
    }
}
