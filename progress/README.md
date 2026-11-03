# Progress Entries

One file per completed task. This replaces adding entries to the top of
`PROGRESS.md`: when every branch edits the same lines of one file, every
open PR conflicts as soon as anything merges. Separate files never conflict.

## Naming

```
progress/YYYY-MM-DD-<slug>.md
```

- `YYYY-MM-DD` is the date the work was done, so `ls` sorts entries by date
  and `ls -r progress/` lists the newest first.
- `<slug>` is the branch name without its `claude/` prefix (for example
  `fix-31-index-precedence`), or a short kebab-case summary. It only has to be
  unique.
- If a branch already has an entry, update that file instead of adding a
  second one.

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
