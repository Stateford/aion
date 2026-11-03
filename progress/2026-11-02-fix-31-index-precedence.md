## 2026-11-02 — Verilog/SV Parsers: Index Binding Power and `**` Associativity (Fixes #31)

- **Index expressions (PAR-6):** `parse_postfix_index` now parses the first expression at binding power 0, not 18. Every operator now works inside `[...]`: `x[i << 2]`, `x[i & 3]`, `x[s ? 1 : 0]`, `x[i == 0]`, `x[a && b]` and the rest. The binary-operator loop treats a `+`/`-` directly followed by `:` as a terminator, so `d[8*i+7 -: 8]` and `d[i+1 +: 8]` give the full base expression. `continue_expr_bp` is gone. Lvalues, concatenations and port connections share the same path. Declaration ranges were never capped.
- **Conditional base:** `d[s ? a : b +: 4]` takes the whole conditional as the base. `d[s ? a +: b]` is rejected, because `s ? a` is not an expression.
- **`**` (PAR-14):** now left-associative, per IEEE 1364-2005 §5.1.2 and 1800-2017 Table 11-2: `2 ** 3 ** 2` is 64. Unary operators still bind tighter, so `-2 ** 2` is `(-2) ** 2`.
- **Tests:**
  - a `precedence_matrix` module in each parser's `expr.rs`, which renders the AST as an S-expression and checks it against the LRM table: about 140 Verilog and 155 SV cases, plus 8 rejection cases in each. It covers every binary level pairwise, unary vs `**`, `?:` nesting, every operator inside `[]`, range selects, `+:`/`-:` with arithmetic bases, error cases, and `<=` as a comparison vs a non-blocking assignment;
  - the old `power_right_associative` tests are replaced by left-assoc tests;
  - 2 elaborator `const_eval` tests that run real parser output: `2 ** 3 ** 2 = 64`, `-2 ** 2 = 4`.
- **Known gaps:**
  - SV `->`/`<->` are not parsed as expression operators (`<->` is not lexed).
  - `inside {[1:3]}` value ranges fail to parse.
  - `foreach (arr[i])` fails, because `parse_expr_bp(23)` consumes the `[i]`.
  - Part-select lowering is still `Slice { high: base, low: width }` (#42).
