# Aion — FPGA Toolchain

## Project Overview

Aion is an open-source, Rust-based HDL compiler toolchain that replaces Quartus/Vivado with a fast, unified, Cargo-inspired workflow. It provides a complete pipeline from parsing and linting through synthesis, place-and-route, and device programming.

- **Language:** Rust (2021 edition)
- **License:** MIT/Apache 2.0 dual-license
- **HDL Support:** VHDL-2008, Verilog-2005, SystemVerilog-2017

## Key Commands

```bash
cargo build                    # Build all crates
cargo test                     # Run all unit + integration tests
cargo test -p aion_<crate>     # Run tests for a specific crate
cargo clippy --all-targets -- -D warnings   # Lint (must pass with zero warnings)
cargo fmt --check              # Check formatting
cargo fmt                      # Auto-format
cargo doc --no-deps            # Build documentation
```

## Workspace Structure

This is a Cargo workspace. All crates live under `crates/`:

```
crates/
├── aion_cli/            # Binary — CLI entry point (clap)
├── aion_common/         # Shared types: Ident, ContentHash, Logic, LogicVec
├── aion_config/         # aion.toml parsing → ProjectConfig
├── aion_source/         # FileId, Span, SourceDb — source file management
├── aion_diagnostics/    # Diagnostic, DiagnosticSink, renderers (terminal/JSON/SARIF)
├── aion_ir/             # AionIR: Design, Module, Signal, Cell, Process, TypeDb
├── aion_vhdl_parser/    # Hand-rolled recursive descent VHDL-2008 parser
├── aion_verilog_parser/ # Hand-rolled recursive descent Verilog-2005 parser
├── aion_sv_parser/      # Hand-rolled recursive descent SystemVerilog-2017 parser
├── aion_elaborate/      # AST → AionIR elaboration engine
├── aion_synth/          # Synthesis: behavioral lowering, optimization, tech mapping
├── aion_pnr/            # Place & route: simulated annealing + PathFinder router
├── aion_timing/         # Static timing analysis, SDC/XDC parsing
├── aion_bitstream/      # Bitstream generation (SOF/POF/RBF, BIT)
├── aion_arch/           # Device architecture models and databases
├── aion_sim/            # Event-driven HDL simulator
├── aion_lint/           # Lint rules and engine
├── aion_lsp/            # Language Server Protocol implementation
├── aion_flash/          # JTAG programming and device detection
├── aion_deps/           # Dependency resolution, fetching, lock file
├── aion_cache/          # Incremental compilation cache management
└── aion_report/         # Report generation (text, JSON, SARIF, SVG)
```

## Critical Rules — ALWAYS Follow

### 1. Every Change MUST Have Tests

- **Never** submit code without corresponding unit tests.
- Write tests in the same file using `#[cfg(test)] mod tests { ... }` for unit tests.
- Place integration tests in `tests/integration/`.
- Test both the happy path AND error/edge cases.
- For parsers: test valid input produces correct AST, and invalid input triggers error recovery with diagnostics.
- For IR transformations: test round-trip serialization via `bincode`.
- For lint rules: test that the rule fires on bad code and stays silent on good code.
- Minimum: 2+ test cases per public function (1 success, 1 error/edge case).

### 2. All Tests MUST Pass Before You Stop

- Run `cargo test -p aion_<crate>` for the crate you changed.
- Run `cargo test` for the full workspace if changes span multiple crates.
- **Do not consider a task complete if any test fails.** Fix failures before finishing.

### 3. Clippy Must Pass with Zero Warnings

- Run `cargo clippy --all-targets -- -D warnings` before completing any task.
- Fix all warnings. Do not use `#[allow(...)]` unless there is a documented reason.

### 4. Code Must Be Formatted

- Run `cargo fmt` after making changes.
- Never commit unformatted code.

### 5. All Public Items MUST Have Doc Comments

- Every `pub struct`, `pub enum`, `pub fn`, `pub trait`, `pub type`, `pub const` needs a `///` doc comment.
- Every `pub enum` variant needs its own `///` doc.
- Every `lib.rs` needs a `//!` crate-level doc at the top.
- Docs must be **specific and useful** — not just restating the type name.
  - Bad: `/// A module.`
  - Good: `/// A single hardware module in the design, containing ports, signals, cells, and behavioral processes.`
