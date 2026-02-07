# Aion — Product Requirements Document

**The modern FPGA toolchain.**

**Version:** 1.0 Draft
**Date:** February 7, 2026
**License:** Open Source (MIT/Apache 2.0 dual-license)
**Implementation Language:** Rust

---

## 1. Executive Summary

Aion is an open-source, Rust-based HDL compiler toolchain that replaces the fragmented, slow, and hostile developer experience of existing FPGA tools (Quartus, Vivado) with a fast, unified, Cargo-inspired workflow. Aion provides a complete pipeline — from parsing and linting through synthesis, place-and-route, and device programming — controlled by a single `aion.toml` configuration file and a small set of intuitive CLI commands.

Aion targets both Intel/Altera and Xilinx/AMD FPGA families with a fully custom synthesis and place-and-route engine, module-level incremental compilation, and aggressive parallelism to deliver build times that consistently beat Quartus and Vivado on equivalent designs.

### Target Audience

Aion serves two primary audiences:

- **Hobbyists and indie FPGA developers** frustrated by multi-gigabyte IDE installs, opaque error messages, and 10-minute compile cycles for trivial designs. Aion gives them a lightweight, scriptable tool that feels like a modern software development workflow.
- **Professional FPGA teams** who need CI/CD-friendly tooling, reproducible builds, structured output for automation, and a compiler that doesn't hold their pipeline hostage. Aion integrates cleanly into existing DevOps workflows and version control practices.

---

## 2. HDL Language Support

Aion ships with first-class parsers and elaboration for three HDL languages:

| Language | Standard | Scope |
|---|---|---|
| VHDL | VHDL-2008 (with VHDL-2019 stretch goal) | Full synthesis + simulation subset |
| Verilog | IEEE 1364-2005 | Full synthesis + simulation subset |
| SystemVerilog | IEEE 1800-2017 | Synthesizable subset + testbench constructs |

All three languages are supported in the same project. Mixed-language designs (e.g., a VHDL top-level instantiating Verilog submodules) are a first-class use case. The parser and elaboration stages are built from scratch in Rust — no dependency on external tools like GHDL or Verilator.

---

## 3. Vendor and Device Support

### 3.1 Launch Targets

Aion ships with architecture support for:

- **Intel/Altera:** Cyclone V, Cyclone 10 LP, MAX 10, Stratix V (initial family set)
- **Xilinx/AMD:** Artix-7, Kintex-7, Spartan-7, Zynq-7000 (initial family set)

Each device family requires a dedicated architecture model for place-and-route, timing analysis, and bitstream generation. Additional families are added incrementally post-launch.

### 3.2 Output Formats

| Format | Vendor | Description |
|---|---|---|
| `.sof` | Intel | SRAM Object File (volatile JTAG programming) |
| `.pof` | Intel | Programmer Object File (non-volatile flash) |
| `.rbf` | Intel | Raw Binary File (passive serial config) |
| `.bit` | Xilinx | Bitstream file (JTAG/SPI programming) |

---

## 4. Project Structure and Configuration

### 4.1 Project Layout

`aion init` scaffolds the following directory structure:

```
my-design/
├── aion.toml           # Project manifest and configuration
├── aion.lock           # Dependency lock file (auto-generated)
├── src/                # RTL source files (.vhd, .v, .sv)
│   └── top.sv
├── tests/              # Testbench files
│   └── top_tb.sv
├── benches/            # Performance/resource benchmarks
├── ip/                 # Local IP cores and vendored dependencies
├── constraints/        # SDC/XDC timing and physical constraint files
│   └── timing.sdc
└── out/                # Build output directory (gitignored)
    ├── cyclone5/       # Per-target output subdirectories
    │   ├── top.sof
    │   └── reports/
    └── artix7/
        ├── top.bit
        └── reports/
```

The `out/` directory is organized by target device when multiple targets are configured. Each target gets its own subdirectory containing bitstreams, reports, and intermediate artifacts. The design explicitly leaves room for a future workspace/monorepo model where multiple sub-designs can share an `ip/` directory and coordinate builds.

