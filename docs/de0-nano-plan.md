# Plan — SystemVerilog → Bitstream → DE0-Nano Programming

**Date:** 2026-06-09
**Target board:** Terasic DE0-Nano (Intel Cyclone IV E, `EP4CE22F17C6`)
**Goal:** Compile a SystemVerilog design with Aion and program the FPGA from the
bitstream Aion produces.

> Status: planning document only. No implementation has been started. This
> captures the analysis and recommended approach so it survives across sessions.

---

## 1. Current State (verified)

`aion build` already runs the full frontend-to-bitstream pipeline. Running it on
`examples/blinky_soc` (which targets this exact board) succeeds:

```
Target de0_nano (EP4CE22F17C6)
Synthesized: 23 LUTs, 12 FFs, 0 BRAM, 0 DSP, 12 IO
Placed and routed
Timing met (worst slack: 0.000 ns)
Generated build/de0_nano/blinky_soc.sof (866 bytes)
```

Two gaps stand between that output and a blinking LED:

1. **The bitstream is not hardware-accurate.** Three backend layers are
   placeholders, all acknowledged in `PROGRESS.md`'s Phase 3 checklist:
   - Placement uses synthetic site IDs from resource counts, not the real
     device grid (`crates/aion_pnr/src/placement/random.rs`).
   - Routing is entirely stubbed — the `Architecture` routing-graph methods
     default to empty, so every net gets `RouteTree::stub()`
     (`crates/aion_pnr/src/routing/pathfinder.rs`). This is the source of the
     `S502` "stub routing" warnings.
   - The config-bit databases are placeholders — the generators hardcode
     `SimplifiedIntelDb` / `SimplifiedXilinxDb`
     (`crates/aion_bitstream/src/xilinx/mod.rs`), emitting deterministic but
     fake bit positions. Telling detail: the generated `.sof` is 866 bytes; a
     real `EP4CE22` `.sof` is roughly 700 KB.
2. **Device programming does not exist.** There is no `aion_flash` crate, no
   `aion flash` command, and no USB dependency, despite both being listed in
   `CLAUDE.md` and fully designed in the technical spec (§16).

### Board-specific reality

