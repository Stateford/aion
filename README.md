# Aion

A fast, open-source FPGA toolchain written in Rust. Aion replaces proprietary vendor tools (Quartus, Vivado) with a unified, Cargo-inspired workflow — from HDL parsing and linting through synthesis, place-and-route, and device programming.

## Features

- **Multi-language HDL support** — VHDL-2008, Verilog-2005, SystemVerilog-2017
- **Unified intermediate representation** — All languages elaborate into a single AionIR, enabling cross-language instantiation and shared analysis
- **15 built-in lint rules** — Warnings (unused signals, width mismatches, missing resets), errors (multiple drivers, port mismatches), and conventions (naming, magic numbers)
- **Event-driven simulator** — Delta-cycle-accurate execution with 4-state logic (0/1/X/Z), multi-driver resolution, and VCD waveform output
- **Incremental compilation** — Content-hash caching skips unchanged work across rebuilds
- **Cargo-like UX** — `aion init`, `aion lint`, project scaffolding, `aion.toml` configuration
- **Precise diagnostics** — Source-span-accurate error messages with codes and suggestions, inspired by `rustc`

## Quick Start

```bash
# Build from source
git clone https://github.com/Stateford/aion.git
cd aion
cargo build --release

# Create a new project
aion init my_design --lang systemverilog
cd my_design

# Run static analysis
aion lint
```

## CLI Usage

### `aion init [NAME]`

Scaffolds a new HDL project with directory structure, `aion.toml`, and template source files.

```bash
aion init my_chip                            # SystemVerilog (default)
aion init my_chip --lang vhdl                # VHDL template
aion init my_chip --target xc7a35t           # With target device
```

### `aion lint`

Runs the full parse-elaborate-lint pipeline on the current project.

```bash
aion lint                                    # Default: all rules, text output
aion lint --deny naming-violation            # Promote rule to error
aion lint --allow unused-signal              # Suppress a rule
aion lint --format json                      # Machine-readable output
```

### Global Flags

| Flag | Description |
|------|-------------|
| `--quiet` | Suppress all output except errors |
| `--verbose` | Enable debug-level output |
| `--color {auto\|always\|never}` | Control colored output |
| `--config <PATH>` | Path to custom `aion.toml` |

## Project Configuration

Projects are configured via `aion.toml`:

```toml
[project]
name = "my_design"
version = "0.1.0"
top_module = "top"
languages = ["systemverilog"]

[targets.default]
device = "xc7a35t"
package = "cpg236"
speed_grade = "-1"

[lint]
deny = ["multiple-drivers", "port-mismatch"]
allow = ["magic-number"]
```

## Architecture

Aion is structured as a Cargo workspace with modular crates, each handling one pipeline stage:

```
Source Files (.v, .sv, .vhd)
        |
   [ Parsing ]          aion_vhdl_parser, aion_verilog_parser, aion_sv_parser
        |
   [ Elaboration ]      aion_elaborate
        |
   [ AionIR ]           aion_ir  (unified intermediate representation)
       / \
      /   \
[ Lint ]  [ Simulate ]  aion_lint, aion_sim
```

### Crate Map

| Crate | Description |
|-------|-------------|
| `aion_common` | Shared types: `Ident`, `Logic`, `LogicVec`, `ContentHash` |
| `aion_source` | Source file management: `FileId`, `Span`, `SourceDb` |
| `aion_diagnostics` | Diagnostic reporting: `Diagnostic`, `DiagnosticSink`, terminal renderer |
| `aion_config` | `aion.toml` parsing and validation |
| `aion_ir` | Core IR: `Design`, `Module`, `Signal`, `Cell`, `Process`, `TypeDb` |
| `aion_vhdl_parser` | Hand-rolled recursive descent VHDL-2008 parser |
| `aion_verilog_parser` | Hand-rolled recursive descent Verilog-2005 parser |
| `aion_sv_parser` | Hand-rolled recursive descent SystemVerilog-2017 parser |
| `aion_elaborate` | AST-to-IR elaboration with cross-language support |
| `aion_lint` | Lint engine with 15 configurable rules |
| `aion_cache` | Content-hash-based incremental compilation cache |
| `aion_cli` | CLI entry point (`init`, `lint` commands) |
| `aion_sim` | Event-driven HDL simulator with VCD output |
| `aion_conformance` | Integration/conformance tests: full pipeline (parse→elaborate→lint) for all 3 languages |

### Design Principles