### 4.2 Configuration (`aion.toml`)

All project configuration lives in a single TOML file. The design philosophy is: **common things should be simple, complex things should be possible.**

```toml
[project]
name = "blinky"
version = "0.1.0"
description = "LED blink example for learning Aion"
authors = ["Ada Lovelace <ada@example.com>"]
top = "src/top.sv"
license = "MIT"

# ──────────────────────────────────────────────
# Device targets
# ──────────────────────────────────────────────

[targets.cyclone5]
device = "5CSEMA5F31C6"         # Full part number
family = "cyclone5"

[targets.artix7]
device = "xc7a35tcpg236-1"
family = "artix7"

# ──────────────────────────────────────────────
# Pin assignments — global or per-target
# ──────────────────────────────────────────────

# Global pins (applied to all targets unless overridden)
[pins]
clk     = { pin = "PIN_AF14", io_standard = "3.3-V LVTTL" }
reset_n = { pin = "PIN_AA14", io_standard = "3.3-V LVTTL" }

# Target-specific pin overrides
[targets.cyclone5.pins]
led0 = { pin = "PIN_V16", io_standard = "3.3-V LVTTL" }
led1 = { pin = "PIN_W16", io_standard = "3.3-V LVTTL" }

[targets.artix7.pins]
led0 = { pin = "H17", io_standard = "LVCMOS33" }
led1 = { pin = "K15", io_standard = "LVCMOS33" }

# ──────────────────────────────────────────────
# Constraints
# ──────────────────────────────────────────────

[constraints]
timing = ["constraints/timing.sdc"]      # SDC/XDC files

[targets.artix7.constraints]
timing = ["constraints/artix7_timing.xdc"]  # Target-specific override

# ──────────────────────────────────────────────
# Clock definitions
# ──────────────────────────────────────────────

[clocks]
clk = { frequency = "50MHz", port = "clk" }

# ──────────────────────────────────────────────
# Dependencies / IP management
# ──────────────────────────────────────────────

[dependencies]
uart_core = { git = "https://github.com/example/uart-ip.git", tag = "v2.1.0" }
spi_master = { path = "../shared-ip/spi" }
# Future registry support:
# fifo_async = { version = "1.3" }

# ──────────────────────────────────────────────
# Build settings
# ──────────────────────────────────────────────

[build]
optimization = "area"          # "area" | "speed" | "balanced"
target_frequency = "100MHz"    # Target clock frequency for timing closure

[test]
waveform_format = "fst"        # "vcd" | "fst" | "ghw"
```

### 4.3 Lock File (`aion.lock`)

Aion generates and maintains an `aion.lock` file that pins exact dependency versions, Git commit hashes, and content checksums. This file should be committed to version control to ensure reproducible builds across machines and CI environments. Running `aion build` with a lock file present will use pinned versions; `aion update` refreshes the lock file to the latest compatible versions.

---

## 5. CLI Interface

Aion's CLI follows Cargo conventions: short, memorable commands with sensible defaults and rich `--help` output.

### 5.1 Command Reference

#### `aion init [name]`
Scaffold a new project.

```
$ aion init blinky
  Creating new Aion project `blinky`
      src/top.sv
      tests/top_tb.sv
      constraints/
      ip/
      aion.toml
  Done. Run `cd blinky && aion build` to compile.
```

**Flags:**
- `--lang <vhdl|verilog|systemverilog>` — Template language (default: SystemVerilog)
- `--target <device>` — Pre-fill device in `aion.toml`

#### `aion build`
Compile the design through the full pipeline: parse → elaborate → synthesize → place & route → generate bitstream.

