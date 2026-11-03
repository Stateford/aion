## 2026-11-02 — Verilog/SV Parser: Member Selects, `'0/'1`, Assignment Patterns, Postfix `++` (Part of #30)

- **Member selects (PAR-2):** new `Expr::Member { base, member, span }` in both parsers, built for any non-name base (`arr[2].valid`, `gen_blk[0].sig`, `inst[0].port`, `f(x).y`), in expressions and lvalues, and chainable (`a[1].b[2].c`). Previously `.ident` was dropped silently. The elaborator can't resolve these yet (no struct types or generate scopes), so it reports **E210** and never falls back to the base.
- **Fill literals and patterns (PAR-10):**
  - The SV lexer has a new `UnbasedUnsizedLiteral` token (`'0 '1 'x 'X 'z 'Z`). The parser builds `Expr::UnbasedUnsized { value: FillValue }`.
  - `'{...}` parses into `Expr::AssignmentPattern { items: Vec<PatternItem> }` (positional, keyed, `default`). Mixing kinds, a duplicate `default`, an empty `'{}`, and `'q`/`'10` are errors with recovery. `type'(x)`/`N'(x)` casts are unchanged.
  - **Elaborator:** assignment values are sized from the target via the new `WidthLookup` (`lower_sv_assign_value`, `lower_sv_stmt_sized`). This applies through `?:` branches and parens, for always/initial/continuous assigns. `q <= '1` on `logic [7:0]` gives `8'hFF`, and `'x`/`'z` fill every bit. `'0` with no context lowers to a 1-bit zero, which is correct. `'1`/`'x`/`'z` with no known width (comparisons, unpacked arrays, ports) get E210.
  - `'{default: c}` and constant positional patterns are lowered for packed vectors. A wrong item count gets **E205**. Keyed or non-constant items and unpacked/struct targets get E210.
- **Postfix `++`/`--` (PAR-11):** new `UnaryOp::PostIncr/PostDecr`, plus a postfix binding power that is checked against `min_bp`. `++`/`--` on a non-variable is a parse error. The elaborator no longer lowers `++`/`--` to `Neg`. Inside a blocking/non-blocking RHS they are hoisted into statements: prefix before the assignment, postfix after (`a = b++` → `{a = b; b = b + 1}`). Anywhere else (under `?:`/`&&`/`||`, conditions, continuous assigns) they get E210. Statement `i++;` and for-loop steps are unchanged.
- **Tests:** 4 lexer, 17 SV expr, 8 SV stmt, 2 SV AST serde, 3 Verilog expr, 1 Verilog stmt, 1 Verilog serde, 5 elaborator unit and 13 elaborator end-to-end tests. These cover member chains and lvalues, `'1` on an 8-bit reg, slices/concats/ternary, patterns, `a = b++` vs `a = --b`, and the error cases.
- **Known gaps:**
  - Member selects aren't resolved in the elaborator, and plain `a.b` still lowers to `b`.
  - `'1` in comparisons needs operand widths.
  - Typed patterns `T'{...}`, replication patterns `'{N{x}}`, keyword casts (`int'(x)`), streaming operators and `inside` ranges are not done.
  - Unary `+` still maps to IR `Neg`, a pre-existing bug.
- **Merged with #31 (2026-11-03):** the precedence-matrix S-expression renderers from #31 now cover `Member`, `UnbasedUnsized`, `AssignmentPattern` and `PostIncr`/`PostDecr`. New matrix tests (`member_fill_pattern_postfix_with_index_ops` in SV, `member_select_with_index_ops` in Verilog) check that these forms compose with full index and `+:`/`-:` expressions, e.g. `arr[i << 2].valid` and `d[8*i+7 -: 8].f`.