- **Programming is easy.** The DE0-Nano has an on-board USB-Blaster, and
  [openFPGALoader](https://trabucayre.github.io/openFPGALoader/vendors/intel.html)
  supports it directly (`openFPGALoader -b de0nano design.rbf`), with Cyclone IV
  programming confirmed working in the community.
- **Native bitstreams are hard.** Cyclone IV has **no complete open bitstream
  database**. The only public efforts are early-stage: Siroco (early Cyclone IV
  RE) and Cyclone_CRAM_Mapper (CRAM address mapping, but for the smaller
  `EP4CE6`). Mistral covers Cyclone V only. Unlike Xilinx 7-series
  (Project X-Ray), there is no ready-made data source for this part.

This splits the work into a fast track that gets the LED blinking and a long
research track toward a fully-open native backend.

---

## 2. Phase 1 — `aion flash` command (≈2–4 days)

Get device programming working by delegating to openFPGALoader.

- **New crate `crates/aion_flash`** following spec §16: the `JtagProgrammer`
  trait (`detect_devices` / `program` / `verify`), `JtagDevice` with IDCODE
  (`EP4CE22` = `0x020F30DD`), and a `FlashError` enum via `thiserror`. First
  backend is a subprocess wrapper around `openFPGALoader`: pure argument-builder
  functions (unit-testable without hardware), a `--detect` output parser, and
  stderr/exit-code mapping into Aion diagnostics.
- **New `aion flash` CLI command** with `--target`, `--file`, `--board`
  (`de0nano`), `--cable` (default `usb-blaster`), `--detect`, `--loader-path`.
  Reuses `resolve_build_target` / `determine_build_dir` from `build.rs`,
  defaults to `build/<target>/<project>.rbf`, and gives an actionable error
  ("run `aion build --format rbf`") if the artifact is missing. RBF is the right
  format — Aion already emits it, and its uncompressed output matches the
  JTAG-mode requirement (no on-chip decompression).
- **Tests:** arg-builder and detect-parser unit tests, a fake-executable
  integration test (temp script standing in for openFPGALoader), CLI parse
  tests, missing-file / missing-loader error paths.
- **Hardware validation (manual, requires the board):** program a
  *Quartus-generated* blinky RBF first. This proves the entire programming path
  independent of Aion's bitstream fidelity.

**Exit criteria:** `aion flash --detect` sees the board; a known-good RBF
programs and blinks.

---

## 3. Phase 2 — Hybrid Quartus backend (≈1–2 weeks)

Make the whole loop real on Cyclone IV now, since no open bitstream database
exists for this part.

- **`aion build --backend quartus`** (flag + `backend` key in `aion.toml`):
  Aion keeps its frontend — parse, lint, elaborate, and refuse to proceed on
  errors (fast feedback) — then:
  - emits a `.qsf` (the `aion.toml` pin map translates 1:1 to
    `set_location_assignment` / IO-standard assignments, plus
    `GENERATE_RBF_FILE ON` and `ON_CHIP_BITSTREAM_DECOMPRESSION OFF`),
  - emits an `.sdc` from the `[clocks]` section,
  - invokes `quartus_map` → `quartus_fit` → `quartus_asm` (Quartus Prime Lite,
    free, supports `EP4CE22`),
  - parses Quartus's log into Aion diagnostics,
  - drops the `.rbf` into `build/<target>/`.
- **Tests:** QSF/SDC golden-file tests, runner arg construction, missing-Quartus
  error path; real-Quartus integration tests gated behind an env var (same
  pattern `aion_xray` uses for its database).

**Exit criteria:** edit `.sv` → `aion build --backend quartus` → `aion flash` →
LED blinks. This is the literal original request, working end to end.

---

## 4. Phase 3 — Native USB-Blaster driver (≈2–3 weeks, optional)

Replace the subprocess with the spec's `rusb`-based driver: USB-Blaster
byte-shift protocol (VID `0x09FB` / PID `0x6001`), a JTAG TAP state machine,
IDCODE chain scan, and the Cyclone IV JTAG configuration sequence
(`JTAG_PROGRAM`, shift RBF, `CHECK_STATUS`, `JTAG_STARTUP`). Protocol logic
unit-tests against a mocked USB transport; final debugging is
hardware-in-the-loop (not CI-testable). Worth doing for a zero-external-tools
toolchain, but functionally equivalent to openFPGALoader delegation.

---

## 5. Phase 4 — Native hardware-accurate bitstreams (research track, months)

The long-term open-toolchain goal, mirroring what `aion_xray` already does for
Artix-7:

1. **Plumbing first (small, do early):** make `aion_bitstream::create_generator`
   accept an injected `ConfigBitDatabase` instead of hardcoding the
   `Simplified*` placeholders. This also unblocks the already-written
   `XRayConfigBitDb` for Xilinx.
2. **`EP4CE22` CRAM database:** Quartus-as-oracle fuzzing (generate minimal
   designs, diff RBFs) to map config bits, bootstrapping from
   Cyclone_CRAM_Mapper (`EP4CE6`) and Siroco. Deliverable: an `aion_cyclone_db`
   crate.
3. **Real device model:** `EP4CE22` tile grid + routing graph in `aion_arch`,
   replacing the stub defaults.
4. **PnR upgrade:** real placement onto LAB/IOE sites (replacing synthetic site
   IDs) and PathFinder on the real graph — the algorithms already exist; they
   currently fall back to stubs.

### On reverse-engineering tooling (Ghidra)

Ghidra is the **wrong primary tool** for the config-bit database:

- It reverse-engineers *code*; the database is a *data-format* problem. The
  proven method (X-Ray, Mistral, prjtrellis, Cyclone_CRAM_Mapper) is black-box
  fuzzing — diff vendor-generated bitstreams, correlate features to flipped
  bits.
- Even decompiling Quartus would not yield the bit positions: `quartus_asm`
  loads large per-device data tables at runtime, so the code gives you the
  algorithm but not the mappings.
- **Licensing:** Quartus's EULA prohibits reverse engineering. X-Ray and Mistral
  use black-box fuzzing specifically to keep their databases clean-room and
  legally shareable. Decompiling Quartus would taint the result — unusable for
  an open project.

Ghidra has only a narrow, legitimate supporting role (file-container framing,
CRC, JTAG protocol) — and for the DE0-Nano those are already solved by
openFPGALoader and Aion's existing RBF writer, so it buys little here.

---

## 6. Recommendation

Do **Phase 1 → Phase 2**: roughly two weeks of work to a real edit-build-flash
loop on the DE0-Nano, with all of Aion's UX intact. Phase 3 is polish; Phase 4
is genuinely uncertain (Cyclone IV RE is early-stage) — treat it as exploratory,
or pick up an Artix-7 board later, where the open data (Project X-Ray) already
exists.

Standing constraint: every hardware-touching step requires the physical board.
Everything else (crate code, arg builders, parsers, golden-file tests) is
build- and unit-testable without hardware; only final programming and bring-up
are manual.
