---
name: bench
description: Set up and run criterion benchmarks for a specific crate
---

# Benchmark Runner

Set up and run criterion benchmarks for an Aion crate. The crate name is provided as `$ARGUMENTS` (without the `aion_` prefix, e.g., `/bench sim`).

## Steps

1. **Validate the crate exists:**
   ```bash
   ls crates/aion_<name>/Cargo.toml
   ```

2. **Check if benchmarks already exist:**
   ```bash
   ls crates/aion_<name>/benches/ 2>/dev/null
   ```

3. **If no benchmarks exist, scaffold them:**

   a. Add criterion to `Cargo.toml`:
   ```toml
   [dev-dependencies]
   criterion = { version = "0.5", features = ["html_reports"] }

   [[bench]]
   name = "<name>_bench"
   harness = false
   ```

   b. Create `benches/<name>_bench.rs`:
   ```rust
   use criterion::{criterion_group, criterion_main, Criterion};

   fn bench_<relevant_operation>(c: &mut Criterion) {
       c.bench_function("<operation>", |b| {
           b.iter(|| {
               // TODO: Add benchmark body
           });
       });
   }

   criterion_group!(benches, bench_<relevant_operation>);
   criterion_main!(benches);
   ```

   c. Choose benchmarks based on crate purpose:
   - **Parsers** (vhdl_parser, verilog_parser, sv_parser): Parse a representative source file
   - **sim**: Simulate N delta cycles on a standard testbench
   - **elaborate**: Elaborate a multi-module design
   - **synth**: Run optimization passes on a netlist
   - **pnr**: Placement + routing on a small design
   - **timing**: STA on a timing graph with N nodes
   - **bitstream**: Generate bitstream for a configured design

4. **Run the benchmarks:**
   ```bash
   cd "$CLAUDE_PROJECT_DIR" && cargo bench -p aion_<name>
   ```

5. **Report results** in this format:

```
## Benchmark Results: aion_<name>

### Benchmarks Run
- <benchmark_name>: <time> (± <std_dev>)

### Comparison (if baseline exists)
- <benchmark_name>: <change%> vs previous

### HTML Report
- Open `target/criterion/<group>/report/index.html` for detailed analysis
```

## Notes

- Do NOT modify existing benchmarks without asking first.
- If benchmarks already exist, just run them and report results.
- Use `criterion::black_box()` to prevent dead code elimination.
- Keep benchmark inputs realistic — use patterns from the conformance test suite where possible.