```
$ aion build
   Compiling blinky v0.1.0 (target: cyclone5)
    Parsing  src/top.sv ........................... ok  [12ms]
    Parsing  ip/uart_core/src/uart.v .............. ok  [8ms]
 Elaborating  top ................................... ok  [45ms]
Synthesizing  top ................................... ok  [1.2s]
Place+Route  ....................................... ok  [3.8s]
   Bitstream  out/cyclone5/blinky.sof .............. ok  [0.4s]

   Resource Usage:
     ALMs:    1,204 / 32,070  ( 3.8%)
     Regs:    1,891 / 64,140  ( 2.9%)
     BRAM:       12 /    397  ( 3.0%)
     DSP:         0 /     87  ( 0.0%)

   Timing:  108.4 MHz achieved  (target: 100 MHz) ✓  slack: +1.2ns

   Finished in 5.5s
```

**Flags:**
- `--target <name>` — Build for a specific target (default: all targets)
- `--release` — Maximum optimization effort (slower build, better QoR)
- `--output-format <sof|pof|rbf|bit>` — Override output format
- `--jobs <n>` — Override thread count (default: all cores)
- `--report-format <text|json|sarif>` — Output format for reports
- `--timing-report` — Emit detailed timing report (critical paths, slack histogram)

#### `aion test [name]`
Run testbenches through the built-in simulator.

```
$ aion test
   Compiling testbench top_tb
   Simulating top_tb ................................ PASS  [0.8s]
   Simulating uart_tb ............................... PASS  [1.2s]
   Simulating spi_edge_cases ........................ FAIL  [0.3s]

   Failures:
     spi_edge_cases:
       assertion failed at spi_tb.sv:142
         expected: 8'hFF
         received: 8'h00
         time: 1450ns

   Waveforms written to out/test/spi_edge_cases.fst

   Result: 2 passed, 1 failed (3 total)  [2.3s]
```

**Flags:**
- `--filter <pattern>` — Run only matching testbenches
- `--waveform <vcd|fst|ghw>` — Override waveform format
- `--no-waveform` — Skip waveform generation for faster runs
- `--verbose` — Show simulation stdout/stderr

#### `aion sim [testbench]`
Launch an interactive simulation session. Opens a TUI with signal inspection, breakpoints, and step-through capability. Waveform files are written on exit.

```
$ aion sim top_tb
   Compiling testbench top_tb
   Launching interactive simulation...

   [Aion Sim] Type 'help' for commands
   > run 100ns
   > inspect clk, reset_n, led0
   > breakpoint top_tb.sv:87
   > continue
   > dump out/sim/debug.fst
   > quit
```

**Flags:**
- `--time <duration>` — Auto-run for specified duration then pause
- `--waveform <vcd|fst|ghw>` — Waveform format for dumps

#### `aion lint`
Static analysis and design-rule checking. Modeled after `cargo clippy` — categorized warnings with explanations and fix suggestions.

```
$ aion lint
   Checking blinky v0.1.0

   warning[W201]: unused signal `debug_bus`
     --> src/top.sv:45:14
      |
   45 |   logic [7:0] debug_bus;
      |               ^^^^^^^^^ declared but never read or driven
      |
      = help: remove the signal or prefix with `_` to suppress

   warning[W305]: potential clock domain crossing
     --> src/top.sv:78:5
      |
   78 |   assign sync_data = async_input;
      |          ^^^^^^^^^ `async_input` is in domain `clk_fast`,
      |                     `sync_data` is in domain `clk_slow`
      |
      = help: use a synchronizer chain or CDC primitive

   error[E102]: non-synthesizable construct
     --> src/uart.v:23:3
      |
   23 |   initial begin
      |   ^^^^^^^ `initial` blocks are not synthesizable
      |
      = note: move to a testbench or guard with `synthesis translate_off`

   warning[S401]: inefficient RAM inference on Cyclone V
     --> src/fifo.sv:56:3
      |
   56 |   reg [7:0] mem [0:255];
      |             ^^^ single-port pattern; Cyclone V M10K requires
      |                 registered output for timing closure
      |
      = help: add an output register stage

   Result: 1 error, 3 warnings
```

