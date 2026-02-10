# Aion

A fast, open-source FPGA toolchain written in Rust. Aion replaces proprietary vendor tools (Quartus, Vivado) with a unified, Cargo-inspired workflow — from HDL parsing and linting through synthesis, place-and-route, and bitstream generation.

## Features

- **Multi-language HDL support** — VHDL-2008, Verilog-2005, SystemVerilog-2017
- **Unified intermediate representation** — All languages elaborate into a single AionIR, enabling cross-language instantiation and shared analysis
- **15 built-in lint rules** — Warnings (unused signals, width mismatches, missing resets), errors (multiple drivers, port mismatches), and conventions (naming, magic numbers)
- **Event-driven simulator** — Delta-cycle-accurate with 4-state logic (0/1/X/Z), delay scheduling, VCD/FST waveform output, and an interactive REPL debugger
- **Synthesis pipeline** — Behavioral lowering, constant propagation, dead code elimination, CSE, and technology mapping to vendor primitives
- **Place & route** — Simulated annealing placement and PathFinder routing with static timing analysis
- **Bitstream generation** — Intel SOF/POF/RBF and Xilinx BIT output formats
- **TUI waveform viewer** — Terminal-based viewer with zoom/scroll, bus expansion, and cursor-time signal values
- **Incremental compilation** — Content-hash caching skips unchanged work across rebuilds
- **Cargo-like UX** — `aion init`, `aion lint`, `aion build`, project scaffolding, `aion.toml` configuration
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

# Lint, simulate, and build
aion lint
aion sim tests/my_tb.sv --time 1us
aion build --target de0_nano
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

### `aion sim <TESTBENCH>`

Runs a single testbench simulation.

```bash
aion sim tests/my_tb.sv                      # Run to completion
aion sim tests/my_tb.sv --time 100ns         # Run for a duration
aion sim tests/my_tb.sv --waveform vcd       # VCD output (default: FST)
aion sim tests/my_tb.sv --interactive        # Launch REPL debugger
```

### `aion test`

Discovers and runs all testbenches in the `tests/` directory.

```bash
aion test                                    # Run all testbenches
aion test --filter uart                      # Filter by name
aion test --no-waveform                      # Skip waveform recording
```

### `aion view <FILE>`

Opens a waveform file in the terminal-based viewer.

```bash
aion view out/my_tb.vcd                      # View VCD waveform
aion view out/my_tb.fst                      # View FST waveform
```

### `aion build`

Runs the full synthesis pipeline: parse, elaborate, synthesize, place & route, timing analysis, and bitstream generation.

```bash
aion build --target de0_nano                 # Build for a target
aion build --target de0_nano -O speed        # Optimize for speed
aion build --target de0_nano --format sof    # Specific output format
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
name = "blinky_soc"
version = "0.1.0"
top = "blinky_top"

[targets.de0_nano]
device = "EP4CE22F17C6"
family = "cyclone4"

[targets.de0_nano.pins]
clk   = { pin = "PIN_R8",  io_standard = "3.3-V LVTTL" }
rst_n = { pin = "PIN_J15", io_standard = "3.3-V LVTTL" }

[targets.de0_nano.pins."leds[0]"]
pin = "PIN_A15"
io_standard = "3.3-V LVTTL"

[targets.de0_nano.pins."leds[1]"]
pin = "PIN_A13"
io_standard = "3.3-V LVTTL"

[clocks.clk]
frequency = "50MHz"
port = "clk"

[build]
optimization = "balanced"
output_formats = ["sof", "pof"]

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
       /|\
      / | \
     /  |  \
[ Lint] | [ Simulate ]  aion_lint, aion_sim
        |
   [ Synthesis ]        aion_synth, aion_arch
        |
   [ Place & Route ]    aion_pnr, aion_timing
        |
   [ Bitstream ]        aion_bitstream
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
| `aion_sim` | Event-driven HDL simulator with VCD/FST output and interactive REPL |
| `aion_tui` | Terminal-based waveform viewer with zoom/scroll and bus expansion |
| `aion_synth` | Synthesis: behavioral lowering, optimization, technology mapping |
| `aion_arch` | Device architecture models (Intel Cyclone IV/V, Xilinx Artix-7) |
| `aion_timing` | Static timing analysis: SDC parsing, propagation, critical path |
| `aion_pnr` | Place & route: simulated annealing placement, PathFinder routing |
| `aion_bitstream` | Bitstream generation: Intel SOF/POF/RBF, Xilinx BIT |
| `aion_cache` | Content-hash-based incremental compilation cache |
| `aion_conformance` | Integration tests: full pipeline across all 3 languages |
| `aion_cli` | CLI entry point: `init`, `lint`, `sim`, `test`, `view`, `build` |

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

## Target Devices

| Vendor | Family | Devices |
|--------|--------|---------|
| Intel/Altera | Cyclone IV E | EP4CE6, EP4CE10, EP4CE22, EP4CE40, EP4CE115 |
| Intel/Altera | Cyclone V | 5CSEMA4, 5CSEMA5, 5CSEBA6 |
| Xilinx/AMD | Artix-7 | XC7A35T, XC7A100T, XC7A200T |

## Building & Testing

```bash
cargo build                                  # Build all crates
cargo test                                   # Run all ~2000 tests
cargo test -p aion_sim                       # Run tests for a specific crate
cargo clippy --all-targets -- -D warnings    # Lint (zero warnings enforced)
cargo fmt --check                            # Check formatting
cargo doc --no-deps                          # Build documentation
```

## Roadmap

| Phase | Focus | Status |
|-------|-------|--------|
| **Phase 0** | Foundation — Parsing, elaboration, linting, caching, CLI | Complete |
| **Phase 1** | Simulation — Event-driven simulator, VCD/FST, TUI, CLI integration | Complete |
| **Phase 2** | Backend — Synthesis, architecture models, PnR, timing, bitstream | Complete |
| **Phase 3** | Polish — LSP, device programming, dependency management, reporting | In Progress |

See [PROGRESS.md](PROGRESS.md) for detailed implementation status.

## Documentation

- [Product Requirements](docs/aion-prd.md) — Vision, scope, and target audience
- [Technical Specification](docs/aion-technical-spec.md) — Data structures, algorithms, and interface contracts

## License

Dual-licensed under [MIT](https://opensource.org/licenses/MIT) and [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0). Choose whichever you prefer.