- For functions: describe what it does, not just what it returns.
- Use `/check-docs <crate>` to run the doc-checker subagent for a thorough audit.

### 6. Update PROGRESS.md After Every Task

- After completing any task, update `PROGRESS.md` with:
  - What was implemented
  - What tests were added
  - Current status of the phase
  - Any blockers or decisions made
- Keep entries concise but informative.

## Architecture Principles

1. **Correctness first.** Wrong bitstreams damage hardware. Reject ambiguity.
2. **Strict error recovery.** Never panic on user input. Report maximum diagnostics per run.
3. **Serialized stage boundaries.** Each pipeline stage reads/writes disk. Enables caching and debugging.
4. **Speed through parallelism and incrementality.** Module-level parallelism everywhere. Fine-grained dependency tracking.
5. **Unified IR.** AionIR is the lingua franca after elaboration. No language-specific types downstream.
6. **Readable errors.** Every diagnostic: precise span, error code, actionable suggestion.
7. **Cargo-like UX.** CLI and project structure follow Cargo conventions.

## Code Style

- Use `thiserror` for error types. Pattern: `AionResult<T> = Result<T, InternalError>`. User errors go through `DiagnosticSink`, not `Result::Err`.
- Every crate's `lib.rs` must start with `#![warn(missing_docs)]` — this makes the compiler emit warnings for undocumented public items. Since the Stop hook runs `cargo clippy -- -D warnings`, missing docs will block task completion automatically.
- Use `serde` with `Serialize`/`Deserialize` derives on all IR types for `bincode` serialization.
- Use opaque ID newtypes: `pub struct ModuleId(u32)`, `pub struct SignalId(u32)`, etc.
- Use `Arena<Id, T>` pattern for indexed storage of IR entities.
- String interning via `Ident(u32)` backed by `lasso::ThreadedRodeo`.
- `Span { file: FileId, start: u32, end: u32 }` on every AST/IR node.
- Parser functions return `Option<Node>` — `None` means error-recovery happened.
- Pratt parsing for all expression grammars.
- Thread safety: `DiagnosticSink` uses `Mutex<Vec<Diagnostic>>` + `AtomicUsize`.
- Parallelism: `rayon` for CPU-bound work, `tokio` only for I/O (LSP, flash, dep fetching).

## Crate Dependency Rules

- Parsers depend on: `aion_source`, `aion_diagnostics`
- `aion_elaborate` depends on: `aion_ir`, all parsers
- `aion_synth` depends on: `aion_ir`, `aion_arch`
- `aion_pnr` depends on: `aion_ir`, `aion_arch`, `aion_timing`
- `aion_lint` depends on: `aion_ir`, `aion_arch`, `aion_diagnostics`
- **No circular dependencies.** `aion_common` and `aion_source` are leaf crates.

## Key External Crates

| Crate | Version | Purpose |
|-------|---------|---------|
| `clap` | 4.x | CLI (derive API) |
| `serde` | 1.x | Serialization |
| `bincode` | 2.x | Binary serialization for stage artifacts |
| `tokio` | 1.x | Async runtime (I/O only) |
| `rayon` | 1.x | Data parallelism (CPU work) |
| `tower-lsp` | 0.20+ | LSP server |
| `petgraph` | 0.6+ | Graph structures |
| `lasso` | 0.7+ | String interning |
| `xxhash-rust` | 0.8+ | Fast hashing (XXH3) |
| `toml` | 0.8+ | TOML parsing |
| `thiserror` | 2.x | Error derives |

## Reference Documents

- `docs/aion-prd.md` — Product requirements
- `docs/aion-technical-spec.md` — Full technical specification (data structures, algorithms, interfaces)

**Always consult the technical spec before implementing a crate.** It contains exact Rust type signatures, algorithm descriptions, and interface contracts.

## Current Phase

Check `PROGRESS.md` for the current implementation phase and what to work on next.