1. **Correctness first** — Wrong bitstreams damage hardware. Reject ambiguity.
2. **Strict error recovery** — Never panic on user input. Report maximum diagnostics per run.
3. **Serialized stage boundaries** — Each pipeline stage reads/writes disk, enabling caching and debugging.
4. **Speed through parallelism** — Module-level parallelism everywhere, fine-grained dependency tracking.
5. **Unified IR** — AionIR is the lingua franca after elaboration. No language-specific types downstream.
6. **Readable errors** — Every diagnostic has a precise span, error code, and actionable suggestion.
7. **Cargo-like UX** — CLI and project structure follow Cargo conventions.

## Lint Rules

| Code | Name | Severity | Description |
|------|------|----------|-------------|
| W101 | `unused-signal` | Warning | Signal declared but never read |
| W102 | `undriven-signal` | Warning | Signal never assigned or driven |
| W103 | `width-mismatch` | Warning | LHS and RHS of assignment have different bit widths |
| W104 | `missing-reset` | Warning | Sequential process has no reset |
| W105 | `incomplete-sensitivity` | Warning | Combinational process missing signals in sensitivity list |
| W106 | `latch-inferred` | Warning | Combinational `if` without `else` or `case` without `default` |
| W107 | `truncation` | Warning | RHS wider than LHS causing bit truncation |
| W108 | `dead-logic` | Warning | Code after `$finish`, always-true/false conditions |
| E102 | `non-synthesizable` | Error | Initial blocks, `$display`/`$finish` in non-initial processes |
| E104 | `multiple-drivers` | Error | Wire signal driven by more than one source |
| E105 | `port-mismatch` | Error | Instance connections don't match module ports |
| C201 | `naming-violation` | Convention | Naming style checks (snake_case, UPPER_CASE, etc.) |
| C202 | `missing-doc` | Convention | Module documentation check |
| C203 | `magic-number` | Convention | Literal values used directly in expressions |
| C204 | `inconsistent-style` | Convention | Process kind style inconsistencies |

## Building & Testing

```bash
cargo build                                  # Build all crates
cargo test                                   # Run all 1058 tests
cargo test -p aion_sim                       # Run tests for a specific crate
cargo clippy --all-targets -- -D warnings    # Lint (zero warnings enforced)
cargo fmt --check                            # Check formatting
cargo doc --no-deps                          # Build documentation
```

### Conformance Tests

The `aion_conformance` crate runs 67 integration tests that exercise the full parse → elaborate → lint pipeline on realistic HDL designs across all three languages.

```bash
cargo test -p aion_conformance                              # Run all 67 conformance tests
cargo test -p aion_conformance --test verilog_conformance   # Verilog-2005 designs (15 tests)
cargo test -p aion_conformance --test sv_conformance        # SystemVerilog-2017 designs (15 tests)
cargo test -p aion_conformance --test vhdl_conformance      # VHDL-2008 designs (12 tests)
cargo test -p aion_conformance --test error_recovery        # Error recovery & graceful degradation (10 tests)
cargo test -p aion_conformance --test lint_detection        # Lint rule detection through full pipeline (10 tests)
```

Test categories:
- **Language conformance** — Counters, FSMs, ALUs, RAMs, shift registers, module hierarchies, generate blocks, gate primitives, functions, packages, structs
- **Error recovery** — Malformed input handling, multi-error reporting, bad-then-good module recovery, empty source safety
- **Lint detection** — Unused signals (W101), latch inference (W106), initial blocks (E102), deny/allow configuration

## Roadmap

| Phase | Focus | Status |
|-------|-------|--------|
| **Phase 0** | Foundation — Parsing, elaboration, linting, caching, CLI | Complete |
| **Phase 1** | Simulation — Event-driven simulator, VCD/FST, CLI integration | In Progress |
| **Phase 2** | Synthesis — Behavioral lowering, optimization, technology mapping | Planned |
| **Phase 3** | Place & Route — Simulated annealing, PathFinder routing, timing | Planned |
| **Phase 4** | Polish — LSP, device programming, dependency management | Planned |

See [PROGRESS.md](PROGRESS.md) for detailed implementation status.

## Target Devices

**Intel/Altera:** Cyclone V, Cyclone 10 LP, MAX 10, Stratix V

**Xilinx/AMD:** Artix-7, Kintex-7, Spartan-7, Zynq-7000

## Documentation

- [Product Requirements](docs/aion-prd.md) — Vision, scope, and target audience
- [Technical Specification](docs/aion-technical-spec.md) — Data structures, algorithms, and interface contracts

## License

Dual-licensed under [MIT](https://opensource.org/licenses/MIT) and [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0). Choose whichever you prefer.
