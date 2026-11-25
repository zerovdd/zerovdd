# Repository Rules and Engineering Guide (`zerovdd`)

This file is the single source of truth for contributors and automated agents working in `zerovdd`. `AGENTS.md` is a symlink to this file.

## Repository Layout

- `crates/`: Root Rust workspace members (portable Layer 0/1 crates, Windows user-mode Layer 2/3 crates, and `zerovdd-checks`).
- `driver-wdk/`: Isolated WDK sub-workspace (`zerovdd-iddcx-sys`, `zerovdd-driver`, `zerovdd.inf`). Excluded from root `Cargo.toml` so root tests and fast CI lanes do not require the WDK or inherit driver panic profiles.
- `fuzz/`: Excluded nightly `libfuzzer` targets.
- `scripts/`: Standalone POSIX / Git Bash scripts (`scripts/lib/common.sh`, `repo-lints.sh`, `check-emdash-diff.sh`, `check-versions.sh`) and Windows PowerShell automation (`scripts/windows/`).
- `.githooks/`: Local git hooks (`pre-commit`, `commit-msg`) enabled via `mise run setup`.
- `.github/`: SHA-pinned GitHub Actions workflows and Dependabot config.

## Non-Negotiable Engineering Standards

1. **Workspace hygiene:**
   - Pin exact Rust channel in `rust-toolchain.toml` and keep CI workflows in lockstep (`ci_pins_lint`).
   - Single root `clippy.toml` (no per-crate shadowing). Every crate in `crates/` and `driver-wdk/` must declare `[lints] workspace = true`.
   - Use `[workspace.dependencies]` in root `Cargo.toml` via `.workspace = true` for shared external dependencies.
   - Commit `Cargo.lock` for the root workspace (and `driver-wdk/Cargo.lock` for the driver sub-workspace).

2. **Rust code and module conventions:**
   - Rust 2018+ module layout (`foo.rs` + `foo/`; never add `mod.rs`).
   - Declare `[lib] doctest = false` on library crates without doctests.
   - Use `thiserror` for libraries and `anyhow` for binaries.
   - Public API items and `clap` derives use doc comments (`///` / `//!`). Non-doc implementation comments are restricted to explicit `// WHY:` rationales and `// SAFETY:` invariants (`doc_comments_only_lint`).
   - Every `unsafe` block and `unsafe fn` requires an adjacent `// SAFETY:` comment (`unsafe_safety_comment_lint`).
   - No `todo!()`, `unimplemented!()`, or bare `#[allow(...)]` without `reason = "..."` (`suppression_and_todo_lint` + Clippy).

3. **Zero planning artifacts in live code:**
   - Never embed plan task codes, batch labels, decision IDs, or phase/wave identifiers in source code, comments, identifiers, tests, or user-facing docs (`no_plan_reference_codes_lint`). `plans/` is exempt.

4. **Test discipline:**
   - Never call `sleep` (`std::thread::sleep`, `tokio::time::sleep`, etc.) in unit or integration tests; synchronize with barriers, channels, or simulated clocks (`test_sleep_lint`).
   - Keep source-text inspection tests strictly inside `crates/zerovdd-checks` (`source_text_test_boundary_lint`).

5. **Git, hooks, and commits:**
   - Run `mise run setup` once per clone to set `core.hooksPath = .githooks`.
   - Use Conventional Commits (`feat(scope): ...`, `fix(scope): ...`, `chore(scope): ...`).
   - Never commit em dashes (U+2014) outside `plans/` and `CHANGELOG.md` (unless explicitly tagged `ok-emdash`).
   - Never include AI vendor attribution trailers or robot emojis in commit messages.
   - Never bypass hooks with `--no-verify`. Never force-push or push to `main` without explicit approval.

6. **Windows portability:**
   - Keep LF line endings on `.rs`, `.toml`, `.yml`, `.md`, `.sh`, and `.githooks/*`; use CRLF only where Windows tools require it (`.inf`, `.wxs`, `.ps1`, `.cmd`, `.bat`).
   - On Windows, prefer Git Bash (`C:\Program Files\Git\bin\bash.exe`) over `C:\Windows\system32\bash.exe` (WSL) when invoking shell scripts.

## Standard Commands

- `mise run setup`: Configure `.githooks` and check required tools.
- `mise run ci`: Run formatting, Clippy (`-D warnings`), Windows target check, `no_std` check, static lints, script tests, workspace tests, and `cargo-machete`.
- `mise run verify:agent`: Single authoritative local gate (`ci` + `deny:rust` + `check-emdash-diff.sh` + generated-file drift check).
- `mise run release:preflight`: Version lockstep, clean working tree, `ci`, `audit:rust`, and `deny:rust`.

See [crates/AGENTS.md](crates/AGENTS.md) for crate layering rules and [TESTING.md](TESTING.md) for the verification tiers.
