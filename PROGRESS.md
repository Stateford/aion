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
| `aion_verilog_parser` | 🟢 Complete | 127 | Lexer, Pratt parser, full AST, error recovery, serde |
| `aion_sv_parser` | 🟢 Complete | 166 | Lexer, Pratt parser, full AST, error recovery, serde |
| `aion_elaborate` | 🟢 Complete | 113 | AST→IR elaboration: registry, const eval, type resolution, expr/stmt lowering, all 3 languages |
| `aion_lint` | 🟢 Complete | 91 | LintEngine, 15 rules (W101-W108, E102/E104/E105, C201-C204), IR traversal helpers |
| `aion_cache` | 🟡 Stub only | — | Content-hash caching for parsed ASTs |
| `aion_cli` | 🟢 Complete | 38 | CLI entry point: `init` (scaffolding) and `lint` (full pipeline) commands |

### Phase 0 Checklist

- [x] Rust workspace with Cargo.toml configured
- [ ] CI/CD pipeline (GitHub Actions)
- [x] `aion_common` — all foundational types
- [x] `aion_source` — source file management and spans
- [x] `aion_diagnostics` — diagnostic types and terminal renderer
- [x] `aion_config` — aion.toml parser
- [x] `aion_vhdl_parser` — full grammar coverage
- [x] `aion_verilog_parser` — full grammar coverage
- [x] `aion_sv_parser` — synthesizable subset
- [x] `aion_ir` — core IR type definitions
- [x] `aion_elaborate` — AST→IR elaboration engine
- [x] `aion_lint` — lint rules and engine (15 rules)
- [x] `aion_cli` — `init` and `lint` commands
- [ ] `aion_cache` — basic content-hash caching
- [x] Human-readable error output with source spans
- [ ] Parse + lint completes in <1s on test projects

### Milestone Criteria

- [ ] All three parsers pass conformance tests on open-source HDL projects
- [ ] `aion lint` produces useful diagnostics on real designs
- [ ] Parse + lint < 1s on any reasonable project
- [ ] Error recovery produces multiple diagnostics per file

---

## Implementation Log

<!-- Entries are prepended here, newest first -->

#### 2026-02-07 — aion_cli init and lint commands

**Crate:** `aion_cli`

**What:** Implemented the CLI entry point with two fully functional commands:

- `main.rs` — Clap-based CLI with `Cli` struct (derive API), `Command` enum (`Init`/`Lint` variants), supporting enums (`ColorChoice`, `HdlLanguage`, `ReportFormat`), `GlobalArgs` for resolved settings, main dispatch loop with exit codes, basic terminal detection
- `init.rs` — `aion init` project scaffolding:
  - Creates standard directory structure: `src/`, `tests/`, `constraints/`, `ip/`
  - Generates `aion.toml` with project metadata (parseable by `aion_config`)
  - Generates template top module and testbench for all 3 languages (SystemVerilog, Verilog, VHDL)
  - Optional `--target` flag adds `[targets.default]` section
  - Cargo-style progress messages
- `lint.rs` — `aion lint` full static analysis pipeline:
  - Finds project root by walking up directories for `aion.toml`
  - Loads config via `aion_config::load_config()`
  - Discovers HDL source files recursively in `src/` (`.v`, `.sv`, `.vhd`, `.vhdl`)
  - Parses each file with the correct language parser
  - Elaborates into unified IR via `aion_elaborate::elaborate()`
  - Merges CLI `--allow`/`--deny` flags with `aion.toml` lint config (CLI takes precedence)
  - Runs `LintEngine` with 15 rules
  - Renders diagnostics via `TerminalRenderer` (text) or JSON output
  - Prints summary (error/warning counts), exits 1 on errors

