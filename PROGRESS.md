# Aion — Implementation Progress

**Started:** 2026-02-07
**Current Phase:** Phase 0 — Foundation

---

## Phase 0 — Foundation (Months 1–4)

**Goal:** Parse all three HDL languages and produce useful lint output.

### Crate Status

| Crate | Status | Tests | Notes |
|-------|--------|-------|-------|
| `aion_common` | 🟢 Complete | 45 | Ident, Interner, ContentHash, Frequency, Logic, LogicVec, AionResult |
| `aion_source` | 🟢 Complete | 22 | FileId, Span, SourceFile, SourceDb, ResolvedSpan |
| `aion_diagnostics` | 🟢 Complete | 22 | Severity, DiagnosticCode, Label, Diagnostic, DiagnosticSink, TerminalRenderer |
| `aion_config` | 🟢 Complete | 22 | ProjectConfig, all config types, loader, validator, target resolver |
| `aion_ir` | 🟡 Stub only | — | Core IR types (Design, Module, Signal, Cell, etc.) |
| `aion_vhdl_parser` | 🟡 Stub only | — | Full VHDL-2008 recursive descent parser |
| `aion_verilog_parser` | 🟡 Stub only | — | Full Verilog-2005 recursive descent parser |
| `aion_sv_parser` | 🟡 Stub only | — | SystemVerilog-2017 parser (synth subset priority) |
| `aion_elaborate` | 🟡 Stub only | — | Basic elaboration (hierarchy resolution) |
| `aion_lint` | 🟡 Stub only | — | W101-W108, E101-E105, C201-C204 rules |
| `aion_cache` | 🟡 Stub only | — | Content-hash caching for parsed ASTs |
| `aion_cli` | 🟡 Stub only | — | `init`, `lint` commands |

### Phase 0 Checklist

- [x] Rust workspace with Cargo.toml configured
- [ ] CI/CD pipeline (GitHub Actions)
- [x] `aion_common` — all foundational types
- [x] `aion_source` — source file management and spans
- [x] `aion_diagnostics` — diagnostic types and terminal renderer
- [x] `aion_config` — aion.toml parser
- [ ] `aion_vhdl_parser` — full grammar coverage
- [ ] `aion_verilog_parser` — full grammar coverage
- [ ] `aion_sv_parser` — synthesizable subset
- [ ] `aion_ir` — core IR type definitions
- [ ] `aion_elaborate` — basic hierarchy resolution
- [ ] `aion_lint` — syntax checking, basic semantic analysis
- [ ] `aion_cli` — `init` and `lint` commands
- [ ] `aion_cache` — basic content-hash caching
- [ ] Human-readable error output with source spans
- [ ] Parse + lint completes in <1s on test projects

### Milestone Criteria

- [ ] All three parsers pass conformance tests on open-source HDL projects
- [ ] `aion lint` produces useful diagnostics on real designs
- [ ] Parse + lint < 1s on any reasonable project
- [ ] Error recovery produces multiple diagnostics per file

---

## Implementation Log

<!-- Entries are prepended here, newest first -->

#### 2026-02-07 — Workspace scaffolding + foundation crates

**Crates:** `aion_common`, `aion_source`, `aion_diagnostics`, `aion_config` + 8 stubs

**What:** Created the Cargo workspace with 12 crate stubs and implemented the four foundation crates:
- `aion_common` — Ident/Interner (lasso-backed), ContentHash (XXH3-128), Frequency (with FromStr/Display), Logic (4-state IEEE 1164 with truth tables), LogicVec (2-bit packed), AionResult/InternalError
- `aion_source` — FileId, Span (with merge/dummy), SourceFile (with line_starts + line_col), SourceDb (load_file + add_source), ResolvedSpan
- `aion_diagnostics` — Severity, DiagnosticCode/Category, Label, SuggestedFix/Replacement, Diagnostic (builder pattern), DiagnosticSink (thread-safe with Mutex+AtomicUsize), TerminalRenderer (rustc-style plain text)
- `aion_config` — Full ProjectConfig tree (ProjectMeta, TargetConfig, PinAssignment, ClockDef, DependencySpec, BuildConfig, OptLevel, TestConfig, WaveformFormat, LintConfig, NamingConfig, NamingConvention), ConfigError, load_config/load_config_from_str, validate_config, resolve_target with pin merging

**Tests added:**
- 45 tests in aion_common (intern roundtrip, hash determinism, frequency parsing, logic truth tables, LogicVec packing/ops, serde roundtrips)
- 22 tests in aion_source (FileId roundtrip/dummy/serde, span merge, line_col, snippet, SourceDb add/load/resolve, ResolvedSpan equality/display)
- 22 tests in aion_diagnostics (severity ordering, code display, builder, with_fix, thread-safety with 10 threads x 100 emissions, renderer output)
- 22 tests in aion_config (minimal/full config parse, missing fields, invalid TOML, defaults, dependency specs incl Registry, target resolution, pin merging, constraint override, all enum variant deserialization, all ConfigError display variants)

**Test results:** 111 passed, 0 failed
**Clippy:** ✅ Clean (zero warnings with -D warnings)
**Next:** Implement `aion_ir` core types, then parsers
**Decisions/Blockers:**
- Used `lasso = { features = ["multi-threaded", "serialize"] }` for ThreadedRodeo
- Omitted `Backtrace` from InternalError (requires nightly features); kept it simple with `message: String`
- Pinned `zerocopy` to 0.8.25 for compatibility with current nightly Rust
- Implemented Clone manually for OptLevel/ConstraintConfig/BuildConfig in resolve.rs rather than adding derive macros

---

## Phase 1 — Simulation (Months 4–8)

_Not yet started. See `docs/aion-prd.md` §15 and `docs/aion-technical-spec.md` §25._

## Phase 2 — Synthesis (Months 8–14)

_Not yet started._

## Phase 3 — Place & Route (Months 14–22)

_Not yet started._

## Phase 4 — Polish & Ecosystem (Months 22–28)

_Not yet started._