**Lint Categories:**
- **Wxxx** — General warnings (unused signals, width mismatches, missing resets, undriven nets)
- **Exxx** — Errors (non-synthesizable constructs, syntax errors, elaboration failures)
- **Cxxx** — Convention (naming violations, coding style, documentation)
- **Sxxx** — Vendor-specific (inefficient patterns for target architecture, sub-optimal inference)
- **Txxx** — Timing (combinational loops, long logic chains, CDC violations)

**Flags:**
- `--fix` — Auto-fix simple warnings where possible
- `--allow <code>` / `--deny <code>` — Override severity
- `--report-format <text|json|sarif>` — Output format
- `--target <name>` — Enable vendor-specific lint rules for a target

#### `aion flash`
Program a connected FPGA device.

```
$ aion flash
   Detecting devices...
   Found: Intel USB-Blaster II on /dev/ttyUSB0
   Device: 5CSEMA5F31C6 (Cyclone V)

   Flashing out/cyclone5/blinky.sof ................ done  [2.1s]
   Verifying ........................................... ok

   Device programmed successfully.
```

**Flags:**
- `--target <name>` — Select target if multiple are configured
- `--programmer <usb-blaster|digilent|auto>` — Override programmer detection
- `--cable <id>` — Select specific cable for multi-cable setups
- `--verify` / `--no-verify` — Toggle post-program verification (default: on)
- `--format <sof|pof|rbf|bit>` — Override bitstream format

**Supported Programmers:**
- Intel USB-Blaster and USB-Blaster II
- Xilinx-compatible JTAG (Digilent HS2, HS3, Arty on-board)

#### `aion update`
Refresh `aion.lock` to latest compatible dependency versions.

#### `aion clean`
Remove the `out/` build directory and incremental compilation cache.

### 5.2 Global Flags

All commands accept:
- `--quiet` / `-q` — Suppress non-error output
- `--verbose` / `-v` — Increase output detail
- `--color <auto|always|never>` — Color output control
- `--config <path>` — Override `aion.toml` location

---

## 6. Compilation Pipeline

Aion implements a fully custom compilation pipeline in Rust. No vendor tools (Quartus, Vivado, Yosys) are invoked at any stage.

### 6.1 Pipeline Stages

```
Source Files (.vhd, .v, .sv)
        │
        ▼
   ┌─────────┐
   │  Parse   │  Lexing + parsing into language-specific ASTs
   └────┬─────┘  Per-file, fully parallelized
        │
        ▼
   ┌─────────────┐
   │  Elaborate   │  Resolve hierarchy, generics/parameters, generate blocks
   └──────┬───────┘  Produces unified design graph (RTLIL-like IR)
         │
         ▼
   ┌─────────────┐
   │  Synthesize  │  Technology mapping, optimization, inference
   └──────┬───────┘  LUT mapping, BRAM/DSP inference, FSM optimization
         │
         ▼
   ┌─────────────┐
   │ Place+Route  │  Architecture-aware placement and routing
   └──────┬───────┘  Timing-driven, with iterative refinement
         │
         ▼
   ┌─────────────────┐
   │ Timing Analysis  │  STA, critical path extraction, slack calculation
   └──────┬───────────┘
         │
         ▼
   ┌─────────────┐
   │  Bitstream   │  Architecture-specific bitstream generation
   └──────┬───────┘  SOF/POF/RBF (Intel), BIT (Xilinx)
         │
         ▼
   Output files + reports
```

### 6.2 Incremental Compilation

Aion tracks dependencies at the **module level**. When a source file changes, only the affected modules and their dependents are recompiled. The incremental compilation cache is stored in `out/.aion-cache/` and contains:

- Per-module AST hashes
- Elaborated module snapshots
- Synthesized netlists per module
- Dependency graph edges

**Cache invalidation rules:**
- Source file content hash changes → reparse that file's modules
- Module interface changes (ports, parameters) → recompile all instantiators
- Module body-only changes → recompile only that module, re-run P&R
- `aion.toml` constraint/pin changes → re-run P&R only (skip synthesis)
- Device target change → full recompile