**Key design decisions:**
- CLI flags: `--quiet`, `--verbose`, `--color`, `--config` as global args; subcommand-specific args under each command
- `init` extracts project name from directory basename (not full path)
- `lint` uses `find_project_root()` for Cargo-like discovery from any subdirectory
- `merge_lint_config()` gives CLI flags precedence over `aion.toml` settings
- JSON output via `serde_json` for machine-readable diagnostics
- End-to-end test: `init` a project then `lint` it — verifying the full pipeline works

**Tests added:** 38 tests
- 13 main.rs tests (clap parsing: init default/with-args, lint default/with-args, global flags quiet/verbose/color variants, config path, language variants, multiple allow)
- 10 init.rs tests (directory structure creation, VHDL/Verilog/SV file generation, valid toml generation, target section, existing dir error, current dir init, extension mappings)
- 15 lint.rs tests (find_project_root current/parent/not-found, detect_language all variants/unknown, discover_files finds-hdl/recursive/empty, merge_config deny-overrides/allow-overrides/combines/empty, end-to-end init+lint)

**Test results:** 808 passed, 0 failed (770 previous + 38 new)
**Clippy:** Clean (zero warnings with -D warnings)
**Docs:** Clean (zero warnings from `cargo doc`)
**Next:** CI/CD pipeline, `aion_cache` implementation, conformance testing on real HDL projects

---

#### 2026-02-08 — aion_lint lint rules and engine

**Crate:** `aion_lint`

**What:** Implemented a full lint engine with 15 rules across 3 categories, plus IR traversal helpers:

- `lib.rs` — `LintRule` trait (code, name, description, default_severity, check_module), public API re-exports
- `engine.rs` — `LintEngine` struct: rule registration, `LintConfig`-based deny/allow/warn lists, severity override, `run()` loop over all modules, `make_diagnostic()` helper
- `helpers.rs` — IR traversal utilities: `collect_read_signals`, `collect_written_signals`, `collect_expr_signals`, `collect_signal_ref_signals`, `is_signal_read_in_module`, `is_signal_driven_in_module`, `count_drivers`, `stmt_has_full_else_coverage`, `has_assign`, `check_cell_port_match`
- `rules/` — 15 individual rule files:

**Warning rules (W101-W108):**
- W101 `unused-signal` — Signal declared but never read (skips Port/Const kinds)
- W102 `undriven-signal` — Signal never assigned/driven (skips Input ports and Const)
- W103 `width-mismatch` — LHS and RHS of assignment have different bit widths
- W104 `missing-reset` — Sequential process has no reset in sensitivity or body
- W105 `incomplete-sensitivity` — Combinational process with SignalList missing read signals
- W106 `latch-inferred` — Combinational process if without else or case without default
- W107 `truncation` — RHS wider than LHS causing bit truncation
- W108 `dead-logic` — Code after $finish, always-true/false conditions

**Error rules (E102, E104, E105):**
- E102 `non-synthesizable` — Initial blocks, Wait/Display/Finish in non-initial processes
- E104 `multiple-drivers` — Wire signal driven by >1 concurrent source
- E105 `port-mismatch` — Cell instance connections don't match module ports

**Convention rules (C201-C204):**
- C201 `naming-violation` — Naming convention utilities (snake_case, UPPER_SNAKE_CASE, camelCase, PascalCase)
- C202 `missing-doc` — Stub for module documentation check (needs source text access)
- C203 `magic-number` — Literal values >1 bit and not 0/1 used directly in expressions
- C204 `inconsistent-style` — Detects latched process kind as potential style issue

**Also added to `aion_common`:** `LogicVec::from_bool()`, `from_u64()`, `to_u64()`, `is_all_zero()`, `is_all_one()` utility methods.

**Key design decisions:**
- `LintRule` trait is Send+Sync for future parallel module analysis
- Engine uses temporary `DiagnosticSink` per rule to enable severity override without modifying rule logic
- Rules operate on `Module` + `Design` references — no interner access (naming rules are stubs for now)
- `PortMatchIssue` enum returned from `check_cell_port_match` for structured error reporting
- C201 naming utilities exported as standalone functions for reuse
- C202 is a stub pending source text / interner access in future engine refactor

