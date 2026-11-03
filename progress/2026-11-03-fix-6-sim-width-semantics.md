## 2026-11-03 — Simulator: Width-Correct Value Semantics (Fixes #6)

- **What changed:**
  - New `aion_sim::arith::Bits`: a two-state integer of any width (64-bit
    limbs) with wrapping add/sub/mul/neg, unsigned and signed div/rem,
    shifts, unsigned and signed compare, square-and-multiply `pow`, and
    decimal formatting.
  - The evaluator now sizes expressions per IEEE 1364-2005 5.4/5.5. It
    computes each expression's self-determined width and signedness, then
    evaluates context-determined operands at the context width. An
    assignment is evaluated at `max(target, rhs)` and then fitted to the
    target, so `{cout, sum} = a + b` keeps the carry (SIM-12). A signed RHS
    is sign-extended and an unsigned one zero-extended. Every
    `PendingUpdate` now has exactly its target's width (SIM-5).
  - Arithmetic, shifts, comparisons, `==`/`!=`, reductions, case matching
    and `%d`/`%h` work at any width (SIM-9). `==` follows IEEE: a known
    mismatching bit gives 0 even when other bits are X. Case items are sized
    to the widest item and compared exactly (X matches X). `%d`/`%h` print
    `x`/`X`/`z`/`Z` per IEEE 17.1.1.3, and a signed `%d` prints negative
    values.
  - `**` uses wrapping modular exponentiation with the full exponent, and
    applies the IEEE Table 5-6 rules to a negative signed exponent (SIM-6).
  - `x / 0` and `x % 0` are all-X at the right width instead of aborting
    the run (SIM-7).
  - A bit-select returns the bit unchanged, X/Z included. An out-of-range,
    X/Z, or >= 2^32 index gives X, because the index is compared as a full
    integer before narrowing (SIM-8).
  - Conditions are true when any bit is 1 (it used to be the LSB only).
    `!`, `&&` and `||` use the same truth value. A ternary with an
    ambiguous condition merges the two branches bit by bit.
  - Signedness comes from signal declarations
    (`SimSignalState::signed`, `with_signed`): `reg signed`, `int`, and
    `integer`. `integer` signals are now 32 bits wide (they used to be 1).
    Signedness drives sign extension, signed `<`/`>`, signed `/`/`%`, and
    `%d`.
  - Kernel: `step_delta` captures `previous_value` once per signal per
    delta and fits each event to the signal's width (no stale upper bits
    from a narrow `schedule_event`). Edge detection follows the IEEE edge
    table: 0->x/z, x/z->1 posedge; 1->x/z, x/z->0 negedge (SIM-10).
  - Out-of-range part-select writes still do not panic and ignore the bits
    outside the signal (IEEE). They are now reported once per
    signal/range in the new `SimResult::warnings` /
    `SimKernel::take_warnings()`, and `aion sim` prints them.
  - `Bool`/`Int` initial values are fitted to the declared width (an `Int`
    is sign-extended). The VCD/FST writers pad a too-narrow value with `x`
    instead of panicking.
  - `SimError::DivisionByZero` is kept for API compatibility, but the
    evaluator no longer returns it.
- **Tests:** 12 `arith` unit tests. 30 evaluator tests in
  `evaluator::tests::width_semantics`, covering every repro from the issue
  plus signed/edge/error cases. 10 kernel tests (the IEEE edge table, x->1
  posedge, a once-per-delta previous value, narrow and wide events,
  out-of-range write warnings, integer width). VCD/FST narrow-value tests.
  New end-to-end suite `crates/aion_conformance/tests/sim_width_semantics.rs`
  (10 tests, source -> elaborate -> simulate). 7 of its 10 tests fail on
  the previous commit; the SIM-5/SIM-12 narrow-assignment repros were
  already fixed there by `apply_write`.
- **Known gaps / decisions:**
  - The IR has no arithmetic-shift operator: the elaborator maps `>>>` to
    `BinaryOp::Shr`, so `>>>` on a signed value is still a logical shift.
    Expression `ty` fields are placeholders, so signedness comes from signal
    declarations only. Literals and parameters are treated as unsigned, so
    `a_signed < 0` compares unsigned, as it did before.
  - These elaborator gaps were found while writing the end-to-end tests and
    left out of scope: it drops Verilog `reg x = v;` declaration
    initialisers, lowers Verilog literals through `i64` (so literals wider
    than 64 bits, and X/Z literals, are wrong), and drops `casez`/`casex`
    (lowered as plain `case`).
  - A part-select with X/Z bounds is still an evaluation error, because its
    width is unknown.
- **Phase status:** Phase 3 (Bitstream & Integration) is current; issue #6
  is fully addressed in `aion_sim`, apart from the `>>>` gap above.

### Review follow-up

- **Signed literals.** The IR could not tell `8'sd2` or an unsized `5`
  from `8'd2`, so the simulator typed every literal unsigned. One literal
  in an expression made it unsigned: `i < 0` with `integer i = -1` gave 0,
  and `w16 = s + 1` zero-extended `s`.
  - `aion_ir`: new `Expr::SignedLiteral { value, sized }`, added as the last
    variant so the serialized tags of the other variants do not change.
    `value` holds the same bits `Literal` would.
  - `aion_elaborate` (`lower_verilog_literal` only): unsized decimals and
    `'s` based literals lower to `SignedLiteral`. Every other literal stays
    `Literal`.
  - `aion_sim`: a sized signed literal is signed at its own width. An unsized
    one is a 32-bit signed integer (IEEE 1364-2005 3.5.1): it is
    zero-extended first, so `1` is +1. A magnitude wider than 32 bits gets
    one extra bit.
  - `aion_lint` and `aion_synth` treat `SignedLiteral` exactly like
    `Literal`, so their behaviour does not change.
- **`$display` specifiers.** `format_display` now parses
  `%[-][width]<conv>` for `d h x o b t c s m %`. `%0d` used to print `%0d`
  and swallow its argument.
  - No width keeps the old output. `%0` prints the minimum digits, which
    also trims `%0b`.
  - An explicit width pads decimal, time and text with spaces and
    `b`/`o`/`h` with zeros. `-` left-justifies.
  - A huge width is capped at 64 Ki characters.
  - `%m` takes no argument and prints `%m`, because the evaluator does not
    know the scope.
- **Tests:**
  - Evaluator: 7 new unit tests (4 for display, 3 for signed literals).
  - `aion_conformance`: 3 end-to-end tests. Two of them (the review repros)
    fail on the previous commit.
  - `aion_elaborate`: lowering tests for signed and unsigned literal tokens.
  - `aion_ir`: serde round-trip tests for the new variant.
  - Lint (`expr_width`, C203, W108), synthesis and kernel tests for the new
    variant.
- **Still open:**
  - Parameters (`NamedConst`) are still unsigned. The elaborator does not
    record parameter signedness.
  - Unsized based literals (`'hFF`) are unsigned, which is correct. The
    simulator still sizes them at their minimal width, not 32 bits.
  - X/Z literals still lower to 0 in the elaborator.