Incremental P&R is the hardest stage to make incremental. The initial approach uses region-based re-routing: only nets affected by changed modules are ripped up and rerouted, while stable regions are preserved.

### 6.3 Parallelism

All pipeline stages exploit parallelism by default using all available CPU cores:

- **Parsing:** Each source file is parsed independently on a separate thread.
- **Elaboration:** Independent module subtrees are elaborated in parallel.
- **Synthesis:** Module-level synthesis is parallelized across the design hierarchy.
- **Place & Route:** Partitioned placement uses concurrent solvers; routing uses parallel net processing.
- **Bitstream generation:** Tile-level bitstream assembly is parallelized.

Thread count defaults to the number of logical cores and can be overridden with `--jobs <n>` or the `AION_JOBS` environment variable.

---

## 7. Built-in Simulator

Aion ships its own event-driven HDL simulator for `aion test` and `aion sim`. The simulator is implemented in Rust for performance and tight integration with the rest of the toolchain.

### 7.1 Simulation Features

- Full VHDL, Verilog, and SystemVerilog simulation support (synthesis + testbench subsets)
- Event-driven simulation kernel with delta-cycle accuracy
- 4-state logic (0, 1, X, Z) with strength modeling
- Testbench constructs: `$display`, `$monitor`, `$readmemh`, `$finish`, assertions, `initial`/`always` blocks
- Assertion-based verification: `assert`, `assume`, `cover` with pass/fail reporting
- Mixed-language simulation (VHDL testbench driving Verilog DUT, and vice versa)

### 7.2 Waveform Output

The simulator writes waveform files in the user's configured format:

| Format | Extension | Tool Compatibility |
|---|---|---|
| VCD | `.vcd` | Universal (GTKWave, ModelSim, Vivado, any viewer) |
| FST | `.fst` | GTKWave (fast, compact — recommended default) |
| GHW | `.ghw` | GTKWave (VHDL-native, preserves type info) |

The format is configured via `[test] waveform_format` in `aion.toml` or the `--waveform` flag. Waveform files are written to `out/test/<testbench_name>.<ext>`.

### 7.3 Interactive Simulation (`aion sim`)

The interactive simulator provides a command-line interface for debugging:

- `run <duration>` — Advance simulation time
- `step` — Advance one delta cycle
- `inspect <signal> [...]` — Print current signal values
- `breakpoint <file>:<line>` — Break at a source location
- `watch <signal> [condition]` — Break on signal change or condition
- `continue` — Resume to next breakpoint
- `dump <file>` — Write waveform snapshot to file
- `scope <hierarchy>` — Navigate the design hierarchy
- `quit` — Exit and write final waveform

---

## 8. LSP Server

Aion includes a Language Server Protocol (LSP) implementation for editor integration. The LSP server reuses the compiler frontend (parser + elaborator) to provide real-time feedback.

### 8.1 Supported Features

- **Diagnostics:** Real-time syntax and semantic errors, lint warnings as you type
- **Go to definition:** Navigate to module, signal, and type declarations
- **Find references:** Find all instantiations of a module, all reads/writes of a signal
- **Hover:** Type information, signal widths, parameter values, port directions
- **Autocomplete:** Module names, signal names, port connections, VHDL/Verilog keywords
- **Signature help:** Port lists when instantiating modules
- **Code actions:** Quick fixes for lint warnings (e.g., add `_` prefix to unused signal)
- **Document symbols:** Outline view of modules, signals, processes/always blocks
- **Workspace symbols:** Search across all project files

### 8.2 Editor Support

A VS Code extension (`aion-vscode`) is published alongside the CLI. The extension bundles the LSP client and provides syntax highlighting, snippets, and build task integration. The LSP server itself is editor-agnostic and works with any LSP-compatible editor (Neovim, Emacs, Helix, Zed, Sublime Text, etc.).

---

## 9. Lint Engine

