## 2026-11-04 — Elaborator: VHDL Boolean Literals in Expressions (Fixes #71)

- **Bug fixed:** `assert false report "..." severity failure;` failed with E204 "unknown signal `false`". The testbench end-of-simulation idiom could not elaborate. `lower_vhdl_name` passed every bare name to `resolve_signal`, and that function only knows signals and constants. Constant evaluation of defaults (`vhdl.rs`) already handled `true`/`false`; expression lowering did not.
- **Fix:** one shared helper, `const_eval::vhdl_boolean_literal`, matches `true`/`false` case-insensitively, as enumeration literals of the predefined `boolean`. Three paths use it, and each one looks up declared names first, so a signal, constant or generic with the same name hides the literal:
  - expression lowering (`lower_vhdl_name`), where the literal becomes a 1-bit `IrExpr::Literal`. The new `lookup_name` does the signal/constant lookup once, and `resolve_signal` reuses it;
  - constant evaluation (`eval_vhdl_expr`), which now returns `ConstValue::Bool`. Before, `generic (EN : boolean := true)` and `generic map (EN => false)` failed with E209;
  - object default evaluation (`eval_vhdl_default`), which used to have its own copy of the match.
- Names with suffixes still go through normal resolution.
- **Verified:** `assert false` in a VHDL testbench now elaborates, and `aion sim` reports the assertion failure.
- **Tests:**
  - 4 unit tests in `expr.rs`: both literals in mixed case, a signal named `true` and a constant named `false` that hide the literal, and look-alike names that still get E204;
  - 3 tests in `const_eval.rs`: the helper itself, evaluating the literals, and a declared name that hides one;
  - 4 elaboration tests in `lib.rs`:
    - `assert False` / `assert TRUE` lower to constant conditions;
    - `assert falsy` still reports E204;
    - boolean generic defaults and `generic map` values elaborate with no errors;
    - literals inside `=`, `/=`, `and` and `not` alongside `boolean` signals elaborate, and both the signals and the literals are 1 bit.
- **Out of scope, still open:** `examples/blinky_vhdl/tests/blinky_tb.vhd` still fails to elaborate, because of `constant CLK_PERIOD : time := 20 ns;` (E210 unknown VHDL type `time`). The simulator ignores VHDL `report` text and `severity` and prints "ASSERTION FAILED: assertion failed" for a `severity note`; that is #12.
