## 2026-11-03 — Process: One Progress File per Task

- **Problem:** CLAUDE.md rule 6 had every task prepend an entry to `PROGRESS.md`. With several PRs open at once, all of them edited the same lines, so every merge into master put every other open PR into conflict (PRs #58, #59, #62, #64–#67 conflicted twice in one day, almost entirely on `PROGRESS.md`).
- **Change:** each task now adds its own file, `progress/YYYY-MM-DD-<slug>.md`. New files never conflict. Updated rule 6 and the "Current Phase" pointer in `CLAUDE.md`, the `/progress` and `/implement` commands, and the `release-notes` skill. `progress/README.md` documents the naming and format.
- **`PROGRESS.md`:** keeps the **Current Phase** line and all history up to 2026-11-02, with a note pointing to `progress/`. Only edit it when the phase changes.
- **Tests:** none; documentation and tooling instructions only.
- **Follow-up:** the open Verilog/SV and elaborator PRs move their entries into `progress/` and stop editing `PROGRESS.md`.