Aion's linter runs as part of `aion lint` and is also integrated into the LSP for real-time feedback. The lint engine operates on the elaborated design graph, giving it full visibility into types, widths, clock domains, and target architecture.

### 9.1 Lint Rule Categories

**General Warnings (Wxxx):**
- W101: Unused signal
- W102: Undriven signal
- W103: Width mismatch in assignment or comparison
- W104: Missing reset for sequential logic
- W105: Incomplete sensitivity list
- W106: Latch inferred (missing else/default)
- W107: Truncation in assignment
- W108: Unreachable code / dead logic

**Errors (Exxx):**
- E101: Syntax error
- E102: Non-synthesizable construct in synthesis context
- E103: Elaboration failure (unresolved parameter, recursive instantiation)
- E104: Multiple drivers on a net
- E105: Port connection mismatch

**Convention (Cxxx):**
- C201: Naming convention violation (configurable patterns)
- C202: Missing module documentation comment
- C203: Magic numbers (unlabeled constants)
- C204: Inconsistent coding style

**Timing and CDC (Txxx):**
- T301: Combinational loop detected
- T302: Long combinational chain (estimated timing risk)
- T305: Clock domain crossing without synchronizer
- T306: Async reset used in synchronous domain

**Vendor-Specific (Sxxx):**
- S401: Inefficient RAM inference for target device
- S402: Sub-optimal DSP usage pattern
- S403: IO standard mismatch warning
- S404: Resource over-utilization estimate

### 9.2 Configuration

Lint rules are configurable in `aion.toml`:

```toml
[lint]
# Override severity
deny = ["W106", "T305"]        # Treat as errors
allow = ["C201"]                # Suppress entirely
warn = ["S401"]                 # Keep as warnings (default)

# Naming conventions
[lint.naming]
module = "snake_case"
signal = "snake_case"
parameter = "UPPER_SNAKE_CASE"
constant = "UPPER_SNAKE_CASE"
```

---

## 10. Post-Build Reports

Every `aion build` generates reports in the `out/<target>/reports/` directory. Reports are available in human-readable text (default terminal output), JSON, and SARIF formats.

### 10.1 Resource Utilization Report

Breaks down FPGA resource usage by category and by module hierarchy:

- **Logic:** ALMs/LUTs, registers/flip-flops
- **Memory:** BRAM (M10K, M20K, Block RAM) utilization by instance
- **DSP:** DSP block usage by instance
- **I/O:** Pin utilization, I/O bank assignments
- **Global resources:** PLLs, clock networks

### 10.2 Timing Report

- Achieved clock frequency vs. target
- Critical path(s) with full hierarchy trace
- Setup and hold slack per clock domain
- Slack histogram
- Inter-clock domain paths

### 10.3 Power Estimation Report

- Static (leakage) power estimate
- Dynamic power estimate by category (logic, routing, I/O, memory, DSP)
- Toggle-rate assumptions and methodology notes
- Thermal design power (TDP) estimate

### 10.4 Floorplan Visualization

A text-based or SVG floorplan showing:

- Module placement regions
- BRAM/DSP column utilization
- I/O bank assignments
- Routing congestion heatmap (SVG mode)

Generated at `out/<target>/reports/floorplan.svg`.

---

## 11. IP and Dependency Management

### 11.1 Dependency Sources

```toml
[dependencies]
# Git repository (tag, branch, or commit)
uart = { git = "https://github.com/example/uart-ip.git", tag = "v2.1.0" }
spi  = { git = "https://github.com/example/spi-ip.git", rev = "a1b2c3d" }

# Local path (for monorepo or development)
fifo = { path = "../shared-ip/fifo" }

# Future: registry (post-v1.0)
# axi_bridge = { version = "^3.0" }
```

### 11.2 Resolution and Lock File

Dependencies are resolved on `aion build` or `aion update`. The resolver:

1. Fetches Git dependencies to a local cache (`~/.aion/cache/`)
2. Resolves version constraints (when registry support is added)
3. Writes exact pinned versions and checksums to `aion.lock`
4. Copies resolved IP into the build environment

