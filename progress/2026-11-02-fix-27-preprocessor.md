## 2026-11-02 — Verilog/SV: Shared Preprocessor `aion_vpp` (Fixes #27)

- **New crate `aion_vpp`:** a text-level preprocessor shared by the Verilog and SV front ends. It depends only on `aion_source` and `aion_diagnostics`. It supports:
  - `` `define `` (object-like and function-like, SV default arguments, `\` continuation), `` `undef ``, `` `undefineall ``;
  - nested invocation, `` `` `` pasting, `` `" `` stringification, `` `__FILE__ ``/`` `__LINE__ ``;
  - nested `` `ifdef ``/`` `ifndef ``/`` `elsif ``/`` `else ``/`` `endif ``;
  - `` `include `` (resolved relative to the including file, then include dirs, with cycle and depth checks);
  - `` `timescale ``/`` `default_nettype `` (both recorded), plus `` `resetall ``, `` `celldefine ``, `` `line ``, `` `pragma ``, `` `begin_keywords ``, `` `unconnected_drive `` and the rest of the standard set.
  - All preprocessor errors use **E106**. Recursion, depth, expansion-count and output-size guards mean user input can't hang it or panic it.
- **Spans:** the output carries a byte-level source map. Tokens copied from a file, a macro body or a macro argument keep their exact span, so `SourceDb` text equals token text and the elaborator still reads literal values correctly. Tokens that exist in no file (pasting, `` `W'b0 ``) point into a `<preprocessed>` view file in the `SourceDb`.
- **Parsers:** `parse_file` now preprocesses (read-only, so `` `include `` is an error there). The new `parse_file_with_options(file, &mut SourceDb, &PreprocessOptions, ..)` adds includes and predefined macros. Parsers read token text through new `text_ranges` (`with_text_ranges`). The lexers are unchanged.
- **CLI:** `parse_all_files` uses `parse_file_with_options`, so `aion build`/`lint` resolve includes.
- **`Span::merge`:** returns `self` for spans in different files instead of panicking, since a construct can now start in one file and end in an included one.
- **Tests:** 58 `aion_vpp` unit tests; 17 parser tests in each of the Verilog and SV parsers; 2 CLI `parse_all_files` tests; `aion_conformance/tests/preprocessor.rs` (2 end-to-end tests from files on disk through elaboration, checking widths); 1 `Span::merge` test.
- **Known gaps:**
  - `aion.toml` has no defines/include-dir fields yet, so the CLI uses defaults.
  - SV-2023 `` `ifdef `` expressions are not supported.
  - Macro arguments must sit in the same frame as the macro name.
  - The lexer still gives a 1-byte error span inside non-ASCII characters (#34).
