---
name: release-notes
description: Generate changelog from recent commits and PROGRESS.md updates
---

# Release Notes Generator

Generate a changelog summary from git history and PROGRESS.md updates. Optionally takes a version tag or commit range as `$ARGUMENTS` (e.g., `/release-notes v0.2.0` or `/release-notes HEAD~20..HEAD`).

## Steps

1. **Determine the range:**
   - If `$ARGUMENTS` is a version tag: use `<tag>..HEAD`
   - If `$ARGUMENTS` is a commit range: use as-is
   - If no arguments: use the last 20 commits

2. **Gather commit history:**
   ```bash
   cd "$CLAUDE_PROJECT_DIR" && git log --oneline --no-merges <range>
   ```

3. **Gather changed files per area:**
   ```bash
   cd "$CLAUDE_PROJECT_DIR" && git diff --stat <range>
   ```

4. **Read PROGRESS.md** for milestone context:
   ```bash
   head -100 "$CLAUDE_PROJECT_DIR/PROGRESS.md"
   ```

5. **Categorize changes** into:
   - **New Crates**: Entirely new crates added
   - **Features**: New capabilities in existing crates
   - **Bug Fixes**: Corrections to existing behavior
   - **Performance**: Optimization improvements
   - **Testing**: New or improved tests
   - **Documentation**: Doc improvements
   - **Infrastructure**: CI/CD, build system, tooling

6. **Generate the release notes:**

```markdown
## Aion <version or date> Release Notes

### Highlights
- <1-3 sentence summary of the most important changes>

### New Crates
- **aion_<name>** — <one-line description>

### Features
- <crate>: <description of feature>

### Bug Fixes
- <crate>: <description of fix>

### Performance
- <crate>: <description of improvement>

### Testing
- <summary of test additions> (<N> new tests)

### Infrastructure
- <description of CI/build/tooling changes>

### Stats
- **Commits**: <N>
- **Files changed**: <N>
- **Crates affected**: <list>
- **Total tests**: <N> (run `cargo test 2>&1 | tail -1` to get current count)
```

## Notes

- Do NOT create git tags or modify any files. This is a read-only report.
- Skip empty categories.
- Focus on user-visible changes — internal refactors go under a brief "Internal" section only if significant.
- If a commit message is unclear, check the diff to understand what changed.