**Tests added:** 91 tests
- 8 engine tests (register builtin rules count, custom rule, run emits diagnostics, allow suppresses, deny promotes severity, rule names, make_diagnostic default/denied)
- 22 helpers tests (collect_signal_ref for Signal/Slice/Concat/Const, collect_expr_signals for signal/binary/literal, collect_read_signals for assign/if, collect_written_signals for assign/block, signal_read/driven_in_module, stmt coverage if-with-else/without-else/case-with-default/without-default, count_drivers multiple/none, has_assign block/nop)
- 4 W101 tests (unused fires, used no warning, port skipped, const skipped)
- 5 W102 tests (undriven fires, driven no warning, input port skipped, output port undriven fires, const skipped)
- 3 W103 tests (width mismatch fires, matching widths no warning, mismatch in process)
- 4 W104 tests (missing reset fires, async reset no warning, sync reset no warning, combinational skipped)
- 3 W105 tests (incomplete sensitivity fires, complete no warning, sensitivity all skipped)
- 4 W106 tests (if without else, if with else, case without default, sequential skipped)
- 3 W107 tests (truncation fires, same width no warning, rhs narrower no warning)
- 4 W108 tests (dead logic after finish, always true, always false, normal no warning)
- 4 E102 tests (initial block fires, wait fires, display fires, normal no error)
- 3 E104 tests (multiple drivers fires, single driver no error, reg skipped)
- 4 E105 tests (missing port fires, extra port fires, matching ports no error, non-instance skipped)
- 10 C201 tests (snake_case valid/invalid, upper_snake_case valid/invalid, camelCase valid/invalid, PascalCase valid/invalid, no false positives)
- 2 C202 tests (stub no diagnostics, rule metadata)
- 5 C203 tests (magic number fires, zero no warning, one-bit no warning, all-ones no warning, magic in process)
- 4 C204 tests (latched fires, combinational no warning, sequential no warning, rule metadata)

**Test results:** 770 passed, 0 failed (679 previous + 91 new)
**Clippy:** Clean (zero warnings with -D warnings)
**Docs:** Clean (zero warnings from `cargo doc`)
**Next:** Implement `aion_cli` (init and lint commands)

---

#### 2026-02-08 — aion_elaborate AST→AionIR elaboration engine

**Crate:** `aion_elaborate`

**What:** Implemented a full AST-to-AionIR elaboration engine across 10 modules:
- `errors` — 12 error codes (E200–E211) and 2 warning codes (W200–W201) with helper functions for all elaboration diagnostics: unknown module, port mismatch, duplicate module/signal, unknown signal/port, type mismatch, top not found, circular instantiation, param eval failure, unsupported construct, no architecture
- `const_eval` — Constant expression evaluator supporting all 3 languages: integer/sized/hex/octal literals parsed from source text, binary arithmetic (+, -, *, /, %), identifier lookup from `ConstEnv`, `$clog2` builtin, range evaluation for Verilog/SV, VHDL integer literals and names
- `types` — Type resolution: Verilog net types (wire/reg/integer/real with ranges), SV port/var types (logic/bit/byte/shortint/int/longint with ranges), VHDL type indications (std_logic, std_logic_vector with constraints, integer, boolean, signed/unsigned)
- `registry` — `ModuleRegistry` scanning all parsed files: Verilog modules, SV modules, VHDL entity/architecture pairs. O(1) lookup by interned name, duplicate detection across languages
- `context` — `ElaborationContext` holding mutable state: Design under construction, registry reference, elaboration cache (name+param_hash → ModuleId), elaboration stack for cycle detection, port ID allocation
- `expr` — Expression lowering from all 3 AST types to IR `Expr`: identifiers → signal lookup, literals → `LogicVec`, binary/unary/ternary ops with operator mapping, concat, repeat, index/slice. VHDL bit strings, character literals, aggregates
- `stmt` — Statement lowering from all 3 AST types to IR `Statement`: blocking/nonblocking assign, if/case, blocks, event control passthrough. SV compound assignments expand to binary op + assign. SV incr/decr expand to +1/-1
- `verilog` — Verilog module elaboration: parameter application with overrides, ANSI/non-ANSI port elaboration, all module items (net/reg/integer/real declarations, continuous assigns, always/initial blocks with sensitivity analysis, module instantiation with cross-language support, generate for/if)
- `sv` — SystemVerilog module elaboration: same structure as Verilog plus `always_comb`/`always_ff`/`always_latch` with correct ProcessKind mapping, VarDecl with full type support, sensitivity list extraction from always_ff blocks
- `vhdl` — VHDL entity+architecture elaboration: generic application, port elaboration from InterfaceDecl (multiple names per decl), architecture declarations (signals, constants), concurrent statements (process with sensitivity, signal assignment, component instantiation with generic map)
- `lib` — Public API: `ParsedDesign` struct, `elaborate()` function, 11 integration tests

