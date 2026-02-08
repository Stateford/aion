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
- [x] `aion_verilog_parser` — full grammar coverage
- [x] `aion_sv_parser` — synthesizable subset
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
