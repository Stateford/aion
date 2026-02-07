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
| `aion_ir` | 🟢 Complete | 77 | Arena, IDs, TypeDb, Design, Module, Signal, Cell, Process, Expr, Statement, SourceMap |
| `aion_vhdl_parser` | 🟢 Complete | 85 | Lexer, Pratt parser, full AST, error recovery, serde |
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
- [x] `aion_vhdl_parser` — full grammar coverage
- [ ] `aion_verilog_parser` — full grammar coverage
- [ ] `aion_sv_parser` — synthesizable subset
- [x] `aion_ir` — core IR type definitions
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

#### 2026-02-07 — aion_vhdl_parser full VHDL-2008 parser

**Crate:** `aion_vhdl_parser`

**What:** Implemented a complete hand-rolled recursive descent VHDL-2008 parser across 8 modules:
- `token` — `VhdlToken` enum (~95 keywords + operators + literals + punctuation), `Token` struct, `lookup_keyword()` function
- `lexer` — Full lexer with case-insensitive keyword matching, based literals (`16#FF#`), character/string/bit-string literals, line comments (`--`), nested block comments (`/* */`), extended identifiers (`\foo\`), error recovery
- `ast` — ~60 AST node types with `Span` on every node, serde derives, `Error` variants for recovery in DesignUnit/Declaration/ConcurrentStatement/SequentialStatement/Expr
- `parser` — `VhdlParser` struct with primitives (advance/eat/expect), error recovery (recover_to_semicolon), top-level rules (design file, entity, architecture, package, package body, generics, ports, interface lists)
- `expr` — Pratt expression parser with correct VHDL precedence (7 levels), physical literal support (`10 ns`), name parsing with dot/index/slice/attribute suffixes, aggregates, qualified expressions
- `types` — Type indication parsing with range constraints, index constraints, discrete ranges
- `decl` — All declaration types: signal, variable, constant, type (enum/range/array/record), subtype, component, function, procedure, alias, attribute declaration/specification
- `stmt` — Concurrent statements (process, signal assignment, component instantiation, for-generate, if-generate, assert) and sequential statements (if/elsif/else, case/when, for/while/loop, next, exit, return, wait, assert, report, null, variable/signal assignment, procedure call)
- `lib` — Public API `parse_file()` wiring lexer → parser

**Key design decisions:**
- `<=` always lexed as `LessEquals`; parser disambiguates by parsing targets as name expressions
- Physical literals (`10 ns`) handled by consuming unit identifier after numeric literal in Pratt parser
- Error recovery via `Error(Span)` poison nodes and `recover_to_semicolon()`
- VHDL-2008 features: `process(all)`, nested block comments, matching operators (`?=`, `?/=`, etc.)

**Tests added:** 85 tests
- 25 lexer tests (keywords, identifiers, all literal types, operators, comments, error cases, spans)
- 5 AST serde roundtrip tests
- 45 parser tests (entity/arch/package parsing, all declaration types, expression precedence/associativity/unary/parens/aggregates, all statement types, error recovery)
- 10 integration tests (counter entity+arch, multiplexer with case, package+body, multi-unit file, error recovery, component instantiation, generate statements, wait with time, signal assignment with after, serde roundtrip)

**Test results:** 273 passed, 0 failed (188 previous + 85 new)
**Clippy:** ✅ Clean (zero warnings with -D warnings)
**Next:** Implement `aion_verilog_parser` and `aion_sv_parser`

---

#### 2026-02-07 — aion_ir core IR types

**Crate:** `aion_ir`

**What:** Implemented all core IR types from the technical spec across 12 submodules:
- `arena` — Generic `Arena<I, T>` container with dense ID-indexed storage, O(1) alloc/lookup, Index/IndexMut impls, serde support
- `ids` — 7 opaque ID newtypes via macro: ModuleId, SignalId, CellId, ProcessId, PortId, TypeId, ClockDomainId
- `types` — `Type` enum (Bit, BitVec, Integer, Real, Bool, Str, Array, Enum, Record, Error) + `TypeDb` with interning and `bit_width()` computation
- `port` — `Port` struct with `PortDirection` enum (Input, Output, InOut)
- `signal` — `Signal` struct with `SignalKind` (Wire, Reg, Latch, Port, Const) + `SignalRef` (Signal, Slice, Concat, Const)
- `const_value` — `ConstValue` enum (Int, Real, Logic, String, Bool)
- `cell` — `Cell` + `CellKind` (Instance, And/Or/Xor/Not/Mux/Add/Sub/Mul/Shl/Shr/Eq/Lt/Concat/Slice/Repeat/Const, Dff/Latch, Memory, Lut/Carry/Bram/Dsp/Pll/Iobuf, BlackBox) + `Connection`, config structs
- `process` — `Process` with `ProcessKind` (Combinational, Sequential, Latched, Initial), `Sensitivity` (All, EdgeList, SignalList), `Edge`
- `expr` — `Expr` tree (Signal, Literal, Unary, Binary, Ternary, FuncCall, Concat, Repeat, Index, Slice) + `UnaryOp` (6 variants) + `BinaryOp` (19 variants)
- `stmt` — `Statement` enum (Assign, If, Case, Block, Wait, Assertion, Display, Finish, Nop) + `CaseArm`, `AssertionKind`
- `source_map` — `SourceMap` with per-module scoping for signals, cells, processes
- `module` — `Module` with signals/cells/processes arenas, `Parameter`, `Assignment`, `ClockDomain`
- `design` — `Design` top-level container with modules arena, type db, source map

Also added `Ident::from_raw()`/`as_raw()` to `aion_common` for IR test construction.

**Tests added:** 77 tests in aion_ir (arena alloc/get/iter/serde, ID roundtrip/equality/hash/serde, TypeDb intern/dedup/bit_width, all cell kinds, all signal kinds/refs, all process kinds/sensitivities, all expr/stmt variants, source map scoped lookups, module/design construction/serde)

**Test results:** 188 passed, 0 failed (111 previous + 77 new)
**Clippy:** ✅ Clean (zero warnings with -D warnings)
**Next:** Implement parsers (VHDL, Verilog, SystemVerilog)

---

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