**Key design decisions:**
- `push_elab_stack` returns `bool` (not `Result`) — `false` on cycle, emits E207 diagnostic
- Module cache keyed by `(Ident, param_hash)` — same module with same params reuses ModuleId
- Borrow conflict resolution: `&mut ctx.design.types` used directly instead of `ctx.types()` in elaboration functions to avoid mutable borrow conflicts with `ctx.source_db`/`ctx.interner`/`ctx.sink`
- Cross-language instantiation: any module can instantiate any language's module via registry lookup
- VHDL architecture selection: last declared architecture (VHDL convention)
- Literals parsed from source text via `source_db.snippet(span)` since parsers store Spans not values

**Tests added:** 113 tests
- 31 const_eval tests (literal parsing for decimal/sized/hex/octal/underscore, Verilog/SV/VHDL expression evaluation, binary arithmetic, $clog2, identifier lookup, range evaluation, non-constant diagnostics, const_to_i64 coercion)
- 14 errors tests (all 12 error codes + 2 warning codes verified for correct code, severity, message content)
- 9 types tests (Verilog bit/bitvec, SV logic/byte/int, VHDL std_logic/std_logic_vector/integer/unknown)
- 6 registry tests (empty, Verilog/SV module registration, duplicate detection, lookup miss, VHDL entity without arch)
- 9 context tests (construction, cache hit/miss/different params, elab stack push/pop/cycle/no-false-positive, types access, port ID allocation)
- 15 expr tests (Verilog/SV identifiers, literals, binary ops, concat, ternary, unknown signal, VHDL names, bit strings, char literals, signal ref lowering)
- 8 stmt tests (blocking assign, if, case, block, event control, SV compound assign, SV incr, VHDL signal assign)
- 5 verilog tests (empty module, ports, wire/reg declarations, continuous assign, always block)
- 3 sv tests (empty module, logic port, always_comb)
- 3 vhdl tests (empty entity, ports, architecture with signal)
- 11 integration tests (simple counter, hierarchy, SV always_ff, VHDL entity+arch, unknown top E206, unknown instantiation E200, mixed language, cache reuse, empty design, serde roundtrip, always_comb combinational)

**Test results:** 679 passed, 0 failed (566 previous + 113 new)
**Clippy:** Clean (zero warnings with -D warnings)
**Next:** Implement `aion_lint` (lint rules and engine)

---

#### 2026-02-08 — aion_sv_parser full SystemVerilog-2017 parser

**Crate:** `aion_sv_parser`