The lock file ensures that every developer and CI machine builds with identical IP versions. It should be committed to version control.

### 11.3 IP Project Structure

An IP dependency is itself an Aion project (or a bare directory of HDL files). At minimum, it must contain HDL source files. If it contains an `aion.toml`, Aion reads its `[project]` metadata and any transitive `[dependencies]`.

---

## 12. Error Reporting

Aion's error messages follow the Rust compiler's design philosophy: errors should be **precise**, **actionable**, and **beautiful**.

### 12.1 Human-Readable Output (Default)

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

All errors and warnings include:

- An error code for searchability and suppression
- A precise source location with context snippet
- Arrows pointing to the exact relevant span
- Explanatory notes with links to related declarations
- Actionable `help` suggestions where possible

### 12.2 Structured Output

- `--report-format json` — Machine-readable JSON for CI pipeline consumption
- `--report-format sarif` — SARIF 2.1.0 for GitHub Code Scanning and IDE integration

---

## 13. Flash/Programming Support

### 13.1 Supported Programmers

| Programmer | Protocol | Vendor |
|---|---|---|
| Intel USB-Blaster | JTAG | Intel/Altera |
| Intel USB-Blaster II | JTAG | Intel/Altera |
| Digilent HS2 / HS3 | JTAG | Xilinx/AMD |
| Digilent on-board (Arty, Basys3, etc.) | JTAG | Xilinx/AMD |

### 13.2 Auto-Detection

`aion flash` automatically detects connected programmers and matches them to the configured target device. If multiple devices are connected, the user is prompted to select or can specify with `--cable`.

### 13.3 Programming Flow

1. Detect connected programmers via USB enumeration
2. Identify FPGA device via JTAG ID code
3. Validate bitstream compatibility (device, checksum)
4. Program device (JTAG SVF/XSVF protocol)
5. Verify (read-back and compare, optional)

---

## 14. Performance Targets

Aion's core competitive advantage is speed. The following targets define "faster than Quartus/Vivado":

| Design Complexity | Metric | Quartus/Vivado Baseline | Aion Target |
|---|---|---|---|
| Small (~5k LUTs) | Full build | 60–120s | < 15s |
| Medium (~50k LUTs) | Full build | 5–15 min | < 2 min |
| Large (~200k LUTs) | Full build | 30–90 min | < 15 min |
| Any | Incremental (body change) | 2–10 min | < 30s |
| Any | Incremental (interface change) | 5–15 min | < 2 min |
| Any | Parse + lint only | 10–30s | < 1s |

These targets assume parallel execution on a modern 8+ core machine. Achieving them depends on the efficiency of the custom synthesis and P&R engines — the most technically risky components of the project.

---

## 15. Phased Roadmap

### Phase 0 — Foundation (Months 1–4)

**Goal:** Parse all three HDL languages and produce useful output without synthesis.

- Rust project setup, CI/CD pipeline, contribution guidelines
- VHDL, Verilog, SystemVerilog parsers (full grammar coverage)
- AST → unified intermediate representation (IR)
- `aion init` — project scaffolding
- `aion lint` — syntax checking, basic semantic analysis (unused signals, width mismatches)
- `aion.toml` parser and project model
- Human-readable error output with source spans
- Initial test suite (parser correctness against open-source HDL corpuses)

**Deliverable:** A fast linter that parses real-world HDL projects and produces useful diagnostics.

### Phase 1 — Simulation (Months 4–8)

**Goal:** Run testbenches and produce waveforms.

- Event-driven simulation kernel (4-state logic, delta cycles)
- `aion test` — testbench runner with pass/fail reporting
- `aion sim` — interactive simulation TUI
- Waveform output: VCD, FST, GHW
- Mixed-language simulation support
- Assertion-based verification (`assert`, `$display`, etc.)
- Dependency management: Git and local path sources, `aion.lock`

**Deliverable:** A usable simulator that replaces Icarus/GHDL for testbench workflows.

