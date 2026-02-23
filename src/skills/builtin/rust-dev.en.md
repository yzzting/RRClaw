---
name: rust-dev
description: Rust development assistant. Code generation, error handling patterns, performance optimization, and cargo command guidance. Use when the user is doing Rust development.
tags: [dev, rust]
---

# Rust Development Assistant

## Code Conventions
- Define error types with `thiserror`; use `color_eyre` for top-level error handling
- Use `tokio` for async code; use `async_trait` for async traits
- Use `serde` + `serde_json` for serialization
- Prefer `&str` over `String`; minimize unnecessary clones
- Public APIs should return `Result<T>`; internal functions may use `Option<T>`

## Workflow
1. Use `file_read` to read relevant code and understand context and existing style
2. Follow the project's established conventions when generating code
3. Run `cargo check` via shell for a quick compilation check
4. Run `cargo test` via shell to verify tests pass
5. Run `cargo clippy -- -W clippy::all` via shell to eliminate lint warnings
6. Confirm `cargo build --release` succeeds

## Common Commands
- `cargo check` — fast compilation check (no binary produced)
- `cargo test` — run all tests
- `cargo test <name>` — run a specific test
- `cargo clippy -- -W clippy::all` — lint check
- `cargo build --release` — release build
- `cargo doc --open` — generate and view documentation
- `cargo add <crate>` — add a dependency

## Notes
- Do not introduce unnecessary new crates; prefer the standard library
- Place tests inside a `#[cfg(test)] mod tests` block
- For tests involving the filesystem, use `tempfile::tempdir()` to create temporary directories
