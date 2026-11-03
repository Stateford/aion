## 2026-11-02 — Elaborator: Unary Plus Lowered as Identity

- **Silent miscompile fixed:** Verilog/SV unary `+` and VHDL unary `+` (`Pos`) were lowered to `UnaryOp::Neg`, so `assign y = +a;` built `y = -a` with no diagnostic. `+(-a)` became `Neg(Neg(a))`. The `map_*_unary_op` helpers now return `Option<UnaryOp>`, with `None` for unary plus, and `lower_{verilog,sv,vhdl}_expr` return the lowered operand directly. No IR variant was added.
- **Const eval:** `eval_{verilog,sv,vhdl}_expr` now evaluate unary plus as an identity. Before, `parameter P = +3` failed with "non-constant expression" (E209). A shared `unary_plus_value` helper keeps a `Real` operand real, so `+R` with `R = 3.5` stays 3.5 instead of truncating to 3. This came from review on #66. Unary minus still truncates reals; that is pre-existing.
- **Checked, not affected:** lint, sim and synth only see IR `UnaryOp`, which has no plus variant.
- **Tests:**
  - 12 elaborated-IR tests (`+a`, `-a`, `+(-a)`, `-(+a)` for Verilog, SV and VHDL) and a mapping test;
  - 8 const-eval tests (`+3`, `-3`, `+-3`, `-+3` per language, unary plus of a non-constant, real-valued `+R` in all three languages, and the helper itself);
  - 4 end-to-end sim tests in `aion_conformance/tests/sim_assign_semantics.rs`.
- **Known gaps (unchanged, out of scope):** other unary mappings are still approximate. `RedNand`/`RedNor`/`RedXnor` drop the inversion, VHDL `abs` and SV `++`/`--` lower to `Neg`, and VHDL `??` lowers to `Not`. Body-level `parameter`/`localparam` declarations are not entered into the const env, and parameters are not resolvable as expression operands (E204).
