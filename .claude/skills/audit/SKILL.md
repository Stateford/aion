---
name: audit
description: Run security and license audits on all workspace dependencies
---

# Dependency Audit

Run security and license audits on the Aion workspace dependencies.

## Steps

1. **Check if `cargo-audit` is installed:**
   ```bash
   cargo audit --version 2>/dev/null || cargo install cargo-audit
   ```

2. **Run security audit:**
   ```bash
   cd "$CLAUDE_PROJECT_DIR" && cargo audit
   ```

3. **Check for yanked crates:**
   ```bash
   cd "$CLAUDE_PROJECT_DIR" && cargo audit --deny yanked
   ```

4. **Check for outdated dependencies:**
   ```bash
   cd "$CLAUDE_PROJECT_DIR" && cargo update --dry-run 2>&1
   ```

5. **Report findings** in this format:

```
## Dependency Audit Report

### Security Vulnerabilities
- <list of advisories, or "None found">

### Yanked Crates
- <list, or "None">

### Available Updates
- <list of outdated deps with current → latest versions>

### Recommended Actions
- <numbered list, or "All clear — no action needed">
```

## Notes

- Do NOT automatically update dependencies. Report findings for the user to decide.
- If `cargo-audit` installation fails, report the error and suggest manual installation.
- Focus on actionable findings — skip informational notices unless they affect this project.
