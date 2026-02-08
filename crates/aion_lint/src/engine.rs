//! Lint engine that manages rule registration, configuration, and execution.
//!
//! The `LintEngine` accepts a `LintConfig` to control which rules are denied,
//! allowed, or warned, then iterates over all modules in the design running
//! each enabled rule.

use std::collections::HashSet;

use aion_config::LintConfig;
use aion_diagnostics::{Diagnostic, DiagnosticSink, Severity};
use aion_ir::Design;

use crate::rules::register_builtin_rules;
use crate::LintRule;

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
                rule.check_module(module, design, &temp_sink);

                // Transfer diagnostics, adjusting severity if denied
                let is_denied = self.denied.contains(&rule_name);
                for mut diag in temp_sink.take_all() {
                    if is_denied {
                        diag.severity = Severity::Error;
                    }
                    sink.emit(diag);
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
}
