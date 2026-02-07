Implement the crate or feature described by: $ARGUMENTS

## Workflow

1. **Read the technical spec** — Open `docs/aion-technical-spec.md` and find the section relevant to what you're implementing. Follow the type signatures, interfaces, and algorithms specified there exactly.

2. **Check PROGRESS.md** — See what's already done and what the current phase expects.

3. **Implement the code** — Follow the architecture principles in CLAUDE.md. Use the exact types from the technical spec.

4. **Write unit tests** as you go — minimum 2 tests per public function. Put tests in `#[cfg(test)] mod tests {}` within the same file.

5. **Run tests** — `cargo test -p aion_<crate>` must pass.

6. **Run clippy** — `cargo clippy -p aion_<crate> -- -D warnings` must pass with zero warnings.

7. **Run fmt** — `cargo fmt` to ensure formatting.

8. **Update PROGRESS.md** — Record what was implemented, tests added, and next steps.

## Quality Checklist (verify before finishing)

- [ ] `lib.rs` starts with `#![warn(missing_docs)]`
- [ ] All public types have `///` doc comments (specific, not generic)
- [ ] All public functions have `///` doc comments
- [ ] All enum variants have `///` doc comments
- [ ] `lib.rs` has a `//!` crate-level doc
- [ ] All public functions have unit tests (≥2 per function)
- [ ] Error paths are tested
- [ ] `cargo test -p aion_<crate>` passes
- [ ] `cargo clippy -p aion_<crate> -- -D warnings` passes (this catches missing docs)
- [ ] `cargo fmt --check` passes
- [ ] PROGRESS.md is updated
