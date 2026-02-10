---
name: new-crate
description: Scaffold a new crate in the Aion workspace with standard boilerplate
---

# New Crate Scaffolder

Create a new crate in the Aion workspace following project conventions. The crate name is provided as `$ARGUMENTS` (without the `aion_` prefix, e.g., `/new-crate lsp`).

## Steps

1. **Validate the name**: Ensure the crate doesn't already exist under `crates/aion_<name>/`.

2. **Create the directory structure:**
   ```
   crates/aion_<name>/
   crates/aion_<name>/src/
   crates/aion_<name>/src/lib.rs
   crates/aion_<name>/Cargo.toml
   ```

3. **Generate `Cargo.toml`** following workspace conventions:
   ```toml
   [package]
   name = "aion_<name>"
   version = "0.1.0"
   edition = "2021"
   license = "MIT OR Apache-2.0"
   description = "<ask user or infer from name>"

   [dependencies]
   # Add common deps based on crate purpose

   [dev-dependencies]
   ```

4. **Generate `src/lib.rs`** with standard boilerplate:
   ```rust
   #![warn(missing_docs)]

   //! <Crate-level documentation describing what this crate does.>

   #[cfg(test)]
   mod tests {
       use super::*;

       #[test]
       fn smoke_test() {
           // TODO: Replace with real tests
       }
   }
   ```

5. **Add the crate to the workspace** `Cargo.toml` members list.

6. **Verify it compiles:**
   ```bash
   cargo check -p aion_<name>
   ```

7. **Report what was created** and remind the user to:
   - Add proper dependencies
   - Implement the crate following the technical spec
   - Write comprehensive tests

## Conventions

- All crates use `edition = "2021"`
- All crates have `#![warn(missing_docs)]` as the first line
- All `lib.rs` files have `//!` crate-level documentation
- All public items need `///` doc comments
- Tests go in `#[cfg(test)] mod tests {}` within the same file
- Check `docs/aion-technical-spec.md` for the crate's planned API
