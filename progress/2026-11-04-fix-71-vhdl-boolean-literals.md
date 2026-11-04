## 2026-11-04 — Elaborator: VHDL Boolean Literals in Expressions (Fixes #71)

- **Bug fixed:** `assert false report "..." severity failure;` failed with E204 "unknown signal `false`". The testbench end-of-simulation idiom could not elaborate. `lower_vhdl_name` passed every bare name to `resolve_signal`, and that function only knows signals and constants. Constant evaluation of defaults (`vhdl.rs`) already handled `true`/`false`; expression lowering did not.
- **Fix:** the new `vhdl_boolean_literal` helper in `expr.rs` matches `true`/`false` case-insensitively, as enumeration literals of the predefined `boolean`. They lower to a 1-bit `IrExpr::Literal`, which is the same encoding as `LogicVec::from_bool`. A signal or constant that is declared with the same name hides the literal. Names with suffixes still go through normal resolution.
- **Verified:** `assert false` in a VHDL testbench now elaborates, and `aion sim` reports the assertion failure.
- **Tests:**
  - 3 unit tests in `expr.rs`: both literals in mixed case, a signal named `true` that hides the literal, and look-alike names that still get E204;
  - 2 elaboration tests in `lib.rs`: `assert False` / `assert TRUE` lower to constant conditions with no errors, and `assert falsy` still reports E204.
- **Out of scope, still open:** `examples/blinky_vhdl/tests/blinky_tb.vhd` still fails to elaborate, because of `constant CLK_PERIOD : time := 20 ns;` (E210 unknown VHDL type `time`). The simulator ignores VHDL `report` text and `severity` and prints "ASSERTION FAILED: assertion failed" for a `severity note`; that is #12.
