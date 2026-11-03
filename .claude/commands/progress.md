Review what was just accomplished and record it as a new progress entry. Do **not** edit `PROGRESS.md`; create a new file instead (see `progress/README.md`):

- Path: `progress/YYYY-MM-DD-<slug>.md`, where the date is today and `<slug>` is the branch name without its `claude/` prefix (or a short kebab-case summary). If the file already exists on this branch, update it rather than creating a second one.
- Content: one `## YYYY-MM-DD — <Area>: <Title> (Fixes #N)` heading followed by bullets, in the same style as the existing entries.

Include:

1. **Date and summary** of what was implemented
2. **Crates affected** — which crates were created or modified
3. **Tests added** — list the test functions/modules added
4. **Test results** — run `cargo test` and record pass/fail counts
5. **Clippy status** — run `cargo clippy --all-targets -- -D warnings` and confirm clean
6. **Phase progress** — note progress against the current phase. Only edit the `Current Phase` line in `PROGRESS.md` if the phase itself changes.
7. **Next steps** — what should be worked on next
8. **Blockers/Decisions** — any open questions or decisions made

If $ARGUMENTS is provided, use it as context for what was accomplished.

After writing the entry, run `cargo test` and `cargo clippy --all-targets -- -D warnings` to verify the current state is clean.
