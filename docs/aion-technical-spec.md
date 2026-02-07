# Aion — Technical Specification

**Version:** 1.0 Draft  
**Date:** February 7, 2026  
**Status:** Living Document  
**Audience:** Core contributors and external open-source contributors  

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Design Principles](#2-design-principles)
3. [Architectural Overview](#3-architectural-overview)
4. [Cargo Workspace & Crate Structure](#4-cargo-workspace--crate-structure)
5. [Core Infrastructure](#5-core-infrastructure)
6. [Parser Design](#6-parser-design)
7. [Intermediate Representation — AionIR](#7-intermediate-representation--aionir)
8. [Elaboration Engine](#8-elaboration-engine)
9. [Synthesis Engine](#9-synthesis-engine)
10. [Place & Route](#10-place--route)
11. [Timing Analysis](#11-timing-analysis)
12. [Bitstream Generation](#12-bitstream-generation)
13. [Simulator](#13-simulator)
14. [Lint Engine](#14-lint-engine)
15. [LSP Server](#15-lsp-server)
16. [Device Programming](#16-device-programming)
17. [Incremental Compilation & Caching](#17-incremental-compilation--caching)
18. [Dependency Management](#18-dependency-management)
19. [Serialization & Stage Boundaries](#19-serialization--stage-boundaries)
20. [CLI Architecture](#20-cli-architecture)
21. [Error Reporting](#21-error-reporting)
22. [Parallelism Model](#22-parallelism-model)
23. [Testing Strategy](#23-testing-strategy)
24. [Performance Budget](#24-performance-budget)
25. [Phased Implementation Guide](#25-phased-implementation-guide)

---

## 1. Introduction

### 1.1 Purpose

This document is the authoritative technical specification for Aion, an open-source FPGA toolchain written in Rust. It translates the product requirements (see `aion-prd.md`) into concrete architectural decisions, data structures, algorithms, crate boundaries, and interface contracts that guide implementation.

Every contributor — whether on the core team or joining from the community — should be able to read this document and understand *how* Aion is built, *why* key decisions were made, and *where* to contribute.

### 1.2 Scope

This spec covers the complete Aion toolchain from source HDL input to programmed FPGA device:

- Parsing of VHDL-2008, Verilog-2005, and SystemVerilog-2017
- Elaboration and type checking
- Logic synthesis and technology mapping
- Place and route for Intel (Cyclone V, Cyclone 10 LP, MAX 10, Stratix V) and Xilinx (Artix-7, Kintex-7, Spartan-7, Zynq-7000) targets
- Bitstream generation (SOF/POF/RBF, BIT)
- Event-driven simulation
- Static analysis / linting
- LSP integration
- JTAG device programming
- Dependency management and incremental compilation
- CLI interface and reporting

### 1.3 Notational Conventions

- Rust type signatures are written in standard Rust syntax
- `aion_*` prefixes denote crate names within the workspace
- `AionIR` refers to the unified intermediate representation
- "Stage" refers to a discrete pipeline step with serialized inputs and outputs
- "Module" without qualification refers to an HDL module/entity, not a Rust module

---

## 2. Design Principles

These principles govern all architectural decisions in Aion. When trade-offs arise, higher-numbered principles yield to lower-numbered ones.

1. **Correctness first.** A wrong bitstream can damage hardware. Every stage must be verifiable, and the default behavior is to reject ambiguity rather than guess.

2. **Strict error recovery.** Aion never crashes on user input. Every pipeline stage must produce diagnostics and degrade gracefully, reporting as many independent errors as possible in a single run (modeled after `rustc`).

3. **Serialized stage boundaries.** Each major pipeline stage reads its input from disk and writes its output to disk. This enables caching, reproducibility, parallel development of stages, and debugging of intermediate artifacts.

4. **Speed through parallelism and incrementality.** Aion exploits module-level parallelism at every stage and tracks fine-grained dependencies to minimize recompilation. Single-threaded performance matters, but the architecture must never preclude parallelism.

5. **Unified IR.** A single intermediate representation serves as the lingua franca between all pipeline stages after elaboration. Language-specific ASTs exist only in the parser; everything downstream operates on AionIR.

6. **Readable errors.** Every diagnostic must include a precise source span, an error code, and an actionable suggestion. Errors are a product feature, not an afterthought.

7. **Cargo-like UX.** The CLI, project structure, and dependency management follow Cargo conventions wherever applicable. An experienced Rust developer should feel immediately at home.

---

## 3. Architectural Overview

### 3.1 High-Level Pipeline

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          aion CLI (aion_cli)                            │
│  init │ build │ test │ sim │ lint │ flash │ update │ clean              │
└───┬─────┬───────┬──────┬─────┬──────┬────────────────────────────────────┘
    │     │       │      │     │      │
    │     ▼       │      │     ▼      │
    │  ┌──────┐   │      │  ┌──────┐  │
    │  │Parse │   │      │  │ Lint │  │
    │  │Stage │   │      │  │Engine│  │
    │  └──┬───┘   │      │  └──────┘  │
    │     │       │      │            │
    │     │ *.ast files (serialized)  │
    │     ▼       │      │            │
    │  ┌──────────┐      │            │
    │  │Elaborate │      │            │
    │  │  Stage   │      │            │
    │  └──┬───────┘      │            │
    │     │              │            │
    │     │ *.air files (AionIR, serialized)
    │     ▼              │            │
    │  ┌──────────┐      │            │
    │  │Synthesize│      │            │
    │  │  Stage   │◄─────┤            │
    │  └──┬───────┘      │            │
    │     │              │            │
    │     │ *.netlist files (serialized)
    │     ▼              │            │
    │  ┌──────────┐   ┌──┴──────┐    │
    │  │Place &   │   │Simulate │    │
    │  │Route     │   │  Stage  │    │
    │  └──┬───────┘   └─────────┘    │
    │     │                          │
    │     │ *.placed files (serialized)
    │     ▼                          │
    │  ┌──────────┐                  │
    │  │Bitstream │                  │
    │  │  Gen     │                  │
    │  └──┬───────┘                  │
    │     │                          │
    │     │ .sof/.bit files          │
    │     ▼                          ▼
    │  ┌──────────┐           ┌──────────┐
    │  │ Reports  │           │  Flash   │
    │  └──────────┘           └──────────┘
```

### 3.2 Serialized Stage Boundaries

Each pipeline stage is an independent transformation:

| Stage | Input (on disk) | Output (on disk) | Format |
|-------|-----------------|-------------------|--------|
| Parse | `.vhd`, `.v`, `.sv` source files | `*.ast` — per-file AST | `bincode`-serialized Rust structs |
| Elaborate | `*.ast` files + `aion.toml` | `*.air` — AionIR modules | `bincode`-serialized AionIR |
| Synthesize | `*.air` files + device model | `*.netlist` — mapped netlist | `bincode`-serialized netlist |
| Place & Route | `*.netlist` + arch DB + constraints | `*.placed` — placed/routed design | `bincode`-serialized placement |
| Timing Analysis | `*.placed` + timing models | timing report (JSON/text) | JSON + human-readable text |
| Bitstream Gen | `*.placed` + arch DB | `.sof`/`.bit`/`.pof`/`.rbf` | Vendor-specific binary |
| Simulate | `*.ast` (or `*.air`) + testbench | waveforms + results | VCD/FST/GHW + JSON results |

All serialized intermediate files live under `out/.aion-cache/` and are keyed by content hashes for incremental compilation (see §17).

### 3.3 Key Technology Choices

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Language | Rust | Memory safety, fearless concurrency, excellent tooling ecosystem |
| Async runtime | Tokio | Used for I/O-heavy operations (flash, LSP server, dependency fetching). CPU-bound work uses `rayon` thread pools within Tokio tasks. |
| Serialization | `bincode` (via `serde`) | Fast binary serialization for inter-stage artifacts; compact on disk |
| CLI framework | `clap` (derive API) | Industry standard for Rust CLIs; matches Cargo conventions |
| LSP | `tower-lsp` | Mature, async-native LSP framework for Rust |
| Parallelism | `rayon` | Data-parallel work-stealing for CPU-bound pipeline stages |
| Hashing | `xxhash-rust` (XXH3) | Fast, non-cryptographic hashing for content-addressed caching |
| Graph library | `petgraph` | Mature, well-tested graph library for dependency and netlist graphs |

---

## 4. Cargo Workspace & Crate Structure

### 4.1 Workspace Layout

```
aion/
├── Cargo.toml              # Workspace root
├── Cargo.lock
├── LICENSE-MIT
├── LICENSE-APACHE
├── README.md
├── docs/
│   ├── aion-prd.md
│   ├── aion-technical-spec.md    # This document
│   └── architecture/             # Diagrams, ADRs
│
├── crates/
│   ├── aion_cli/                 # Binary crate — CLI entry point
│   ├── aion_common/              # Shared types, error infra, diagnostics
│   ├── aion_config/              # aion.toml parsing and project model
│   ├── aion_source/              # Source file management, spans, file IDs
│   ├── aion_diagnostics/         # Diagnostic types, rendering, SARIF/JSON output
│   │
│   ├── aion_vhdl_parser/         # VHDL-2008 parser → VHDL AST
│   ├── aion_verilog_parser/      # Verilog-2005 parser → Verilog AST
│   ├── aion_sv_parser/           # SystemVerilog-2017 parser → SV AST
│   │
│   ├── aion_ir/                  # AionIR type definitions and utilities
│   ├── aion_elaborate/           # AST → AionIR elaboration engine
│   ├── aion_synth/               # Synthesis: optimization + technology mapping
│   ├── aion_pnr/                 # Place and route engine
│   ├── aion_timing/              # Static timing analysis
│   ├── aion_bitstream/           # Bitstream generation (all vendors)
│   │
│   ├── aion_arch/                # Device architecture models and databases
│   │   ├── src/
│   │   │   ├── lib.rs            # Architecture trait definitions
│   │   │   ├── intel/            # Intel/Altera family models
│   │   │   └── xilinx/           # Xilinx/AMD family models
│   │   └── data/                 # Architecture database files (binary)
│   │
│   ├── aion_sim/                 # Event-driven simulator kernel
│   ├── aion_lint/                # Lint rules and lint engine
│   ├── aion_lsp/                 # LSP server implementation
│   ├── aion_flash/               # JTAG programming and device detection
│   ├── aion_deps/                # Dependency resolution, fetching, lock file
│   ├── aion_cache/               # Incremental compilation cache management
│   │
│   └── aion_report/              # Report generation (text, JSON, SARIF, SVG)
│
├── extensions/
│   └── vscode/                   # VS Code extension (aion-vscode)
│
└── tests/
    ├── integration/              # End-to-end integration tests
    ├── fixtures/                 # HDL test fixtures
    └── conformance/              # HDL standard conformance suites
```

### 4.2 Crate Dependency Graph

```
aion_cli
 ├── aion_config
 ├── aion_common
 ├── aion_source
 ├── aion_diagnostics
 ├── aion_vhdl_parser ──┐
 ├── aion_verilog_parser──┼── all depend on aion_source, aion_diagnostics
 ├── aion_sv_parser ────┘
 ├── aion_ir
 ├── aion_elaborate ─── depends on aion_ir, all parsers
 ├── aion_synth ─────── depends on aion_ir, aion_arch
 ├── aion_pnr ───────── depends on aion_ir, aion_arch, aion_timing
 ├── aion_timing ────── depends on aion_ir, aion_arch
 ├── aion_bitstream ─── depends on aion_arch, aion_pnr
 ├── aion_sim ───────── depends on aion_ir, all parsers
 ├── aion_lint ──────── depends on aion_ir, aion_arch, aion_diagnostics
 ├── aion_lsp ───────── depends on aion_ir, aion_lint, all parsers
 ├── aion_flash ─────── depends on aion_arch
 ├── aion_deps ──────── depends on aion_config
 ├── aion_cache ─────── depends on aion_common
 └── aion_report ────── depends on aion_ir, aion_timing, aion_pnr
```

### 4.3 Crate Responsibilities

#### `aion_common`

Shared foundational types used across the entire workspace.

```rust
// crates/aion_common/src/lib.rs

/// A unique identifier for any named entity in the design.
/// Interned strings for cheap cloning and comparison.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ident(u32); // Index into global interner

/// Global string interner (thread-safe).
pub struct Interner { /* lasso::ThreadedRodeo or similar */ }

/// Content hash for cache invalidation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash([u8; 16]); // XXH3-128

/// Frequency value with unit.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Frequency(f64); // Always stored in Hz

/// A 4-state logic value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Logic {
    Zero = 0,
    One  = 1,
    X    = 2, // Unknown
    Z    = 3, // High-impedance
}

/// A vector of 4-state logic values, packed for efficiency.
/// Uses 2 bits per value, stored in a BitVec.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogicVec {
    width: u32,
    /// Packed storage: 2 bits per logic value.
    /// Bit 0 = value, Bit 1 = mask (0=known, 1=X/Z).
    data: Vec<u64>,
}
```

#### `aion_source`

Source file management, span tracking, and source maps for diagnostics.

```rust
// crates/aion_source/src/lib.rs

/// Opaque ID for a source file loaded into the compilation session.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId(u32);

/// A byte offset range within a source file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub file: FileId,
    pub start: u32, // Byte offset from start of file
    pub end: u32,   // Byte offset (exclusive)
}

impl Span {
    pub fn merge(self, other: Span) -> Span { /* ... */ }
    pub const DUMMY: Span = Span { file: FileId(u32::MAX), start: 0, end: 0 };
}

/// The source database. Owns all loaded source text and resolves
/// FileId + byte offsets to line/column for diagnostics.
pub struct SourceDb {
    files: Vec<SourceFile>,
}

pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub content: String,
    /// Byte offsets of each line start, for fast line/column lookup.
    line_starts: Vec<u32>,
    pub content_hash: ContentHash,
}

impl SourceDb {
    pub fn load_file(&mut self, path: &Path) -> Result<FileId, io::Error>;
    pub fn get_file(&self, id: FileId) -> &SourceFile;
    pub fn resolve_span(&self, span: Span) -> ResolvedSpan;
    pub fn snippet(&self, span: Span) -> &str;
}

/// A span resolved to human-readable line/column coordinates.
pub struct ResolvedSpan {
    pub file_path: PathBuf,
    pub start_line: u32,   // 1-indexed
    pub start_col: u32,    // 1-indexed
    pub end_line: u32,
    pub end_col: u32,
}
```

#### `aion_diagnostics`

Diagnostic creation, severity management, and multi-format rendering.

```rust
// crates/aion_diagnostics/src/lib.rs

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Help,
    Note,
    Warning,
    Error,
}

/// A structured diagnostic message.
#[derive(Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,  // e.g., E102, W201, T305
    pub message: String,
    pub primary_span: Span,
    pub labels: Vec<Label>,     // Additional annotated spans
    pub notes: Vec<String>,     // Explanatory footnotes
    pub help: Vec<String>,      // Actionable suggestions
    pub fix: Option<SuggestedFix>, // Auto-applicable fix
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Label {
    pub span: Span,
    pub message: String,
    pub style: LabelStyle, // Primary, Secondary
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SuggestedFix {
    pub message: String,
    pub replacements: Vec<Replacement>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Replacement {
    pub span: Span,
    pub new_text: String,
}

/// Diagnostic code registry.
/// Each code maps to a category and a stable identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DiagnosticCode {
    pub category: Category,
    pub number: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    Error,      // Exxx
    Warning,    // Wxxx
    Convention, // Cxxx
    Timing,     // Txxx
    Vendor,     // Sxxx
}

/// Rendering backend trait — implemented for terminal, JSON, SARIF.
pub trait DiagnosticRenderer {
    fn render(&self, diag: &Diagnostic, source_db: &SourceDb) -> String;
}

pub struct TerminalRenderer { pub color: ColorChoice, pub width: u16 }
pub struct JsonRenderer;
pub struct SarifRenderer;

/// Accumulates diagnostics during a compilation session.
/// Thread-safe for parallel pipeline stages.
pub struct DiagnosticSink {
    diagnostics: std::sync::Mutex<Vec<Diagnostic>>,
    error_count: std::sync::atomic::AtomicUsize,
}

impl DiagnosticSink {
    pub fn emit(&self, diag: Diagnostic);
    pub fn has_errors(&self) -> bool;
    pub fn error_count(&self) -> usize;
    pub fn take_all(&self) -> Vec<Diagnostic>;
}
```

#### `aion_config`

Parses and validates `aion.toml` into a strongly-typed project model.

```rust
// crates/aion_config/src/lib.rs

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    pub project: ProjectMeta,
    #[serde(default)]
    pub targets: BTreeMap<String, TargetConfig>,
    #[serde(default)]
    pub pins: BTreeMap<String, PinAssignment>,
    #[serde(default)]
    pub constraints: ConstraintConfig,
    #[serde(default)]
    pub clocks: BTreeMap<String, ClockDef>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub build: BuildConfig,
    #[serde(default)]
    pub test: TestConfig,
    #[serde(default)]
    pub lint: LintConfig,
}

#[derive(Debug, Deserialize)]
pub struct ProjectMeta {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub top: String,        // Path to top-level HDL file
    #[serde(default)]
    pub license: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TargetConfig {
    pub device: String,       // Full part number, e.g. "5CSEMA5F31C6"
    pub family: String,       // e.g. "cyclone5", "artix7"
    #[serde(default)]
    pub pins: BTreeMap<String, PinAssignment>,
    #[serde(default)]
    pub constraints: Option<ConstraintConfig>,
}

#[derive(Debug, Deserialize)]
pub struct PinAssignment {
    pub pin: String,
    pub io_standard: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ConstraintConfig {
    #[serde(default)]
    pub timing: Vec<String>,  // Paths to SDC/XDC files
}

#[derive(Debug, Deserialize)]
pub struct ClockDef {
    pub frequency: String,    // e.g. "50MHz" — parsed to Frequency
    pub port: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    Git { git: String, tag: Option<String>, branch: Option<String>, rev: Option<String> },
    Path { path: String },
    Registry { version: String }, // Future
}

#[derive(Debug, Default, Deserialize)]
pub struct BuildConfig {
    #[serde(default = "default_optimization")]
    pub optimization: OptLevel,
    pub target_frequency: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptLevel {
    Area,
    Speed,
    #[default]
    Balanced,
}

#[derive(Debug, Default, Deserialize)]
pub struct TestConfig {
    #[serde(default = "default_waveform_format")]
    pub waveform_format: WaveformFormat,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WaveformFormat {
    Vcd,
    #[default]
    Fst,
    Ghw,
}

#[derive(Debug, Default, Deserialize)]
pub struct LintConfig {
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub warn: Vec<String>,
    #[serde(default)]
    pub naming: Option<NamingConfig>,
}

#[derive(Debug, Deserialize)]
pub struct NamingConfig {
    pub module: Option<NamingConvention>,
    pub signal: Option<NamingConvention>,
    pub parameter: Option<NamingConvention>,
    pub constant: Option<NamingConvention>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamingConvention {
    SnakeCase,
    CamelCase,
    UpperSnakeCase,
    PascalCase,
}

/// Parse and validate aion.toml from a project directory.
pub fn load_config(project_dir: &Path) -> Result<ProjectConfig, ConfigError>;

/// Resolve a fully-merged target configuration (global + target-specific overrides).
pub fn resolve_target(
    config: &ProjectConfig,
    target_name: &str,
) -> Result<ResolvedTarget, ConfigError>;
```

---

## 5. Core Infrastructure

### 5.1 Error Recovery Philosophy

Aion follows `rustc`'s strict error recovery model: **the compiler never panics on user input and always attempts to produce the maximum number of independent diagnostics per compilation run.**

Error recovery is implemented at every pipeline stage:

**Parsers:** On encountering a syntax error, the parser records a diagnostic and attempts to recover by:
1. Synchronizing to the next statement/declaration boundary (semicolons, `end`, `endmodule`, closing braces)
2. Inserting a "poison" AST node (`AstNode::Error(Span)`) at the recovery point
3. Continuing to parse subsequent declarations

**Elaboration:** If a module instantiation references an undefined module, the elaborator:
1. Records an error diagnostic
2. Creates a "black box" placeholder with inferred port types from the instantiation site
3. Continues elaborating the rest of the design

**Synthesis:** Unsupported constructs produce an error diagnostic and the containing module is synthesized as a black box. Downstream stages operate on the partial netlist.

**Place & Route:** If synthesis produced errors, P&R is skipped for affected modules but may still run for clean portions (in incremental mode).

The `DiagnosticSink` (§4.3) is the central mechanism. Every stage receives a `&DiagnosticSink` and calls `sink.emit(diagnostic)`. After each stage, the orchestrator checks `sink.has_errors()` to decide whether to continue to the next stage.

### 5.2 Result Types

```rust
// crates/aion_common/src/result.rs

/// The standard result type for fallible operations that produce diagnostics.
/// Ok contains the result value (which may be partial/degraded).
/// Err indicates an unrecoverable internal error (bug), not a user error.
///
/// User errors are reported via DiagnosticSink and the operation still returns Ok
/// with a best-effort result.
pub type AionResult<T> = Result<T, InternalError>;

/// An internal compiler error — indicates a bug in Aion, not user input.
#[derive(Debug, thiserror::Error)]
#[error("internal compiler error: {message}")]
pub struct InternalError {
    pub message: String,
    pub backtrace: std::backtrace::Backtrace,
}
```

### 5.3 String Interning

All identifiers, module names, signal names, and file paths are interned into a global `Interner`. This provides:
- O(1) equality comparison (compare `u32` indices)
- O(1) cloning (copy a `u32`)
- Deduplication of identical strings across the compilation
- Safe concurrent access from parallel pipeline stages

```rust
// Implementation uses `lasso::ThreadedRodeo` under the hood.
pub static INTERNER: Lazy<Interner> = Lazy::new(Interner::new);
```

---

## 6. Parser Design

### 6.1 Strategy Evaluation

Aion must parse three complex HDL languages. The parser strategy has major implications for performance, error recovery quality, and maintainability.

#### Option A: Hand-Rolled Recursive Descent

**Pros:**
- Best error recovery — full control over synchronization points and error messages
- Best performance — no grammar interpretation overhead, can exploit language-specific shortcuts
- Used by `rustc`, Roslyn, TypeScript compiler — proven approach for production compilers
- Incremental reparsing is easier to implement (fine-grained parse tree updates)
- No external tooling dependencies

**Cons:**
- Highest initial development effort — three full parsers written by hand
- Grammar changes require manual propagation
- Harder for new contributors to understand parser structure without grammar reference

#### Option B: Parser Generator (LALRPOP / pest / tree-sitter)

**Pros:**
- Grammar is explicitly declared in a separate file — easier to audit against the standard
- Lower initial effort per language
- tree-sitter specifically provides incremental parsing out of the box
- Community-maintained grammars may exist (tree-sitter-verilog, etc.)

**Cons:**
- Error recovery is limited by the generator's capabilities — often produces poor diagnostics
- Performance overhead from grammar interpretation
- Less control over AST shape — often produces CST (concrete syntax tree) that needs post-processing
- Generator-specific learning curve for contributors
- Dependency on external tool correctness and maintenance

#### Option C: Hybrid Approach

**Pros:**
- Hand-rolled parser for the most complex language (SystemVerilog), generator for the simpler ones
- Balances effort vs. control

**Cons:**
- Inconsistent contributor experience across parsers
- Still need to understand both approaches

### 6.2 Recommendation: Hand-Rolled Recursive Descent

Aion uses hand-rolled recursive descent parsers for all three HDL languages.

**Rationale:**

1. **Error recovery is a core product feature.** The PRD's error reporting design (§12) requires precise source spans, multi-span labels, and actionable suggestions. This level of diagnostic quality requires fine-grained control over recovery behavior that parser generators cannot provide.

2. **SystemVerilog's grammar is context-sensitive.** SystemVerilog famously requires semantic feedback during parsing (e.g., whether an identifier is a type or a variable affects parsing). This is extremely difficult to express in a declarative grammar but straightforward in a recursive descent parser with a symbol table.

3. **Performance ceiling.** Hand-rolled parsers consistently outperform generated parsers. For Aion's target of sub-1-second parse+lint on any project, this matters.

4. **Precedent.** Every production-quality compiler in this space (Quartus, Vivado, Verilator, Slang) uses hand-rolled parsers. The `slang` SystemVerilog compiler demonstrates that a hand-rolled SV parser in C++ is feasible and produces excellent diagnostics. Aion's SV parser should study `slang`'s architecture.

5. **Unified contributor experience.** All three parsers share the same patterns and infrastructure (lexer utilities, token types, recovery helpers, AST node allocation), making it easier for contributors to work across languages.

### 6.3 Parser Architecture

Each language parser lives in its own crate but shares infrastructure via `aion_source` and `aion_diagnostics`.

```
aion_vhdl_parser/
├── src/
│   ├── lib.rs          # Public API: parse_file() -> VhdlAst
│   ├── lexer.rs        # VHDL lexer (token stream)
│   ├── token.rs        # VHDL token types
│   ├── parser.rs       # Top-level parser driver
│   ├── ast.rs          # VHDL AST node types
│   ├── expr.rs         # Expression parsing (Pratt parser)
│   ├── stmt.rs         # Statement parsing
│   ├── decl.rs         # Declaration parsing
│   ├── types.rs        # Type parsing
│   └── recovery.rs     # Error recovery utilities
```

#### 6.3.1 Lexer

Each language has its own lexer producing a language-specific token stream. Lexers are hand-written for maximum performance and to handle language-specific peculiarities (VHDL is case-insensitive; SystemVerilog has context-sensitive keywords).

```rust
// crates/aion_vhdl_parser/src/token.rs (representative)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VhdlToken {
    // Keywords
    Architecture,
    Begin,
    Component,
    Entity,
    End,
    Generate,
    Generic,
    If,
    Is,
    Library,
    Of,
    Port,
    Process,
    Signal,
    Use,
    // ... all VHDL-2008 keywords

    // Literals
    IntLiteral,
    RealLiteral,
    CharLiteral,
    StringLiteral,
    BitStringLiteral,

    // Operators and punctuation
    LeftParen,
    RightParen,
    Semicolon,
    Colon,
    ColonEquals,    // :=
    LessEquals,     // <=
    Arrow,          // =>
    DoubleStar,     // **
    // ...

    // Special
    Identifier,
    ExtendedIdentifier, // \escaped\
    Comment,
    Whitespace,
    Eof,
    Error, // Lexer error token
}

#[derive(Clone, Copy)]
pub struct Token {
    pub kind: VhdlToken,
    pub span: Span,
}
```

#### 6.3.2 Recursive Descent Parser

```rust
// crates/aion_vhdl_parser/src/parser.rs (representative patterns)

pub struct VhdlParser<'src> {
    tokens: Vec<Token>,
    pos: usize,
    source_db: &'src SourceDb,
    sink: &'src DiagnosticSink,
    /// For error recovery: tracks nesting depth of parentheses, etc.
    nesting: Vec<NestingKind>,
}

impl<'src> VhdlParser<'src> {
    pub fn parse_design_file(&mut self) -> VhdlDesignFile {
        let mut units = Vec::new();
        while !self.at_eof() {
            match self.parse_design_unit() {
                Some(unit) => units.push(unit),
                None => {
                    // Recovery: skip to next design unit boundary
                    self.recover_to_design_unit();
                }
            }
        }
        VhdlDesignFile { units }
    }

    fn parse_entity_declaration(&mut self) -> Option<EntityDecl> {
        let start = self.expect(VhdlToken::Entity)?;
        let name = self.expect_ident()?;
        self.expect(VhdlToken::Is)?;

        let generics = if self.at(VhdlToken::Generic) {
            self.parse_generic_clause()
        } else {
            None
        };

        let ports = if self.at(VhdlToken::Port) {
            self.parse_port_clause()
        } else {
            None
        };

        self.expect(VhdlToken::End)?;
        self.eat(VhdlToken::Entity); // Optional trailing "entity"
        self.eat_ident(); // Optional trailing name
        self.expect(VhdlToken::Semicolon)?;

        Some(EntityDecl {
            span: start.span.merge(self.prev_span()),
            name,
            generics,
            ports,
        })
    }

    /// Error recovery: skip tokens until we find a design unit boundary.
    fn recover_to_design_unit(&mut self) {
        self.sink.emit(Diagnostic {
            severity: Severity::Error,
            code: DiagnosticCode::syntax_error(),
            message: format!("expected design unit, found `{}`", self.current_text()),
            primary_span: self.current_span(),
            labels: vec![],
            notes: vec![],
            help: vec!["expected `entity`, `architecture`, `package`, or `configuration`".into()],
            fix: None,
        });
        // Skip to next `entity`, `architecture`, `package`, `configuration`, or EOF
        while !self.at_eof() && !self.at_design_unit_start() {
            self.advance();
        }
    }
}
```

#### 6.3.3 Expression Parsing

All three parsers use Pratt parsing (operator-precedence parsing) for expressions. This handles precedence and associativity cleanly.

```rust
// Shared pattern across all three language parsers

fn parse_expr(&mut self, min_bp: u8) -> Option<Expr> {
    let mut lhs = self.parse_prefix_expr()?;

    loop {
        let op = match self.current_token_to_binop() {
            Some(op) => op,
            None => break,
        };

        let (l_bp, r_bp) = op.binding_power();
        if l_bp < min_bp {
            break;
        }

        self.advance(); // consume operator
        let rhs = self.parse_expr(r_bp)?;
        lhs = Expr::Binary {
            span: lhs.span().merge(rhs.span()),
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }

    Some(lhs)
}
```

### 6.4 Language-Specific ASTs

Each language produces its own AST type. These are *not* the unified IR — they preserve the full syntactic structure of each language and are consumed only by the elaboration stage and the simulator.

```rust
// crates/aion_vhdl_parser/src/ast.rs (excerpted)

#[derive(Debug, Serialize, Deserialize)]
pub struct VhdlDesignFile {
    pub units: Vec<DesignUnit>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DesignUnit {
    Entity(EntityDecl),
    Architecture(ArchitectureDecl),
    Package(PackageDecl),
    PackageBody(PackageBodyDecl),
    Configuration(ConfigurationDecl),
    Error(Span), // Poison node from error recovery
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EntityDecl {
    pub span: Span,
    pub name: Ident,
    pub generics: Option<GenericClause>,
    pub ports: Option<PortClause>,
}

// crates/aion_sv_parser/src/ast.rs (excerpted)

#[derive(Debug, Serialize, Deserialize)]
pub struct SvSourceFile {
    pub items: Vec<SvItem>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SvItem {
    Module(SvModuleDecl),
    Interface(SvInterfaceDecl),
    Package(SvPackageDecl),
    Error(Span),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SvModuleDecl {
    pub span: Span,
    pub name: Ident,
    pub params: Vec<SvParamDecl>,
    pub ports: Vec<SvPortDecl>,
    pub items: Vec<SvModuleItem>,
}
```

### 6.5 Serialization

Parsed ASTs are serialized to disk using `bincode` at the `out/.aion-cache/ast/` path, keyed by the source file's `ContentHash`. This enables incremental compilation — unchanged files are not re-parsed.

```
out/.aion-cache/
  ast/
    <content_hash_1>.ast   # Serialized VhdlDesignFile
    <content_hash_2>.ast   # Serialized SvSourceFile
    ...
```

---

## 7. Intermediate Representation — AionIR

### 7.1 Design Philosophy

AionIR is a **single, unified intermediate representation** that all pipeline stages downstream of elaboration consume and produce. It is inspired by LLVM's IR philosophy:

- **Language-independent:** No VHDL-isms or Verilog-isms. Everything is lowered to a common semantic model.
- **Hierarchical:** Preserves the module hierarchy for incremental compilation and reporting.
- **SSA-like for data flow:** Signals and nets are defined once (at their declaration point) and connected through explicit edges.
- **Serializable:** The full IR can be serialized to disk and deserialized without loss.
- **Queryable:** Efficient traversal for synthesis, analysis, and optimization passes.

### 7.2 Core Types

```rust
// crates/aion_ir/src/lib.rs

use serde::{Serialize, Deserialize};
use petgraph::graph::NodeIndex;

/// A complete design after elaboration.
/// This is the top-level AionIR structure, containing all modules in the design.
#[derive(Debug, Serialize, Deserialize)]
pub struct Design {
    /// All modules in the design, keyed by ModuleId.
    pub modules: Arena<ModuleId, Module>,

    /// The top-level module.
    pub top: ModuleId,

    /// Global type definitions.
    pub types: TypeDb,

    /// Source mapping: every IR node traces back to a source Span.
    pub source_map: SourceMap,
}

/// Opaque, Copy-able ID for a module.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ModuleId(u32);

/// Opaque, Copy-able ID for a signal within a module.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SignalId(u32);

/// Opaque, Copy-able ID for a cell (primitive or instantiation) within a module.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct CellId(u32);

/// Opaque, Copy-able ID for a process/always block within a module.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ProcessId(u32);

/// Opaque, Copy-able ID for a port on a module.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct PortId(u32);
```

### 7.3 Module Representation

```rust
// crates/aion_ir/src/module.rs

/// A single hardware module in the design.
#[derive(Debug, Serialize, Deserialize)]
pub struct Module {
    pub id: ModuleId,
    pub name: Ident,
    pub span: Span,

    /// Module parameters (after generic/parameter resolution).
    pub params: Vec<Parameter>,

    /// Module ports (the external interface).
    pub ports: Vec<Port>,

    /// All signals (wires, registers) declared within this module.
    pub signals: Arena<SignalId, Signal>,

    /// Primitive cells (logic gates, LUTs) and module instantiations.
    pub cells: Arena<CellId, Cell>,

    /// Behavioral processes (always blocks, VHDL processes).
    /// These are present before synthesis and lowered to cells during synthesis.
    pub processes: Arena<ProcessId, Process>,

    /// Direct combinational assignments (assign statements, concurrent assignments).
    pub assignments: Vec<Assignment>,

    /// Clock domain annotations.
    pub clock_domains: Vec<ClockDomain>,

    /// Content hash of this module's source inputs (for incremental compilation).
    pub content_hash: ContentHash,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Port {
    pub id: PortId,
    pub name: Ident,
    pub direction: PortDirection,
    pub ty: TypeId,
    pub signal: SignalId, // The signal backing this port
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortDirection {
    Input,
    Output,
    InOut,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Signal {
    pub id: SignalId,
    pub name: Ident,
    pub ty: TypeId,
    pub kind: SignalKind,
    pub init: Option<ConstValue>,     // Initial/reset value
    pub clock_domain: Option<ClockDomainId>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalKind {
    Wire,     // Combinational signal
    Reg,      // Sequential signal (flip-flop output)
    Latch,    // Latch output (usually a lint warning)
    Port,     // Backed by a port
    Const,    // Compile-time constant
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Parameter {
    pub name: Ident,
    pub ty: TypeId,
    pub value: ConstValue,  // Resolved value after elaboration
    pub span: Span,
}
```

### 7.4 Cells and Connectivity

```rust
// crates/aion_ir/src/cell.rs

/// A cell is either a primitive operation or a module instantiation.
#[derive(Debug, Serialize, Deserialize)]
pub struct Cell {
    pub id: CellId,
    pub name: Ident,
    pub kind: CellKind,
    pub connections: Vec<Connection>,
    pub span: Span,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum CellKind {
    /// Instantiation of another module.
    Instance {
        module: ModuleId,
        params: Vec<(Ident, ConstValue)>,
    },

    /// Primitive combinational operations (post-synthesis).
    And { width: u32 },
    Or { width: u32 },
    Xor { width: u32 },
    Not { width: u32 },
    Mux { width: u32, select_width: u32 },
    Add { width: u32 },
    Sub { width: u32 },
    Mul { width: u32 },
    Shl { width: u32 },
    Shr { width: u32 },
    Eq { width: u32 },
    Lt { width: u32 },
    Concat,
    Slice { offset: u32, width: u32 },
    Repeat { count: u32 },
    Const { value: LogicVec },

    /// Sequential elements.
    Dff { width: u32, has_reset: bool, has_enable: bool },
    Latch { width: u32 },

    /// Memory primitives.
    Memory {
        depth: u32,
        width: u32,
        read_ports: u32,
        write_ports: u32,
    },

    /// Technology-mapped primitives (post-tech-mapping).
    Lut { width: u32, init: LogicVec }, // Look-up table
    Carry { width: u32 },               // Carry chain
    Bram(BramConfig),                    // Block RAM
    Dsp(DspConfig),                      // DSP block
    Pll(PllConfig),                      // PLL/clock management
    Iobuf(IobufConfig),                  // I/O buffer

    /// Black box (unresolved or errored module).
    BlackBox { port_names: Vec<Ident> },
}

/// A connection between a cell port and a signal.
#[derive(Debug, Serialize, Deserialize)]
pub struct Connection {
    pub port_name: Ident,
    pub direction: PortDirection,
    pub signal: SignalRef,
}

/// A reference to a signal or a part of a signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalRef {
    /// Full signal.
    Signal(SignalId),
    /// Bit slice of a signal.
    Slice { signal: SignalId, high: u32, low: u32 },
    /// Concatenation of signal references.
    Concat(Vec<SignalRef>),
    /// Constant value.
    Const(LogicVec),
}
```

### 7.5 Process Representation

Processes represent behavioral code (VHDL processes, Verilog `always` blocks) before they are lowered to cells by the synthesis engine.

```rust
// crates/aion_ir/src/process.rs

#[derive(Debug, Serialize, Deserialize)]
pub struct Process {
    pub id: ProcessId,
    pub name: Option<Ident>,
    pub kind: ProcessKind,
    pub body: Statement,
    pub sensitivity: Sensitivity,
    pub span: Span,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ProcessKind {
    Combinational,    // always_comb / combinational process
    Sequential,       // always_ff / clocked process
    Latched,          // always_latch
    Initial,          // initial block (testbench only, not synthesizable)
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Sensitivity {
    All,                            // always_comb / process(all)
    EdgeList(Vec<EdgeSensitivity>), // always_ff @(posedge clk, negedge rst)
    SignalList(Vec<SignalId>),       // process(a, b, c)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EdgeSensitivity {
    pub signal: SignalId,
    pub edge: Edge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Edge {
    Posedge,
    Negedge,
    Both,
}

/// Behavioral statements (lowered from language-specific constructs).
#[derive(Debug, Serialize, Deserialize)]
pub enum Statement {
    Assign { target: SignalRef, value: Expr, span: Span },
    If { condition: Expr, then_body: Box<Statement>, else_body: Option<Box<Statement>>, span: Span },
    Case { subject: Expr, arms: Vec<CaseArm>, default: Option<Box<Statement>>, span: Span },
    Block { stmts: Vec<Statement>, span: Span },
    Wait { duration: Option<Expr>, span: Span }, // Simulation only
    Assertion { kind: AssertionKind, condition: Expr, message: Option<String>, span: Span },
    Display { format: String, args: Vec<Expr>, span: Span }, // $display / report
    Finish { span: Span }, // $finish / stop
    Nop,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CaseArm {
    pub patterns: Vec<Expr>,
    pub body: Statement,
    pub span: Span,
}

/// Expressions in the IR (language-independent).
#[derive(Debug, Serialize, Deserialize)]
pub enum Expr {
    Signal(SignalRef),
    Literal(LogicVec),
    Unary { op: UnaryOp, operand: Box<Expr>, ty: TypeId, span: Span },
    Binary { op: BinaryOp, lhs: Box<Expr>, rhs: Box<Expr>, ty: TypeId, span: Span },
    Ternary { condition: Box<Expr>, true_val: Box<Expr>, false_val: Box<Expr>, ty: TypeId, span: Span },
    FuncCall { name: Ident, args: Vec<Expr>, ty: TypeId, span: Span },
    Concat(Vec<Expr>),
    Repeat { expr: Box<Expr>, count: u32, span: Span },
    Index { expr: Box<Expr>, index: Box<Expr>, span: Span },
    Slice { expr: Box<Expr>, high: Box<Expr>, low: Box<Expr>, span: Span },
}
```

### 7.6 Type System

```rust
// crates/aion_ir/src/types.rs

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TypeId(u32);

/// Central type database — interned types for cheap comparison.
#[derive(Debug, Serialize, Deserialize)]
pub struct TypeDb {
    types: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Type {
    /// Single bit.
    Bit,

    /// Bit vector of known width.
    BitVec { width: u32, signed: bool },

    /// Integer (for parameters and constants).
    Integer,

    /// Real (for parameters and simulation).
    Real,

    /// Boolean.
    Bool,

    /// String (for parameters and simulation).
    Str,

    /// Array type (for memories and multi-dimensional signals).
    Array { element: TypeId, size: u32 },

    /// Enum type (for FSMs).
    Enum { name: Ident, variants: Vec<Ident> },

    /// Record / struct type (from VHDL records or SV structs).
    Record { name: Ident, fields: Vec<(Ident, TypeId)> },

    /// Error type (from failed type resolution).
    Error,
}

impl TypeDb {
    pub fn intern(&mut self, ty: Type) -> TypeId;
    pub fn get(&self, id: TypeId) -> &Type;
    pub fn bit_width(&self, id: TypeId) -> Option<u32>;
}
```

### 7.7 Source Map

Every IR node traces back to the original source location. This enables diagnostics at any pipeline stage to point to the user's source code.

```rust
// crates/aion_ir/src/source_map.rs

/// Maps IR entity IDs to their original source spans.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SourceMap {
    module_spans: HashMap<ModuleId, Span>,
    signal_spans: HashMap<(ModuleId, SignalId), Span>,
    cell_spans: HashMap<(ModuleId, CellId), Span>,
    process_spans: HashMap<(ModuleId, ProcessId), Span>,
}
```

---

## 8. Elaboration Engine

### 8.1 Overview

The elaboration engine (`aion_elaborate`) transforms language-specific ASTs into AionIR. This is where:
- Module hierarchy is resolved (instantiation tree flattened to a graph)
- Generics/parameters are evaluated and substituted
- Generate blocks are expanded
- Types are resolved and checked
- Language-specific constructs are lowered to the common IR

### 8.2 Elaboration Algorithm

```
Input:  Set of parsed ASTs (from disk cache)
        + aion.toml configuration (top module, parameters)
Output: Design (AionIR) written to disk

1. Build a "module registry" — map every declared module/entity to its AST node
2. Identify the top-level module from aion.toml
3. Starting from the top module, recursively elaborate:
   a. Resolve parameters (evaluate constant expressions, apply defaults)
   b. Create a new Module in the Design arena
   c. Elaborate ports → Port + Signal entries
   d. Elaborate declarations → Signal entries
   e. Elaborate behavioral code → Process entries
   f. For each instantiation:
      i.   Resolve the target module name in the registry
      ii.  Evaluate parameter overrides
      iii. Check if this (module_name, params) combination is already elaborated
           - If yes: reuse the existing ModuleId (avoids duplication)
           - If no: recursively elaborate the target module with these params
      iv.  Create a Cell::Instance entry with connections
   g. Elaborate assignments → Assignment entries
4. Perform type checking across the fully elaborated design
5. Annotate clock domains (static analysis of clock tree)
6. Write the Design to disk as serialized AionIR
```

### 8.3 Key Interfaces

```rust
// crates/aion_elaborate/src/lib.rs

pub struct ElaborationContext {
    /// The design being constructed.
    design: Design,
    /// Registry of all known modules from parsed ASTs.
    module_registry: ModuleRegistry,
    /// Cache of already-elaborated (module_name, params) → ModuleId.
    elab_cache: HashMap<(Ident, Vec<ConstValue>), ModuleId>,
    /// Diagnostic sink for errors and warnings.
    sink: DiagnosticSink,
    /// Source database for span resolution.
    source_db: SourceDb,
}

pub struct ModuleRegistry {
    /// VHDL entities and architectures.
    vhdl_entities: HashMap<Ident, Vec<(VhdlEntityDecl, Vec<VhdlArchitectureDecl>)>>,
    /// Verilog/SV modules.
    sv_modules: HashMap<Ident, SvModuleDecl>,
    verilog_modules: HashMap<Ident, VerilogModuleDecl>,
}

/// Entry point: elaborate a complete design from parsed ASTs.
pub fn elaborate(
    asts: &ParsedDesign,
    config: &ProjectConfig,
    source_db: &SourceDb,
    sink: &DiagnosticSink,
) -> AionResult<Design>;

/// The parsed design: a collection of per-file ASTs loaded from cache.
pub struct ParsedDesign {
    pub vhdl_files: Vec<VhdlDesignFile>,
    pub verilog_files: Vec<VerilogSourceFile>,
    pub sv_files: Vec<SvSourceFile>,
}
```

### 8.4 Generate Block Expansion

Generate blocks (VHDL `for ... generate`, SystemVerilog `genvar`/`generate for`) are expanded at elaboration time. Each iteration creates a new scope with:
- A unique name (e.g., `gen_block[0]`, `gen_block[1]`)
- Its own signals, cells, and assignments
- Parameter values substituted from the loop variable

Conditional generates (`if ... generate`) evaluate their condition at elaboration time and include only the active branch.

### 8.5 Mixed-Language Support

Mixed-language instantiation (e.g., a VHDL entity instantiating a Verilog module) is handled at the module registry level. The registry contains entries from all three languages, and the elaborator resolves instantiation targets across language boundaries. Port type compatibility is checked during connection elaboration using the unified `TypeId` system.

---

## 9. Synthesis Engine

### 9.1 Overview

The synthesis engine (`aion_synth`) transforms elaborated AionIR into a technology-mapped netlist ready for place and route. Synthesis operates in three major phases:

1. **Behavioral lowering:** Processes and behavioral code → combinational and sequential cells
2. **Logic optimization:** Technology-independent optimization passes
3. **Technology mapping:** Map generic cells to target architecture primitives

### 9.2 Behavioral Lowering

Processes (always blocks, VHDL processes) are lowered to concrete cell networks:

```
Process (behavioral) → Analysis → Cell graph

Sequential process (always_ff):
  → DFF cells for registered signals
  → Combinational MUX trees for if/case → DFF.D input

Combinational process (always_comb):
  → MUX trees, logic gates, arithmetic cells
  → Latch detection: if a signal is not assigned in all control paths,
    emit W106 warning and infer a Latch cell
```

#### FSM Detection

During behavioral lowering, the synthesis engine detects finite state machines:

1. Identify `enum`-typed or range-typed register signals used in case statements
2. Extract state transition graph
3. Apply FSM encoding (one-hot, binary, or gray based on `[build] optimization` setting):
   - `area` → binary encoding
   - `speed` → one-hot encoding
   - `balanced` → heuristic based on state count

### 9.3 Logic Optimization Passes

After lowering, the following technology-independent passes run:

| Pass | Description | Ordering |
|------|-------------|----------|
| Constant propagation | Evaluate constant inputs through logic cones | 1st |
| Dead code elimination | Remove cells with no fanout to outputs | 2nd |
| Common subexpression elimination | Share identical logic cones | 3rd |
| Boolean optimization | AND-Inverter Graph (AIG) optimization using rewriting rules | 4th |
| Retiming | Move registers across combinational logic to balance pipeline stages (when `optimization = "speed"`) | 5th |
| Resource sharing | Share arithmetic operators across mutually exclusive paths | 6th |

### 9.4 Technology Mapping

Technology mapping converts the optimized generic netlist into target-architecture primitives.

```rust
// crates/aion_synth/src/tech_map.rs

/// Technology mapper trait — implemented per architecture family.
pub trait TechMapper: Send + Sync {
    /// Map a generic cell to architecture-specific primitives.
    fn map_cell(&self, cell: &Cell, module: &mut Module) -> Vec<CellId>;

    /// Infer BRAM from memory patterns.
    fn infer_bram(&self, mem: &MemoryCell, module: &mut Module) -> Option<CellId>;

    /// Infer DSP from arithmetic patterns.
    fn infer_dsp(&self, arith: &ArithmeticPattern, module: &mut Module) -> Option<CellId>;

    /// Map LUT — pack Boolean functions into device LUTs.
    fn map_to_luts(&self, logic_cone: &LogicCone, module: &mut Module) -> Vec<CellId>;
}

/// Intel ALM mapper (6-input fracturable LUT).
pub struct AlmMapper {
    pub family: IntelFamily,
}

/// Xilinx 6-LUT mapper.
pub struct Lut6Mapper {
    pub family: XilinxFamily,
}
```

#### LUT Mapping Algorithm

LUT mapping uses a depth-optimal cut enumeration algorithm:

1. Build an AND-Inverter Graph (AIG) from the Boolean functions
2. Enumerate feasible cuts for each AIG node (cuts that fit in a K-input LUT)
3. Select a minimum-depth cover using dynamic programming
4. For area optimization: apply area-flow and exact-area recovery passes
5. Pack selected cuts into LUT cells with the computed truth tables

#### BRAM Inference

Memory arrays that meet the following criteria are inferred as BRAM:

- Depth × width exceeds the LUT-RAM threshold for the target device
- Access patterns match supported BRAM configurations (single-port, simple-dual-port, true-dual-port)
- Read latency is compatible (registered output for M10K, optional for Xilinx Block RAM)

If a memory pattern is detected but doesn't cleanly map, a vendor-specific lint warning (`S401`) is emitted with a suggestion for how to restructure the code.

#### DSP Inference

Multiply and multiply-accumulate patterns are detected and mapped to DSP blocks:

- `a * b` → single DSP multiply
- `a * b + c` → DSP with integrated add
- `acc <= acc + a * b` → DSP in accumulate mode
- Pipeline registers before/after multiply → absorbed into DSP internal registers

### 9.5 Module-Level Incremental Synthesis

Synthesis operates on individual modules. When only one module's body has changed (ports unchanged), only that module is re-synthesized. The synthesized netlist for each module is serialized independently:

```
out/.aion-cache/synth/
  <module_content_hash_1>.netlist
  <module_content_hash_2>.netlist
  ...
```

### 9.6 Key Interfaces

```rust
// crates/aion_synth/src/lib.rs

/// Synthesize a complete design from AionIR to a technology-mapped netlist.
pub fn synthesize(
    design: &Design,
    target: &ResolvedTarget,
    arch: &dyn Architecture,
    opt_level: OptLevel,
    sink: &DiagnosticSink,
) -> AionResult<MappedDesign>;

/// The output of synthesis: a design where all processes have been lowered
/// and all cells are technology-mapped.
#[derive(Debug, Serialize, Deserialize)]
pub struct MappedDesign {
    /// Modules with only technology-mapped cells (no processes).
    pub modules: Arena<ModuleId, MappedModule>,
    pub top: ModuleId,
    pub types: TypeDb,
    pub source_map: SourceMap,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MappedModule {
    pub id: ModuleId,
    pub name: Ident,
    pub ports: Vec<Port>,
    pub signals: Arena<SignalId, Signal>,
    pub cells: Arena<CellId, Cell>, // Only tech-mapped cells: LUT, DFF, BRAM, DSP, etc.
    pub assignments: Vec<Assignment>,
    pub resource_usage: ResourceUsage,
    pub content_hash: ContentHash,
}
```

---

## 10. Place & Route

### 10.1 Overview

The place-and-route engine (`aion_pnr`) takes a technology-mapped netlist and produces a fully placed and routed design for a specific FPGA device. This is the most technically challenging component of Aion.

### 10.2 Data Structure Evaluation and Recommendation

The P&R engine's inner loops iterate over the netlist millions of times during placement and routing. The data structure choice is critical for performance.

#### Option A: Arena-Based Allocation

Store all entities (cells, nets, pins, sites) in typed arenas indexed by opaque IDs.

**Pros:** Cache-friendly sequential access, simple implementation, natural fit for serialization. Deletion is O(1) via a free list or tombstone.

**Cons:** Random access patterns during routing still cause cache misses. Difficult to add new fields without modifying the arena type.

#### Option B: ECS-Style (Entity-Component-System)

Store entity IDs and component data in separate arrays (like a column-store database).

**Pros:** Excellent cache behavior when iterating over a single component (e.g., "all cell positions"). Easy to add new data components. Parallel iteration via `rayon`.

**Cons:** More complex API. Scattering entity data across components makes per-entity operations (e.g., "get all information about this cell") slower.

#### Option C: Petgraph-Heavy

Represent the entire netlist as a `petgraph::Graph` with cells as nodes and nets as hyperedge groups.

**Pros:** Natural graph operations (BFS, DFS, topological sort). `petgraph` is well-tested.

**Cons:** Poor cache locality for P&R inner loops. Hyperedge representation is awkward in standard graph libraries.

#### Recommendation: Hybrid Arena + Auxiliary Structures

Aion uses **arena-based allocation** as the primary storage with **auxiliary indexed structures** for efficient queries:

```rust
// crates/aion_pnr/src/data.rs

/// Opaque IDs for P&R entities.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PnrCellId(u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PnrNetId(u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PnrPinId(u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SiteId(u32);

/// The P&R netlist — flat, fully elaborated, arena-based.
#[derive(Debug, Serialize, Deserialize)]
pub struct PnrNetlist {
    pub cells: Arena<PnrCellId, PnrCell>,
    pub nets: Arena<PnrNetId, PnrNet>,
    pub pins: Arena<PnrPinId, PnrPin>,

    // Auxiliary indices (rebuilt from arena data, not serialized):
    #[serde(skip)]
    cell_to_pins: HashMap<PnrCellId, Vec<PnrPinId>>,
    #[serde(skip)]
    net_to_pins: HashMap<PnrNetId, Vec<PnrPinId>>,
    #[serde(skip)]
    pin_to_cell: HashMap<PnrPinId, PnrCellId>,
    #[serde(skip)]
    pin_to_net: HashMap<PnrPinId, PnrNetId>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PnrCell {
    pub id: PnrCellId,
    pub name: Ident,
    pub cell_type: PnrCellType,
    pub placement: Option<SiteId>,  // None if unplaced
    pub is_fixed: bool,             // I/O pads, locked cells
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PnrCellType {
    Lut { inputs: u8, init: LogicVec },
    Dff,
    Carry,
    Bram(BramConfig),
    Dsp(DspConfig),
    Iobuf { direction: PortDirection, standard: Ident },
    Pll(PllConfig),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PnrNet {
    pub id: PnrNetId,
    pub name: Ident,
    pub driver: PnrPinId,          // Source pin
    pub sinks: Vec<PnrPinId>,     // Destination pins
    pub routing: Option<RouteTree>, // None if unrouted
    pub timing_critical: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PnrPin {
    pub id: PnrPinId,
    pub name: Ident,
    pub direction: PortDirection,
    pub cell: PnrCellId,
    pub net: Option<PnrNetId>,
}
```

**Why this approach:**

1. Arena storage gives cache-friendly iteration for the common case (iterating all cells, all nets).
2. Auxiliary `HashMap` indices provide O(1) lookup for connectivity queries.
3. Indices are rebuilt on deserialization (not serialized), keeping the on-disk format simple.
4. New data fields can be added to `PnrCell`/`PnrNet` without restructuring.
5. The arena approach naturally supports the serialized stage boundary model.

### 10.3 Architecture Model

```rust
// crates/aion_arch/src/lib.rs

/// Trait representing a target FPGA architecture.
/// Implemented per device family (Cyclone V, Artix-7, etc.).
pub trait Architecture: Send + Sync {
    fn family_name(&self) -> &str;
    fn device_name(&self) -> &str;

    // --- Fabric Model ---
    fn grid_dimensions(&self) -> (u32, u32); // (columns, rows)
    fn get_tile(&self, col: u32, row: u32) -> Option<&Tile>;
    fn get_site(&self, id: SiteId) -> &Site;
    fn sites_of_type(&self, ty: SiteType) -> &[SiteId];
    fn bel_in_site(&self, site: SiteId, bel_name: &str) -> Option<BelId>;

    // --- Routing Model ---
    fn routing_graph(&self) -> &RoutingGraph;
    fn pip_delay(&self, pip: PipId) -> Delay;
    fn wire_delay(&self, wire: WireId) -> Delay;

    // --- Timing Model ---
    fn cell_delay(&self, cell_type: &PnrCellType, from_pin: &str, to_pin: &str) -> Delay;
    fn setup_time(&self, cell_type: &PnrCellType, data_pin: &str) -> Delay;
    fn hold_time(&self, cell_type: &PnrCellType, data_pin: &str) -> Delay;
    fn clock_to_out(&self, cell_type: &PnrCellType, clk_pin: &str, q_pin: &str) -> Delay;

    // --- Resource Counts ---
    fn total_luts(&self) -> u32;
    fn total_ffs(&self) -> u32;
    fn total_bram(&self) -> u32;
    fn total_dsp(&self) -> u32;
    fn total_io(&self) -> u32;
    fn total_pll(&self) -> u32;

    // --- Bitstream ---
    fn bitstream_generator(&self) -> &dyn BitstreamGenerator;

    // --- Tech Mapping ---
    fn tech_mapper(&self) -> &dyn TechMapper;
}

/// A tile in the FPGA grid.
#[derive(Debug)]
pub struct Tile {
    pub col: u32,
    pub row: u32,
    pub tile_type: TileType,
    pub sites: Vec<SiteId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileType {
    Logic,   // CLB / LAB
    Bram,    // Block RAM column
    Dsp,     // DSP column
    Io,      // I/O bank
    Clock,   // Clock network tile
    Empty,   // Unusable / padding
}

/// A site is a specific placement location (e.g., one ALM, one LUT+FF pair).
#[derive(Debug)]
pub struct Site {
    pub id: SiteId,
    pub site_type: SiteType,
    pub bels: Vec<Bel>,       // Basic Elements of Logic within the site
    pub tile: (u32, u32),
}

/// Routing graph for the device.
#[derive(Debug)]
pub struct RoutingGraph {
    pub wires: Arena<WireId, Wire>,
    pub pips: Arena<PipId, Pip>,  // Programmable Interconnect Points
}
```

### 10.4 Placement Algorithm

Aion uses a **simulated annealing** placer as the primary algorithm, with an **analytical placer** for initial seed placement on larger designs.

#### Analytical Placement (Seed)

For designs larger than 10k cells, an analytical placer provides the initial placement:

1. Build a quadratic wirelength objective (half-perimeter wirelength model)
2. Solve the unconstrained quadratic program (conjugate gradient solver)
3. Spread cells to resolve overlap (recursive bisection partitioning)
4. Legalize to valid sites (greedy nearest-site assignment)

This produces a coarse but reasonable placement in O(n log n) time.

#### Simulated Annealing Refinement

Starting from the analytical seed (or random for small designs):

```
T = initial_temperature (proportional to design size)
while T > final_temperature:
    for i in 0..moves_per_temperature:
        // Propose a random move:
        //   - Swap two cells of compatible type
        //   - Move a cell to an empty compatible site
        //   - Swap two cells within a window (reduces as T decreases)
        proposed_move = random_move(T)

        // Evaluate cost delta:
        //   ΔC = Δ(wirelength) + α·Δ(timing) + β·Δ(congestion)
        delta_cost = evaluate_move(proposed_move)

        // Accept if improvement, or probabilistically if worse:
        if delta_cost < 0 || random() < exp(-delta_cost / T):
            apply_move(proposed_move)

    T *= cooling_rate  // Typically 0.95-0.99
```

**Cost function:**

```
C = w_wl · HPWL + w_timing · WNS_penalty + w_congestion · congestion_estimate
```

Where:
- `HPWL` = total half-perimeter wirelength across all nets
- `WNS_penalty` = worst negative slack penalty (only after initial STA)
- `congestion_estimate` = routing congestion estimate from placement density

**Parallelism:** The annealing loop is parallelized using independent move regions. The placement grid is partitioned into non-overlapping regions, and each region can be optimized independently on a separate thread. Synchronization occurs at temperature step boundaries.

### 10.5 Routing Algorithm

Aion uses **PathFinder** — a negotiated congestion-driven router:

```
for iteration in 1..max_iterations:
    for each net (ordered by criticality):
        rip_up(net)  // Remove current routing if any
        route(net)    // A* search on the routing graph

        // A* cost: delay + h·history_cost + p·present_congestion
        // h increases each iteration (forces resolution of congestion)
        // p increases each iteration (more aggressive avoidance)

    if no_congestion():
        break  // Legal routing found

    update_history_costs()  // Increase cost of overused resources
    update_criticality()    // Re-run STA, update net priorities
```

**Net ordering:** Nets are routed in decreasing criticality order. Critical-path nets are routed first when routing resources are least congested.

**Parallel routing:** Nets that share no routing resources can be routed in parallel. Aion uses a graph coloring scheme to identify independent net groups and routes each group on a separate thread. Congestion updates are synchronized between parallel batches.

### 10.6 Incremental P&R

When only some modules have changed:

1. Identify the set of affected cells and nets
2. Rip up routing for affected nets
3. Unplace affected cells (keep unaffected cells locked)
4. Re-place affected cells using constrained annealing (only allow moves near original locations)
5. Re-route affected nets

This avoids the full annealing run and typically converges in seconds for small changes.

---

## 11. Timing Analysis

### 11.1 Overview

The timing analysis engine (`aion_timing`) performs static timing analysis (STA) on the placed and routed design.

### 11.2 Timing Graph

```rust
// crates/aion_timing/src/lib.rs

/// A timing graph node: either a cell pin or a routing node.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimingNodeId(u32);

/// A timing edge with delay information.
#[derive(Debug)]
pub struct TimingEdge {
    pub from: TimingNodeId,
    pub to: TimingNodeId,
    pub delay: Delay,
    pub edge_type: TimingEdgeType,
}

#[derive(Debug, Clone, Copy)]
pub enum TimingEdgeType {
    CellDelay,    // Combinational delay through a cell
    NetDelay,     // Routing delay (wire + PIP)
    SetupCheck,   // Setup time constraint at FF input
    HoldCheck,    // Hold time constraint at FF input
    ClockToQ,     // Clock-to-output delay at FF
}

/// Delay value with min/max corners.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Delay {
    pub min_ns: f64,
    pub typ_ns: f64,
    pub max_ns: f64,
}

/// Timing constraints parsed from SDC/XDC files.
#[derive(Debug, Serialize, Deserialize)]
pub struct TimingConstraints {
    pub clocks: Vec<ClockConstraint>,
    pub input_delays: Vec<IoDelay>,
    pub output_delays: Vec<IoDelay>,
    pub false_paths: Vec<FalsePath>,
    pub multicycle_paths: Vec<MulticyclePath>,
    pub max_delay_paths: Vec<MaxDelayPath>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClockConstraint {
    pub name: Ident,
    pub period_ns: f64,
    pub port: Ident,
    pub waveform: Option<(f64, f64)>, // rise_time, fall_time
}

/// Run static timing analysis.
pub fn analyze_timing(
    netlist: &PnrNetlist,
    arch: &dyn Architecture,
    constraints: &TimingConstraints,
    sink: &DiagnosticSink,
) -> AionResult<TimingReport>;

/// The timing report: critical paths, slack, achieved frequency.
#[derive(Debug, Serialize, Deserialize)]
pub struct TimingReport {
    pub clock_domains: Vec<ClockDomainTiming>,
    pub critical_paths: Vec<CriticalPath>,
    pub worst_slack_ns: f64,
    pub achieved_frequency: Frequency,
    pub target_frequency: Frequency,
    pub met: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CriticalPath {
    pub from: TimingEndpoint,
    pub to: TimingEndpoint,
    pub delay_ns: f64,
    pub slack_ns: f64,
    pub elements: Vec<PathElement>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathElement {
    pub node_name: String,
    pub node_type: String,     // "LUT", "DFF", "routing", etc.
    pub delay_ns: f64,
    pub cumulative_ns: f64,
    pub location: Option<(u32, u32)>, // Grid coordinates
    pub source_span: Option<Span>,
}
```

### 11.3 STA Algorithm

1. **Build timing graph** from placed/routed netlist + architecture timing models
2. **Forward propagation** (arrival times): Traverse from inputs/clock-edges forward, accumulating delays
3. **Backward propagation** (required times): Traverse from outputs/clock-edges backward, computing required arrival times
4. **Slack computation:** `slack = required_time - arrival_time` at each endpoint
5. **Critical path extraction:** Trace back from the worst-slack endpoint to find the critical path

### 11.4 SDC/XDC Parsing

A dedicated SDC/XDC parser handles timing constraint files:

```rust
// crates/aion_timing/src/sdc.rs

/// Parse SDC (Synopsys Design Constraints) file.
pub fn parse_sdc(
    path: &Path,
    source_db: &mut SourceDb,
    sink: &DiagnosticSink,
) -> AionResult<TimingConstraints>;

/// Parse Xilinx XDC (Xilinx Design Constraints) file.
/// XDC is a superset of SDC with Xilinx-specific commands.
pub fn parse_xdc(
    path: &Path,
    source_db: &mut SourceDb,
    sink: &DiagnosticSink,
) -> AionResult<TimingConstraints>;
```

Supported SDC commands:
- `create_clock`, `create_generated_clock`
- `set_input_delay`, `set_output_delay`
- `set_false_path`, `set_multicycle_path`
- `set_max_delay`, `set_min_delay`
- `set_clock_groups`
- `get_ports`, `get_pins`, `get_nets`, `get_clocks` (collection accessors)

---

## 12. Bitstream Generation

### 12.1 Overview

Bitstream generation (`aion_bitstream`) converts a placed and routed design into a vendor-specific binary file that configures the FPGA.

### 12.2 Architecture

```rust
// crates/aion_bitstream/src/lib.rs

/// Trait for generating bitstreams for a specific device.
pub trait BitstreamGenerator: Send + Sync {
    /// Generate a bitstream from a placed/routed design.
    fn generate(
        &self,
        netlist: &PnrNetlist,
        arch: &dyn Architecture,
        sink: &DiagnosticSink,
    ) -> AionResult<Bitstream>;

    /// Supported output formats for this generator.
    fn supported_formats(&self) -> &[BitstreamFormat];
}

#[derive(Debug)]
pub struct Bitstream {
    pub data: Vec<u8>,
    pub format: BitstreamFormat,
    pub device: String,
    pub checksum: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitstreamFormat {
    Sof,  // Intel SRAM Object File
    Pof,  // Intel Programmer Object File
    Rbf,  // Intel Raw Binary File
    Bit,  // Xilinx bitstream
}
```

### 12.3 Intel Bitstream Generation

Aion's Intel bitstream generation builds on the reverse-engineering work of the open-source FPGA community:
- **Mistral** project for Cyclone V bitstream documentation
- Community JTAG documentation for programming protocols

The Intel bitstream pipeline:

1. **Configuration bit assembly:** For each placed and routed element, look up the configuration bits in the architecture database and set the corresponding bits in a configuration RAM image.
2. **Frame assembly:** Group configuration bits into device-specific frames (Cyclone V uses a column-based frame structure).
3. **CRC computation:** Compute per-frame and global CRC checksums.
4. **Format wrapping:** Wrap the configuration data in SOF/POF/RBF container format with appropriate headers.

### 12.4 Xilinx Bitstream Generation

Xilinx bitstream generation draws on:
- **Project X-Ray** database for Artix-7 / 7-series bitstream documentation
- Frame-based configuration architecture documentation

The Xilinx bitstream pipeline:

1. **Frame-level configuration:** Each configuration element (LUT init, routing PIP, BRAM content) maps to specific bits within specific frames.
2. **Frame assembly:** Organize bits into the device's configuration frame array.
3. **Header generation:** Create the BIT file header with device ID, timestamp, design name.
4. **Configuration commands:** Generate the FPGA configuration command sequence (write to FDRI register, etc.).
5. **CRC generation:** Compute CRC-32 for integrity verification.

### 12.5 Verification

Bitstream correctness is verified by:

1. **Round-trip check:** Read back the bitstream, decode it, and verify it matches the placed/routed netlist.
2. **Golden reference comparison:** For reference designs, compare Aion's bitstream against vendor-generated bitstreams bit-by-bit (configuration bits only, ignoring metadata).
3. **Hardware verification:** Program the device and verify functional behavior via JTAG readback.

---

## 13. Simulator

### 13.1 Overview

Aion's built-in simulator (`aion_sim`) is an event-driven HDL simulator supporting VHDL, Verilog, and SystemVerilog. It operates on the AionIR (for synthesizable designs) or directly on ASTs (for testbench constructs that don't lower to IR).

### 13.2 Event-Driven Kernel

```rust
// crates/aion_sim/src/kernel.rs

/// The simulation kernel — manages simulation time, events, and signal state.
pub struct SimKernel {
    /// Current simulation time.
    current_time: SimTime,

    /// Event queue (priority queue sorted by time, then delta).
    event_queue: BinaryHeap<Reverse<SimEvent>>,

    /// All signal values in the design.
    signals: Arena<SimSignalId, SimSignalState>,

    /// Process state for all processes in the design.
    processes: Vec<ProcessState>,

    /// Waveform recorder.
    recorder: Option<Box<dyn WaveformRecorder>>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SimTime {
    /// Femtoseconds from time 0 (enough for sub-picosecond precision).
    pub fs: u64,
    /// Delta cycle within the current time step.
    pub delta: u32,
}

#[derive(Clone)]
pub struct SimEvent {
    pub time: SimTime,
    pub signal: SimSignalId,
    pub value: LogicVec,
    pub strength: DriveStrength,
}

pub struct SimSignalState {
    pub value: LogicVec,
    pub strength: DriveStrength,
    pub drivers: Vec<Driver>,  // For resolution of multiple drivers
}

/// Drive strength levels (VHDL-style, simplified).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DriveStrength {
    HighImpedance,
    Weak,
    Pull,
    Strong,
    Supply,
}
```

### 13.3 Simulation Loop

```
1. Initialize: set all signals to their initial values
2. Execute all initial blocks / process initialization
3. Main loop:
   a. Dequeue all events at the current time + current delta
   b. Update signal values (with resolution for multi-driver nets)
   c. Identify processes sensitive to changed signals
   d. Execute triggered processes → generate new events
   e. If new events exist at current time (delta cycle):
      - Increment delta counter, go to (a)
   f. If no events at current time:
      - Advance to next event time, reset delta counter, go to (a)
   g. If event queue is empty or $finish reached:
      - Stop simulation
```

### 13.4 Mixed-Language Simulation

The simulator handles mixed-language designs through the unified AionIR:

1. Both VHDL and Verilog/SV testbench constructs are supported
2. Language-specific behaviors (VHDL signal assignment semantics vs. Verilog blocking/non-blocking) are preserved during IR lowering for simulation
3. The IR includes simulation-only constructs (`Wait`, `Display`, `Finish`, `Assertion`) that are stripped during synthesis

### 13.5 Waveform Output

```rust
// crates/aion_sim/src/waveform.rs

pub trait WaveformRecorder {
    fn record_change(&mut self, time: SimTime, signal: SimSignalId, value: &LogicVec);
    fn finalize(&mut self, output_path: &Path) -> AionResult<()>;
}

pub struct VcdRecorder { /* ... */ }
pub struct FstRecorder { /* ... */ }
pub struct GhwRecorder { /* ... */ }

/// Create a recorder based on configuration.
pub fn create_recorder(format: WaveformFormat) -> Box<dyn WaveformRecorder>;
```

### 13.6 Interactive Mode (`aion sim`)

The interactive simulator wraps the kernel with a TUI command interface:

```rust
// crates/aion_sim/src/interactive.rs

pub struct InteractiveSim {
    kernel: SimKernel,
    breakpoints: Vec<Breakpoint>,
    watches: Vec<Watch>,
    command_history: Vec<String>,
}

pub enum SimCommand {
    Run(SimTime),                      // run <duration>
    Step,                              // step one delta cycle
    Inspect(Vec<String>),              // inspect <signal> [...]
    Breakpoint(BreakpointSpec),        // breakpoint <file>:<line>
    Watch(WatchSpec),                  // watch <signal> [condition]
    Continue,                          // resume to next breakpoint
    Dump(PathBuf),                     // dump waveform to file
    Scope(String),                     // navigate hierarchy
    Help,
    Quit,
}
```

---

## 14. Lint Engine

### 14.1 Overview

The lint engine (`aion_lint`) performs static analysis on the elaborated AionIR design. It runs as part of `aion lint` and is also invoked incrementally by the LSP server.

### 14.2 Architecture

```rust
// crates/aion_lint/src/lib.rs

/// A lint rule.
pub trait LintRule: Send + Sync {
    fn code(&self) -> DiagnosticCode;
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn default_severity(&self) -> Severity;

    /// Run the lint rule on a single module.
    fn check_module(
        &self,
        module: &Module,
        design: &Design,
        arch: Option<&dyn Architecture>,
        sink: &DiagnosticSink,
    );
}

/// The lint engine — manages rules and configuration.
pub struct LintEngine {
    rules: Vec<Box<dyn LintRule>>,
    config: LintConfig,
}

impl LintEngine {
    pub fn new(config: &LintConfig) -> Self;

    /// Register all built-in rules.
    pub fn register_builtin_rules(&mut self);

    /// Run all enabled rules on the design.
    pub fn run(
        &self,
        design: &Design,
        arch: Option<&dyn Architecture>,
        sink: &DiagnosticSink,
    );
}
```

### 14.3 Rule Implementation Example

```rust
// crates/aion_lint/src/rules/unused_signal.rs

pub struct UnusedSignal;

impl LintRule for UnusedSignal {
    fn code(&self) -> DiagnosticCode {
        DiagnosticCode { category: Category::Warning, number: 101 }
    }

    fn name(&self) -> &str { "unused-signal" }
    fn description(&self) -> &str { "signal is declared but never read" }
    fn default_severity(&self) -> Severity { Severity::Warning }

    fn check_module(
        &self,
        module: &Module,
        design: &Design,
        _arch: Option<&dyn Architecture>,
        sink: &DiagnosticSink,
    ) {
        for (id, signal) in module.signals.iter() {
            if signal.name.as_str().starts_with('_') {
                continue; // Conventionally suppressed
            }
            if !is_signal_read(module, id) && signal.kind != SignalKind::Port {
                sink.emit(Diagnostic {
                    severity: Severity::Warning,
                    code: self.code(),
                    message: format!("unused signal `{}`", signal.name),
                    primary_span: signal.span,
                    labels: vec![Label {
                        span: signal.span,
                        message: "declared but never read or driven".into(),
                        style: LabelStyle::Primary,
                    }],
                    notes: vec![],
                    help: vec![
                        "remove the signal or prefix with `_` to suppress".into()
                    ],
                    fix: Some(SuggestedFix {
                        message: "prefix with `_`".into(),
                        replacements: vec![Replacement {
                            span: signal.name_span(),
                            new_text: format!("_{}", signal.name),
                        }],
                    }),
                });
            }
        }
    }
}
```

### 14.4 Built-In Rule Categories

All rules from the PRD (§9.1) are implemented:

**General Warnings (Wxxx):** W101–W108 — unused signals, undriven signals, width mismatches, missing resets, incomplete sensitivity lists, latch inference, truncation, dead logic.

**Errors (Exxx):** E101–E105 — syntax errors, non-synthesizable constructs, elaboration failures, multiple drivers, port mismatches.

**Convention (Cxxx):** C201–C204 — naming violations, missing documentation, magic numbers, style inconsistencies.

**Timing/CDC (Txxx):** T301, T302, T305, T306 — combinational loops, long chains, CDC violations, async reset in sync domain.

**Vendor-Specific (Sxxx):** S401–S404 — inefficient RAM/DSP patterns, I/O standard mismatches, resource over-utilization. These rules require an `Architecture` reference and are only active when a target is specified.

---

## 15. LSP Server

### 15.1 Overview

The LSP server (`aion_lsp`) provides real-time editor integration. It reuses the parser and elaboration engine in an incremental mode.

### 15.2 Architecture

```rust
// crates/aion_lsp/src/lib.rs

use tower_lsp::{jsonrpc, lsp_types::*, Client, LanguageServer, LspService, Server};

pub struct AionLanguageServer {
    client: Client,
    /// In-memory project state — incrementally updated.
    state: RwLock<ProjectState>,
}

struct ProjectState {
    source_db: SourceDb,
    /// Per-file parsed ASTs (updated on file change).
    asts: HashMap<FileId, ParsedFile>,
    /// Partially elaborated design (updated lazily).
    design: Option<Design>,
    /// Diagnostic sink for the current state.
    diagnostics: Vec<(FileId, Vec<Diagnostic>)>,
    /// Project configuration.
    config: Option<ProjectConfig>,
}

#[tower_lsp::async_trait]
impl LanguageServer for AionLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult>;
    async fn did_open(&self, params: DidOpenTextDocumentParams);
    async fn did_change(&self, params: DidChangeTextDocumentParams);
    async fn did_save(&self, params: DidSaveTextDocumentParams);
    async fn goto_definition(&self, params: GotoDefinitionParams) -> jsonrpc::Result<Option<GotoDefinitionResponse>>;
    async fn references(&self, params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>>;
    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>>;
    async fn completion(&self, params: CompletionParams) -> jsonrpc::Result<Option<CompletionResponse>>;
    async fn code_action(&self, params: CodeActionParams) -> jsonrpc::Result<Option<CodeActionResponse>>;
    async fn document_symbol(&self, params: DocumentSymbolParams) -> jsonrpc::Result<Option<DocumentSymbolResponse>>;
    async fn shutdown(&self) -> jsonrpc::Result<()>;
}
```

### 15.3 Incremental Updates

On file change:

1. Re-lex and re-parse only the changed file
2. If the file's AST hash changed:
   a. Re-run lint on the changed file's modules
   b. If module interfaces changed: re-elaborate affected parts of the design
   c. Publish updated diagnostics to the editor

The LSP server runs parsing and linting on a background thread (via Tokio's `spawn_blocking`) to avoid blocking the main LSP event loop.

### 15.4 Capabilities

| Feature | Implementation |
|---------|----------------|
| Diagnostics | Re-parse + lint on every change, publish via `textDocument/publishDiagnostics` |
| Go to definition | Resolve identifier at cursor → look up in module registry / signal table → return source span |
| Find references | For modules: find all instantiation sites. For signals: find all reads/writes. |
| Hover | Display: signal type, width, direction, clock domain, parameter value |
| Autocomplete | Context-dependent: module names, signal names, port names, keywords |
| Signature help | When typing a module instantiation, show port list with types |
| Code actions | Quick fixes from lint engine's `SuggestedFix` entries |
| Document symbols | Module/entity declarations, signal declarations, process/always blocks |

---

## 16. Device Programming

### 16.1 Overview

The `aion_flash` crate handles detecting connected FPGA programmers, communicating via JTAG, and programming bitstreams to devices.

### 16.2 Architecture

```rust
// crates/aion_flash/src/lib.rs

/// Trait for JTAG programmer backends.
pub trait JtagProgrammer: Send {
    fn name(&self) -> &str;
    fn detect_devices(&mut self) -> AionResult<Vec<JtagDevice>>;
    fn program(&mut self, device: &JtagDevice, bitstream: &[u8]) -> AionResult<()>;
    fn verify(&mut self, device: &JtagDevice, bitstream: &[u8]) -> AionResult<bool>;
    fn close(&mut self);
}

pub struct JtagDevice {
    pub idcode: u32,
    pub device_name: String,
    pub family: String,
    pub position: u8,  // Chain position for multi-device JTAG chains
}

/// Detect all connected programmers.
pub fn detect_programmers() -> AionResult<Vec<Box<dyn JtagProgrammer>>>;

/// USB-Blaster (I/II) implementation.
pub struct UsbBlaster { /* USB handle via rusb */ }

/// Digilent JTAG (HS2/HS3/on-board) implementation.
pub struct DigilentJtag { /* USB handle via rusb */ }
```

### 16.3 Programming Flow

1. Enumerate USB devices, identify known programmer VID/PIDs
2. Open programmer, initialize JTAG TAP
3. Read IDCODE from JTAG chain → identify device
4. Validate bitstream device ID matches connected device
5. For Intel: send SVF/XSVF programming commands via JTAG
6. For Xilinx: send configuration commands (write to FPGA config registers via JTAG)
7. Verify (optional): readback configuration and compare CRC
8. Close JTAG connection

---

## 17. Incremental Compilation & Caching

### 17.1 Cache Structure

```
out/.aion-cache/
├── manifest.json        # Maps source files → content hashes, dependency edges
├── ast/                 # Serialized per-file ASTs
│   ├── <hash>.ast
│   └── ...
├── air/                 # Serialized per-module AionIR
│   ├── <hash>.air
│   └── ...
├── synth/               # Serialized per-module mapped netlists
│   ├── <hash>.netlist
│   └── ...
└── pnr/                 # Serialized placed/routed design (per-target)
    ├── <target>/
    │   ├── <hash>.placed
    │   └── ...
    └── ...
```

### 17.2 Cache Manifest

```rust
// crates/aion_cache/src/lib.rs

#[derive(Serialize, Deserialize)]
pub struct CacheManifest {
    /// Aion version that produced this cache (invalidate on version change).
    pub aion_version: String,

    /// Per-source-file state.
    pub files: HashMap<PathBuf, FileCache>,

    /// Per-module dependency edges.
    pub module_deps: HashMap<Ident, ModuleCacheEntry>,

    /// Per-target P&R state.
    pub targets: HashMap<String, TargetCache>,
}

#[derive(Serialize, Deserialize)]
pub struct FileCache {
    pub content_hash: ContentHash,
    pub ast_cache_key: String,       // Key in ast/ directory
    pub modules_defined: Vec<Ident>, // Modules defined in this file
}

#[derive(Serialize, Deserialize)]
pub struct ModuleCacheEntry {
    /// Hash of the module's interface (ports, parameters).
    pub interface_hash: ContentHash,
    /// Hash of the module's body.
    pub body_hash: ContentHash,
    /// Modules that this module instantiates.
    pub dependencies: Vec<Ident>,
    /// Cache keys for elaborated IR and synthesized netlist.
    pub air_cache_key: String,
    pub synth_cache_key: Option<String>,
}
```

### 17.3 Invalidation Rules

| What Changed | Invalidates |
|-------------|-------------|
| Source file content hash | Re-parse that file |
| Module interface (ports/params) hash | Re-elaborate all instantiators (transitively) |
| Module body-only hash | Re-elaborate + re-synthesize only that module |
| `aion.toml` pin/constraint changes | Re-run P&R only |
| `aion.toml` target device change | Full re-synthesis + P&R |
| `aion.toml` optimization level change | Full re-synthesis + P&R |
| Aion version change | Full invalidation |

### 17.4 Incremental Build Flow

```
1. Load CacheManifest
2. Scan source files → compute content hashes
3. Identify changed files:
   a. New files: parse and add to cache
   b. Modified files: re-parse, compare AST hashes
   c. Deleted files: remove from cache, invalidate dependents
4. Identify affected modules:
   a. If interface changed: mark all transitive instantiators as dirty
   b. If body-only changed: mark only this module as dirty
5. Re-elaborate dirty modules (reuse clean modules from cache)
6. Re-synthesize dirty modules (reuse clean modules from cache)
7. Determine P&R strategy:
   a. If only body changes: incremental P&R (rip-up and re-route affected nets)
   b. If interface changes: full P&R
8. Write updated artifacts and manifest to cache
```

---

## 18. Dependency Management

### 18.1 Resolution Algorithm

```rust
// crates/aion_deps/src/lib.rs

pub struct DependencyResolver {
    cache_dir: PathBuf,  // ~/.aion/cache/
}

impl DependencyResolver {
    /// Resolve all dependencies declared in aion.toml.
    pub async fn resolve(
        &self,
        config: &ProjectConfig,
        lock_file: Option<&LockFile>,
    ) -> AionResult<ResolvedDeps>;

    /// Update the lock file to latest compatible versions.
    pub async fn update(
        &self,
        config: &ProjectConfig,
    ) -> AionResult<LockFile>;
}

#[derive(Serialize, Deserialize)]
pub struct LockFile {
    pub version: u32,
    pub dependencies: Vec<LockedDependency>,
}

#[derive(Serialize, Deserialize)]
pub struct LockedDependency {
    pub name: String,
    pub source: LockedSource,
    pub content_hash: ContentHash,
    pub transitive_deps: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub enum LockedSource {
    Git { url: String, commit: String },
    Path { path: String },
    Registry { version: String }, // Future
}

pub struct ResolvedDeps {
    /// Map from dependency name to local path containing HDL sources.
    pub deps: HashMap<String, PathBuf>,
}
```

### 18.2 Git Dependency Fetching

1. Check if the locked commit exists in `~/.aion/cache/git/<url_hash>/`
2. If not: clone (or fetch) the repository
3. Checkout the specified tag/branch/rev
4. Verify content hash matches lock file (if lock file exists)
5. Scan for `aion.toml` in the dependency to discover transitive dependencies
6. Return the local path for inclusion in the build

Git operations use `git2` (libgit2 Rust bindings) for in-process Git without shelling out.

---

## 19. Serialization & Stage Boundaries

### 19.1 Serialization Format

All inter-stage artifacts are serialized using `bincode` with `serde`. `bincode` was chosen for:
- Speed (10-100x faster than JSON/MessagePack for complex structs)
- Compactness (smaller files = faster I/O)
- Simplicity (direct `Serialize`/`Deserialize` derive macros)

### 19.2 Versioning

Each serialized artifact includes a version header:

```rust
#[derive(Serialize, Deserialize)]
pub struct ArtifactHeader {
    /// Magic bytes: "AION"
    pub magic: [u8; 4],
    /// Artifact format version (incremented on breaking IR changes).
    pub format_version: u32,
    /// Aion version that produced this artifact.
    pub aion_version: String,
    /// Content hash of the artifact data.
    pub checksum: ContentHash,
}
```

If `format_version` does not match the current Aion version's expected format, the artifact is discarded and regenerated from scratch. This ensures cache compatibility is explicit.

### 19.3 File Naming

Artifact files are named by the content hash of their inputs:

```
out/.aion-cache/ast/<xxh3_of_source_content>.ast
out/.aion-cache/air/<xxh3_of_module_content>.air
out/.aion-cache/synth/<xxh3_of_module_ir_plus_device>.netlist
out/.aion-cache/pnr/<target>/<xxh3_of_full_netlist_plus_constraints>.placed
```

Content-addressed naming means cache entries are naturally deduplicated and garbage collection is straightforward (delete entries not referenced by the current manifest).

---

## 20. CLI Architecture

### 20.1 Command Structure

```rust
// crates/aion_cli/src/main.rs

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "aion", version, about = "The modern FPGA toolchain")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Suppress non-error output.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Increase output detail.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Color output control.
    #[arg(long, global = true, default_value = "auto")]
    pub color: ColorChoice,

    /// Override aion.toml location.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Scaffold a new project.
    Init {
        name: Option<String>,
        #[arg(long, default_value = "systemverilog")]
        lang: HdlLanguage,
        #[arg(long)]
        target: Option<String>,
    },

    /// Compile the design (full pipeline).
    Build {
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        output_format: Option<BitstreamFormat>,
        #[arg(long, short)]
        jobs: Option<usize>,
        #[arg(long, default_value = "text")]
        report_format: ReportFormat,
        #[arg(long)]
        timing_report: bool,
    },

    /// Run testbenches.
    Test {
        name: Option<String>,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        waveform: Option<WaveformFormat>,
        #[arg(long)]
        no_waveform: bool,
        #[arg(long)]
        verbose: bool,
    },

    /// Interactive simulation.
    Sim {
        testbench: String,
        #[arg(long)]
        time: Option<String>,
        #[arg(long)]
        waveform: Option<WaveformFormat>,
    },

    /// Static analysis and linting.
    Lint {
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        allow: Vec<String>,
        #[arg(long)]
        deny: Vec<String>,
        #[arg(long, default_value = "text")]
        report_format: ReportFormat,
        #[arg(long)]
        target: Option<String>,
    },

    /// Program a connected FPGA.
    Flash {
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        programmer: Option<ProgrammerType>,
        #[arg(long)]
        cable: Option<String>,
        #[arg(long, default_value = "true")]
        verify: bool,
        #[arg(long)]
        format: Option<BitstreamFormat>,
    },

    /// Refresh dependency lock file.
    Update,

    /// Remove build artifacts.
    Clean,
}
```

### 20.2 Build Orchestration

The `aion build` command orchestrates the full pipeline:

```rust
// crates/aion_cli/src/build.rs (pseudocode)

pub async fn run_build(args: &BuildArgs) -> AionResult<()> {
    let config = aion_config::load_config(&project_dir)?;
    let targets = resolve_targets(&config, args.target.as_deref())?;
    let sink = DiagnosticSink::new();
    let mut source_db = SourceDb::new();

    // 1. Resolve dependencies
    let deps = aion_deps::resolve(&config, lock_file.as_ref()).await?;

    // 2. Discover and load source files
    let source_files = discover_sources(&config, &deps)?;
    for file in &source_files {
        source_db.load_file(file)?;
    }

    // 3. Load cache manifest
    let mut cache = aion_cache::load_or_create(&out_dir)?;

    // 4. Parse (parallel, incremental)
    let parsed = aion_cache::parse_incremental(
        &source_files, &source_db, &cache, &sink
    )?;

    if sink.has_errors() {
        render_diagnostics(&sink, &source_db, args.report_format);
        return Ok(());
    }

    // 5. Elaborate
    let design = aion_elaborate::elaborate(&parsed, &config, &source_db, &sink)?;

    if sink.has_errors() {
        render_diagnostics(&sink, &source_db, args.report_format);
        return Ok(());
    }

    // 6. For each target: synthesize, P&R, generate bitstream
    for target in &targets {
        let arch = aion_arch::load_architecture(&target.family, &target.device)?;
        let opt = if args.release { OptLevel::Speed } else { config.build.optimization };

        // Synthesize
        let mapped = aion_synth::synthesize(&design, target, arch.as_ref(), opt, &sink)?;

        if sink.has_errors() { continue; }

        // Place & Route
        let placed = aion_pnr::place_and_route(&mapped, arch.as_ref(), &constraints, &sink)?;

        if sink.has_errors() { continue; }

        // Timing Analysis
        let timing = aion_timing::analyze_timing(&placed, arch.as_ref(), &constraints, &sink)?;

        // Bitstream
        let bitstream = arch.bitstream_generator().generate(&placed, arch.as_ref(), &sink)?;

        // Write outputs
        write_bitstream(&bitstream, &out_dir, &target.name)?;
        write_reports(&mapped, &timing, &out_dir, &target.name, args.report_format)?;

        print_summary(&mapped, &timing, &target);
    }

    // 7. Save cache
    cache.save()?;

    render_diagnostics(&sink, &source_db, args.report_format);
    Ok(())
}
```

---

## 21. Error Reporting

### 21.1 Terminal Rendering

Aion's terminal renderer follows `rustc`'s style:

```
error[E103]: parameter `DATA_WIDTH` has no default value and is not provided
  --> src/top.sv:23:5
   |
23 |   uart_core u0 (
   |   ^^^^^^^^^ missing parameter: `DATA_WIDTH`
   |
   = note: `uart_core` declared at ip/uart_core/src/uart.sv:1
   = help: add `.DATA_WIDTH(8)` to the instantiation
```

The renderer is implemented in `aion_diagnostics::TerminalRenderer` and handles:
- ANSI color coding (red for errors, yellow for warnings, cyan for notes)
- Multi-line source snippets with context
- Multi-span annotations (primary + secondary labels)
- Line number gutters aligned to the widest number
- Unicode-safe column alignment
- Terminal width detection for wrapping

### 21.2 JSON Output

`--report-format json` emits a JSON array of diagnostics:

```json
[
  {
    "severity": "error",
    "code": "E103",
    "message": "parameter `DATA_WIDTH` has no default value and is not provided",
    "file": "src/top.sv",
    "line": 23,
    "column": 5,
    "labels": [...],
    "notes": [...],
    "help": [...]
  }
]
```

### 21.3 SARIF Output

`--report-format sarif` emits SARIF 2.1.0 for integration with GitHub Code Scanning, Azure DevOps, and IDEs that support SARIF. The `aion_diagnostics::SarifRenderer` maps Aion diagnostics to SARIF `Result` objects with `codeFlows`, `relatedLocations`, and `fixes`.

---

## 22. Parallelism Model

### 22.1 Runtime Architecture

Aion uses a two-tier parallelism model:

**Tier 1 — Tokio (async I/O):** The top-level orchestrator, LSP server, dependency fetching, and flash programming use Tokio for async I/O. Tokio manages the event loop and non-blocking operations.

**Tier 2 — Rayon (CPU-bound work):** All CPU-bound pipeline stages (parsing, elaboration, synthesis, P&R, simulation) are dispatched onto a Rayon thread pool. The thread pool size defaults to the number of logical cores and is configurable via `--jobs` or `AION_JOBS`.

```rust
// Integration pattern:

// From an async context (Tokio), dispatch CPU work to Rayon:
let result = tokio::task::spawn_blocking(move || {
    rayon::scope(|s| {
        // Parallel parsing
        for file in &files {
            s.spawn(|_| {
                let ast = parse_file(file, &source_db, &sink);
                // Store result...
            });
        }
    });
}).await?;
```

### 22.2 Per-Stage Parallelism

| Stage | Parallelism Strategy |
|-------|---------------------|
| Parse | One Rayon task per source file |
| Elaborate | Independent module subtrees elaborated in parallel; shared module registry behind `RwLock` |
| Synthesize | One Rayon task per module (module-level independence) |
| Place (annealing) | Grid partitioned into independent regions; each region optimized on a separate thread |
| Route (PathFinder) | Nets grouped by independence (no shared routing resources); groups routed in parallel |
| Bitstream | Tile-level bitstream assembly parallelized |
| Simulate | Single-threaded (event-driven simulation is inherently sequential; parallelism via test-level sharding) |
| Lint | One Rayon task per module per lint rule |

### 22.3 Thread Safety

- `DiagnosticSink` uses `Mutex<Vec<Diagnostic>>` and `AtomicUsize` for the error counter — safe for concurrent emission from parallel tasks.
- `SourceDb` is populated before parallel stages begin and then shared as `&SourceDb` (immutable reference).
- `Interner` uses `lasso::ThreadedRodeo` for lock-free concurrent interning.
- AionIR `Arena` types are not shared during mutation; each parallel task produces its own arena, which is merged sequentially after the parallel phase.

---

## 23. Testing Strategy

### 23.1 Unit Tests

Every crate has comprehensive unit tests (`#[cfg(test)]` modules) covering:
- Parser: correct AST for valid input, error recovery for invalid input
- Type system: type resolution, width calculation, compatibility checks
- IR: serialization round-trip, graph connectivity invariants
- Synthesis: optimization pass correctness (CSE, constant prop, DCE)
- P&R: placement legality, routing legality, timing calculation correctness

### 23.2 Integration Tests

End-to-end tests in `tests/integration/` exercise the full pipeline:
- `aion build` on reference designs → verify bitstream is produced
- `aion test` on testbench suites → verify simulation results
- `aion lint` on linting test fixtures → verify diagnostics match expected output
- Incremental compilation scenarios → verify correct cache invalidation

### 23.3 Conformance Tests

Parser conformance tests (in `tests/conformance/`) validate against:
- Open-source VHDL test suites
- Open-source Verilog/SystemVerilog test suites (e.g., from the `sv-tests` project)
- Hand-written edge cases for each language feature

### 23.4 Benchmark Tests (Post-MVP)

After the MVP is functional, benchmark tests are added to track:
- Build time regression on reference designs (small/medium/large)
- Incremental build time regression
- Resource utilization quality (LUT count, FF count) vs. vendor tools
- Timing closure quality (achieved frequency) vs. vendor tools
- Memory usage during compilation

Benchmarks run in CI and produce trend reports to detect performance regressions.

---

## 24. Performance Budget

These targets are derived from the PRD (§14) and guide optimization priorities:

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| Small design full build (~5k LUTs) | < 15s | Wall-clock time, 8-core machine |
| Medium design full build (~50k LUTs) | < 2 min | Wall-clock time, 8-core machine |
| Large design full build (~200k LUTs) | < 15 min | Wall-clock time, 8-core machine |
| Incremental (body-only change) | < 30s | Wall-clock time, any design |
| Incremental (interface change) | < 2 min | Wall-clock time, medium design |
| Parse + lint only | < 1s | Wall-clock time, any design |
| Peak memory (medium design) | < 4 GB | RSS measurement |
| LSP response (diagnostics) | < 500ms | Time from keystroke to published diagnostics |
| Simulation throughput | > 1M events/sec | Events per wall-clock second |

### 24.1 Performance-Critical Paths

1. **P&R annealing inner loop:** Must evaluate move cost in < 1μs. This requires O(1) wirelength delta computation (incremental HPWL) and cache-friendly data layout.

2. **Routing A* search:** Must find a path in < 100μs per net on average. Requires efficient priority queue and adjacency list representation for the routing graph.

3. **Parsing:** Must achieve > 10 MB/s of source text throughput. The lexer should be branchless where possible.

4. **Serialization I/O:** `bincode` serialization is fast but I/O can dominate for large designs. Use memory-mapped files for reading cached artifacts where beneficial.

---

## 25. Phased Implementation Guide

This section maps the PRD's roadmap to concrete implementation tasks with crate-level granularity.

### Phase 0 — Foundation (Months 1–4)

**Goal:** Parse all three HDLs and produce useful lint output.

**Crates to implement:**
- `aion_common` — All foundational types
- `aion_source` — Source file management and span tracking
- `aion_diagnostics` — Diagnostic types and terminal renderer
- `aion_config` — `aion.toml` parser (project metadata, basic fields)
- `aion_vhdl_parser` — Full VHDL-2008 parser
- `aion_verilog_parser` — Full Verilog-2005 parser
- `aion_sv_parser` — SystemVerilog-2017 parser (synthesizable subset priority)
- `aion_ir` — Core IR type definitions (needed for lint)
- `aion_elaborate` — Basic elaboration (hierarchy resolution, no full type system yet)
- `aion_lint` — W101-W108, E101-E105, C201-C204 rules
- `aion_cli` — `init`, `lint` commands
- `aion_cache` — Basic content-hash caching for parsed ASTs

**Milestone criteria:**
- All three parsers pass conformance tests on open-source HDL projects
- `aion lint` produces useful diagnostics on real designs
- Parse + lint completes in < 1s on any reasonable project
- Error recovery produces multiple diagnostics per file (no single-error-and-stop)

### Phase 1 — Simulation (Months 4–8)

**Goal:** Run testbenches and produce waveforms.

**Crates to implement:**
- `aion_sim` — Event-driven simulation kernel, 4-state logic, delta cycles
- `aion_sim::waveform` — VCD, FST, GHW output
- `aion_sim::interactive` — Interactive simulation TUI
- `aion_deps` — Git and local-path dependency resolution, `aion.lock`
- `aion_elaborate` — Complete type system, generate block expansion, mixed-language
- `aion_cli` — `test`, `sim`, `update` commands

**Milestone criteria:**
- `aion test` runs standard testbenches and produces correct waveforms
- `aion sim` interactive mode supports run/step/inspect/breakpoint
- Simulation results cross-validated against Icarus Verilog and GHDL on test suites
- Dependencies fetched from Git, lock file generated and reproducible

### Phase 2 — Synthesis (Months 8–14)

**Goal:** Synthesize HDL to technology-mapped netlists.

**Crates to implement:**
- `aion_synth` — Behavioral lowering, optimization passes, technology mapping
- `aion_arch` — Architecture trait definition, initial device models (Cyclone V, Artix-7 stubs)
- `aion_cache` — Module-level incremental synthesis caching
- `aion_report` — Resource utilization reporting
- `aion_cli` — `build` command (synthesis only, no P&R)

**Milestone criteria:**
- `aion build` produces synthesized netlists with resource reports
- LUT mapping produces functionally correct results (verified via simulation of synthesized netlist)
- BRAM and DSP inference works for common patterns
- Module-level incremental synthesis works correctly
- Resource utilization is within 2x of Yosys on reference designs

### Phase 3 — Place & Route (Months 14–22)

**Goal:** End-to-end compilation from HDL to bitstream.

**Crates to implement:**
- `aion_arch` — Full architecture models for Cyclone V, Artix-7 (including routing graphs, timing models)
- `aion_pnr` — Placement (analytical + annealing), routing (PathFinder)
- `aion_timing` — Static timing analysis, SDC/XDC parsing
- `aion_bitstream` — SOF (Intel) and BIT (Xilinx) generation
- `aion_report` — Timing reports, power estimation, floorplan SVG

**Milestone criteria:**
- `aion build` produces bitstreams that successfully program Cyclone V and Artix-7 devices
- Reference designs work correctly on hardware
- Timing analysis matches vendor tools within 10% on reference designs
- Full build meets performance targets for small/medium designs

### Phase 4 — Polish & Ecosystem (Months 22–28)

**Goal:** Production-quality toolchain with IDE integration and broad device support.

**Crates to implement:**
- `aion_lsp` — Full LSP server
- `aion_flash` — USB-Blaster and Digilent JTAG programming
- `aion_lint` — Vendor-specific rules (S401-S404), timing rules (T301-T306)
- `aion_arch` — Expanded device models (MAX 10, Cyclone 10, Kintex-7, Zynq-7000, Spartan-7, Stratix V)
- `aion_diagnostics` — JSON and SARIF renderers
- `extensions/vscode/` — VS Code extension

**Milestone criteria:**
- LSP provides real-time diagnostics, go-to-definition, and autocomplete in VS Code
- `aion flash` programs devices via USB-Blaster and Digilent JTAG
- All launch device families have architecture models
- Full build meets performance targets across all design complexity tiers
- Documentation: user guide, architecture guide, contributor guide published
- Aion v1.0 released

---

## Appendix A: Glossary

| Term | Definition |
|------|-----------|
| AIG | AND-Inverter Graph — canonical representation for Boolean logic optimization |
| ALM | Adaptive Logic Module — Intel's configurable logic block |
| AionIR | Aion Intermediate Representation — the unified IR |
| BEL | Basic Element of Logic — the smallest placeable unit within a site |
| BRAM | Block RAM — dedicated memory blocks in the FPGA fabric |
| CDC | Clock Domain Crossing — a signal crossing between two unrelated clock domains |
| CLB | Configurable Logic Block (Xilinx) |
| CRC | Cyclic Redundancy Check |
| CSE | Common Subexpression Elimination |
| DCE | Dead Code Elimination |
| DFF | D-type Flip-Flop |
| DSP | Digital Signal Processing block |
| FSM | Finite State Machine |
| FST | Fast Signal Trace — compact waveform format (GTKWave) |
| GHW | GHDL Waveform — VHDL-native waveform format |
| HPWL | Half-Perimeter Wirelength — standard wirelength estimation metric |
| IR | Intermediate Representation |
| JTAG | Joint Test Action Group — standard debug/programming interface |
| LAB | Logic Array Block (Intel) |
| LUT | Look-Up Table — the fundamental logic element in an FPGA |
| P&R | Place and Route |
| PIP | Programmable Interconnect Point — a configurable connection in the routing fabric |
| PLL | Phase-Locked Loop — clock management primitive |
| QoR | Quality of Results — measure of synthesis/P&R output quality |
| SARIF | Static Analysis Results Interchange Format |
| SDC | Synopsys Design Constraints — timing constraint format |
| STA | Static Timing Analysis |
| SVF | Serial Vector Format — JTAG programming file format |
| VCD | Value Change Dump — standard waveform format |
| WNS | Worst Negative Slack |
| XDC | Xilinx Design Constraints — Xilinx timing constraint format |

---

## Appendix B: Reference Crate Versions

| Crate | Version | Purpose |
|-------|---------|---------|
| `clap` | 4.x | CLI framework |
| `serde` | 1.x | Serialization framework |
| `bincode` | 2.x | Binary serialization |
| `tokio` | 1.x | Async runtime |
| `rayon` | 1.x | Data parallelism |
| `tower-lsp` | 0.20+ | LSP server framework |
| `petgraph` | 0.6+ | Graph data structures |
| `lasso` | 0.7+ | String interning |
| `xxhash-rust` | 0.8+ | Fast hashing |
| `git2` | 0.19+ | Git operations |
| `rusb` | 0.9+ | USB device access (for JTAG) |
| `toml` | 0.8+ | TOML parsing |
| `thiserror` | 2.x | Error derive macros |

---

*This is a living document. As implementation progresses, sections will be updated to reflect actual decisions, trade-offs encountered, and lessons learned. All contributors are encouraged to propose amendments via the RFC process.*