### Phase 2 — Synthesis (Months 8–14)

**Goal:** Synthesize HDL to technology-mapped netlists.

- Elaboration engine (hierarchy resolution, generics, generate blocks)
- Custom synthesis engine: logic optimization, technology mapping
- BRAM, DSP, and PLL inference
- FSM detection and optimization
- LUT mapping for Intel (ALM) and Xilinx (6-LUT) architectures
- Module-level incremental compilation
- Resource utilization reporting
- `aion build` (synthesis only, no P&R yet)

**Deliverable:** Synthesized netlists with resource reports, comparable to Yosys output quality.

### Phase 3 — Place and Route (Months 14–22)

**Goal:** Full compilation from HDL to bitstream.

- Architecture models for Cyclone V, Artix-7 (initial targets)
- Placement engine (simulated annealing / analytical placement)
- Routing engine (pathfinder-based negotiated congestion routing)
- Timing-driven placement and routing (STA integration)
- SDC/XDC constraint file parsing
- Timing report generation (critical paths, slack)
- Power estimation
- Floorplan visualization (SVG)
- Bitstream generation: SOF (Intel), BIT (Xilinx)
- Additional output formats: POF, RBF

**Deliverable:** End-to-end compilation for Cyclone V and Artix-7. The "v1.0 moment."

### Phase 4 — Polish and Ecosystem (Months 22–28)

**Goal:** Production-quality toolchain with editor integration and device programming.

- LSP server with full feature set
- VS Code extension (`aion-vscode`)
- `aion flash` — USB-Blaster and Digilent JTAG programming
- JSON and SARIF report output
- Expanded device support (MAX 10, Cyclone 10, Kintex-7, Zynq-7000, Spartan-7, Stratix V)
- Performance optimization sprint (hit speed targets vs Quartus/Vivado)
- Vendor-specific lint rules (Sxxx category)
- Documentation: user guide, architecture guide, contributor guide
- Community infrastructure: issue templates, RFC process, plugin API design

**Deliverable:** Aion v1.0 release.

### Future (Post v1.0)

- IP registry (aion.io or similar — "crates.io for FPGA IP")
- Workspace/monorepo support
- Additional vendor support (Lattice ECP5/iCE40, Efinix, Gowin)
- Formal verification integration
- HLS frontend (Rust-to-RTL or similar)
- Cloud compilation service
- Rust-based testbench scripting API
- `aion bench` — resource/timing regression benchmarks

---

## 16. Technical Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Custom P&R quality vs vendor tools | Timing closure failure on complex designs | Start with well-understood architectures (Cyclone V, Artix-7); benchmark aggressively against vendor tools; accept lower QoR initially and iterate |
| Bitstream format reverse engineering | Incorrect bitstreams could damage hardware | Build on Project IceStorm/Trellis/Apicula community work; extensive verification against known-good bitstreams; CRC and readback verification |
| SystemVerilog complexity | Parser/elaboration gaps in edge cases | Prioritize synthesizable subset; build comprehensive test suite from open-source SV projects; accept pragmatic subset coverage for v1.0 |
| Performance targets | May not beat vendor tools on large designs at launch | Module-level parallelism and incremental compilation provide wins even if single-threaded P&R is slower; optimize hot paths post-launch |
| Simulation accuracy | Behavioral mismatches vs. established simulators | Cross-validate against Icarus, GHDL, and Verilator on open-source test suites; prioritize correctness over performance in simulator |

---

## 17. Success Metrics

- **Build speed:** Aion full-build is measurably faster than Quartus/Vivado on reference designs across all complexity tiers
- **Incremental speed:** Module-body-only changes recompile in under 30 seconds for medium designs
- **Correctness:** 100% pass rate on established HDL compliance test suites (parser level); bitstream-level verification against vendor tools on reference designs
- **Adoption:** 1,000+ GitHub stars and 100+ unique users within 6 months of v1.0
- **Ecosystem:** 10+ third-party IP packages available via Git dependency within 12 months of v1.0
