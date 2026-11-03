# Progress Entries

One file per completed task. This replaces adding entries to the top of
`PROGRESS.md`: when every branch edits the same lines of one file, every
open PR conflicts as soon as anything merges. Separate files never conflict.

## Naming

```
progress/YYYY-MM-DD-<slug>.md
```

- `YYYY-MM-DD` is the date the work was done, so entries sort by date.
  List the newest first with `ls -r progress/????-??-??-*.md`; a bare
  `ls -r progress/` would put this README first.
- `<slug>` is the branch name without its `claude/` prefix, with any
  remaining `/` replaced by `-` (for example `fix-31-index-precedence`), or a
  short kebab-case summary. It must not contain `/` and only has to be unique.
- One file per **task**. When you revise the same task (for example after
  review), update its file instead of adding a second one. A different task on
  the same branch gets its own file.

## Contents

Same format as the historical entries in `PROGRESS.md`: one heading, then
concise bullets.

```markdown
## 2026-11-02 — Verilog/SV Parsers: Short Title (Fixes #31)

- **What changed:** ...
- **Tests:** ...
- **Known gaps:** ...
```

Cover what was implemented, what tests were added, the status of the current
phase, and any blockers or decisions made (CLAUDE.md rule 6).

`PROGRESS.md` still holds the **Current Phase** line and all entries written
before this convention. Edit it only when the phase itself changes.
