Review what was just accomplished and record it as a new progress entry. Do **not** add entries to `PROGRESS.md`; create a new file instead (see `progress/README.md`):

- Path: `progress/YYYY-MM-DD-<slug>.md`, where the date is today and `<slug>` is the branch name without its `claude/` prefix and with any remaining `/` replaced by `-` (or a short kebab-case summary).
- One file per task. If this task already has a file (for example, you are revising it after review), update that file rather than creating a second one. A different task on the same branch gets its own file.
- Content: one `## YYYY-MM-DD — <Area>: <Title> (Fixes #N)` heading followed by bullets, in the same style as the existing entries.

First run `cargo test` and `cargo clippy --all-targets -- -D warnings` so the entry records the current results. Then write the entry, including:

1. **Date and summary** of what was implemented
2. **Crates affected** — which crates were created or modified
3. **Tests added** — list the test functions/modules added
4. **Test results** — the pass/fail counts from the `cargo test` run above
5. **Clippy status** — whether the `cargo clippy` run above was clean
6. **Phase progress** — note progress against the current phase. Only edit the `Current Phase` line in `PROGRESS.md` if the phase itself changes.
7. **Next steps** — what should be worked on next
8. **Blockers/Decisions** — any open questions or decisions made

If $ARGUMENTS is provided, use it as context for what was accomplished.

If you change any code after writing the entry, rerun both commands and update the recorded results.