**What:** Implemented a complete hand-rolled recursive descent SystemVerilog-2017 parser (synthesizable subset) across 8 modules:
- `token` — `SvToken` enum (~100 variants: all Verilog-2005 keywords + ~45 SV keywords like `logic`, `bit`, `int`, `enum`, `struct`, `typedef`, `interface`, `package`, `always_comb`, `always_ff`, `always_latch`, `import`, `modport`, `unique`, `priority`, `return`, `break`, `continue` + ~45 operators including `++`, `--`, `+=`, `-=`, `::`, `->`, `==?`, `!=?`, `'`), `Token` struct, `lookup_keyword()`, predicates (`is_keyword`, `is_direction`, `is_net_type`, `is_data_type`, `is_always_variant`, `is_assignment_op`)
- `lexer` — Full lexer with all Verilog-2005 features + SV-specific operators (`++`, `--`, `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`, `<<<`, `>>>`, `==?`, `!=?`, `::`, `->`, `'`), case-sensitive keywords
- `ast` — ~75 AST node types with `Span` on every node, serde derives, `Error` variants. New SV types: `SvPortType` with `InterfacePort` variant, `VarType` (Logic/Bit/Byte/Shortint/Int/Longint), `TypeSpec` (named/scoped types), `EnumDecl`/`StructDecl`/`TypedefDecl`, `AlwaysCombBlock`/`AlwaysFfBlock` (with sensitivity list)/`AlwaysLatchBlock`, `SvInterfaceDecl`/`SvModportDecl`/`SvModportPort`, `SvPackageDecl`/`SvImport`, `SvAssertion`, `CompoundOp`, `CaseModifier` (Unique/Priority). All statement/expression variants from Verilog plus compound assignments, incr/decr, return/break/continue, scoped identifiers, wildcard equality
- `parser` — `SvParser` struct with primitives, error recovery, top-level rules (source file, module, interface, package), ANSI/non-ANSI port detection including interface ports (`axi_if.master bus`), parameter port lists with type parameters, end labels (`endmodule : name`)
- `expr` — Pratt expression parser with IEEE 1800-2017 precedence (13 levels + ternary + SV ops), `inside` at relational level, `==?`/`!=?` at equality level, prefix/postfix `++`/`--`, scoped names (`pkg::name`), same `<=` disambiguation and part-select restricted binding power as Verilog parser
- `stmt` — All Verilog statements + compound assignments (`+=` etc), `++`/`--` (prefix/postfix), `return`/`break`/`continue`, `do...while`, `unique if`/`priority case`, `for (int i = 0; ...)` with local variable declarations, immediate assertions (`assert`/`assume`/`cover`), local variable declarations in procedural blocks
- `decl` — All Verilog declarations + `logic`/`bit`/`byte`/`int`/`longint` variable declarations, `typedef` (logic, enum, struct packed), `enum` type with member values, `struct packed` with field declarations, `import pkg::*` / `import pkg::name`, `modport` declarations, `always_comb`/`always_ff` (extracts sensitivity list from `@(...)`)/`always_latch`, functions with return types and ANSI ports, tasks with `automatic`. Named-type variable disambiguation (`state_t state;` vs `mod_name inst(...)`) via 3rd-token peek. Scoped-type variable support (`pkg::type_t var;`)
- `lib` — Public API `parse_file()` + 16 integration tests

**Key design decisions:**
- Standalone crate — no code sharing with Verilog parser (follows VHDL/Verilog precedent)
- `always_ff @(posedge clk)` extracts sensitivity list into `AlwaysFfBlock.sensitivity` field rather than wrapping body in `EventControl`
- Named-type vs instantiation disambiguation: `ident ident` pattern checked by peeking 3rd token — `(` means instantiation, otherwise named-type variable declaration
- Interface port parsing: `ident.modport name` pattern detected in ANSI port list, creates `SvPortType::InterfacePort`
- Function/task ANSI port parsing: single name per port call with comma-lookahead to detect next port declaration (direction/type keyword)
- Same `<=` disambiguation and part-select restricted binding power as Verilog parser

**Tests added:** 166 tests
- 10 token tests (keyword lookup, SV-specific keywords, predicates for direction/net_type/data_type/always_variant/assignment_op)
- 30 lexer tests (SV operators, compound assignments, increment/decrement, scope resolution, wildcard equality, arrow, tick, SV keywords by category, all Verilog lexer tests)
- 10 AST serde roundtrip tests (module, source file, expr, statement, binary op, compound op, enum, range, import, span accessor)
- 15 parser tests (minimal module, ports, parameters, interfaces, packages, end labels, error recovery, direction inheritance, signed ports, multiple modules)
- 26 expression tests (identifiers, literals, binary ops, precedence, unary, ternary, concat, repeat, index, range/part-select, function/system calls, hierarchical names, scoped identifiers, prefix/postfix increment/decrement, wildcard equality/inequality, power associativity)
- 20 statement tests (blocking/non-blocking, if/else, case, for with int, while, forever, do-while, event control, compound assignments, return, break/continue, unique if, priority case, incr/decr, local var decl, null, system task)
- 39 declaration tests (wire, reg, integer, real, logic/bit/int, parameter, localparam, assign, all always variants, initial, typedef logic/enum/struct, import wildcard/named, instantiation named/positional, gate, genvar, generate for/if, function with return type/verilog style/end label, task with ANSI ports/end label, non-ANSI port decl, assertion, var with init, multiple var names, enum variable)
- 16 integration tests (counter, mux, FSM with enum, package+import, interface+modport, struct packed, for loop with int, always_latch, end labels, compound assignments, named import, non-ANSI ports, error recovery, serde roundtrip, generate+always_ff, function with return)

**Test results:** 566 passed, 0 failed (400 previous + 166 new)
**Clippy:** Clean (zero warnings with -D warnings)
**Docs:** Clean (zero warnings from `cargo doc`)
**Next:** Implement `aion_elaborate` (AST → AionIR elaboration)

---

#### 2026-02-07 — aion_verilog_parser full Verilog-2005 parser

**Crate:** `aion_verilog_parser`

**What:** Implemented a complete hand-rolled recursive descent Verilog-2005 parser across 7 modules:
- `token` — `VerilogToken` enum (~55 keywords + ~30 operators + literals + identifiers), `Token` struct, `lookup_keyword()` function (case-sensitive), `is_keyword()`, `is_direction()`, `is_net_type()` predicates
- `lexer` — Full lexer with case-sensitive keyword matching, sized/based literals (`4'b1010`, `16'hFF`, `8'sb10101010`), unsized based literals (`'b1`, `'hFF`), real literals, `//` line comments, `/* */` block comments (non-nesting), escaped identifiers (`\my+signal `), system identifiers (`$display`), compiler directives (backtick — skipped with diagnostic), C-style string escapes
- `ast` — ~45 AST node types with `Span` on every node, serde derives, `Error` variants for recovery in VerilogItem/ModuleItem/Statement/Expr. Covers: VerilogSourceFile, ModuleDecl (ANSI/non-ANSI ports), PortDecl, Direction, NetType, ParameterDecl, all module items (NetDecl, RegDecl, IntegerDecl, RealDecl, ContinuousAssign, AlwaysBlock, InitialBlock, Instantiation, GateInst, GenerateBlock, GenvarDecl, FunctionDecl, TaskDecl, DefparamDecl), all statements (Blocking, NonBlocking, Block, If, Case, For, While, Forever, Repeat, Wait, EventControl, Delay, TaskCall, SystemTaskCall, Disable, Null), full expression tree (Identifier, HierarchicalName, Literal, RealLiteral, StringLiteral, Index, RangeSelect, PartSelect, Concat, Repeat, Unary, Binary, Ternary, FuncCall, SystemCall, Paren), UnaryOp (10 variants incl. reduction), BinaryOp (23 variants)
- `parser` — `VerilogParser` struct with primitives (advance/eat/expect/expect_ident/peek_is/peek_kind), error recovery (recover_to_semicolon), top-level rules (source file, module, ANSI/non-ANSI port detection, parameter port list)
- `expr` — Pratt expression parser with 13 Verilog precedence levels (IEEE 1364-2005 Table 5-4), right-associative `**` and `?:`, concatenation `{a,b}` vs replication `{3{a}}` detection, postfix index/range/part-select (`[i]`, `[m:l]`, `[i+:w]`, `[i-:w]`), hierarchical names, function/system calls
- `stmt` — All statement types: blocking/non-blocking assignments with `<=` disambiguation (LHS parsed as name expression to avoid Pratt consuming `<=` as comparison), begin/end blocks with labels and declarations, if/else, case/casex/casez, for/while/forever/repeat, wait, event control (`@(posedge clk or negedge rst)`, `@(*)`, `@*`), delay control, system task calls, disable
- `decl` — All module items: net/reg/integer/real declarations with ranges and array dimensions, parameter/localparam, non-ANSI port declarations, continuous assign, always/initial blocks, module instantiation (named + positional ports, parameter overrides, multiple instances), gate primitives, generate for/if with begin/end labels, genvar, defparam, function/task declarations
- `lib` — Public API `parse_file()` wiring lexer → parser

**Key design decisions:**
- Case-sensitive keywords (unlike VHDL which is case-insensitive)
- `<=` disambiguation: statement parser uses `parse_name_or_lvalue()` to parse LHS without entering Pratt parser, then checks for `=` (blocking) or `<=` (non-blocking). In expression context (inside `if()` conditions), `<=` is the comparison operator handled by Pratt parser.
- Sized literals (`4'b1010`) handled entirely in lexer — detect `'` after digits, consume base letter + base-specific digits (including x/z/?)
- ANSI vs non-ANSI port detection by peeking for direction keyword after `(`
- Instantiation detection: peek for second identifier or `#` after first ident at module-item level
- Concatenation `{a,b}` vs replication `{3{a}}`: check for inner `{` after first expr in braces
- Part-select disambiguation (`[i+:4]` vs `[a+b:0]`): parse first expression with restricted binding power (bp=18) to stop before `+`/`-`, then check for `+:` or `-:` pattern
- Compiler directives (backtick) emit "not yet supported" diagnostic and skip to end of line

**Tests added:** 127 tests
- 6 token tests (keyword lookup case-sensitivity, all keywords, non-keywords, predicates)
- 30 lexer tests (empty input, whitespace, keywords, identifiers, escaped/system identifiers, all literal types, sized literals with x/z, operators, comments, compiler directives, error cases, spans)
- 8 AST serde roundtrip tests (expr, module, source file, statement, binary op, case arm, range, span accessor)
- 11 parser tests (minimal module, empty ports, ANSI/non-ANSI ports, parameters, direction inheritance, error recovery, body items, signed ports, multiple modules)
- 22 expression tests (identifiers, literals, binary ops, precedence, associativity, unary, reduction, ternary, concat, repeat, index, range/part-select, function/system calls, hierarchical names, strings, complex expressions)
- 20 statement tests (blocking/non-blocking, if/else, case/casex, for/while/forever/repeat, wait, event control posedge/multiple/star/@*, delay, system task, disable, labeled blocks, null)
- 17 declaration tests (wire, reg, integer, real, parameter, localparam, assign, always, initial, instantiation named/positional, gate, genvar, function, task, non-ANSI ports, generate for/if)
- 13 integration tests (counter, mux, shift register, ALU, RAM, FSM, generate, testbench, instantiation chain, multi-module, error recovery, serde roundtrip)

**Test results:** 400 passed, 0 failed (273 previous + 127 new)
**Clippy:** Clean (zero warnings with -D warnings)
**Next:** Implement `aion_sv_parser` (SystemVerilog-2017 parser)

---

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
