#!/usr/bin/env bash
# Standalone local and CI lint/verification helper (does not require mise in CI).
#
# Subcommands:
#   (default / --all)   Run full standalone repo lints
#   --setup             Configure .githooks and verify required tooling
#   --fmt               Format root workspace and driver-wdk (when present)
#   --fmt-check         Check formatting in root workspace and driver-wdk (when present)
#   --check-win         Check existing Windows user-mode crates against x86_64-pc-windows-msvc
#   --check-nostd       Check existing no_std Layer 0/1 crates under --no-default-features
#   --check-drift       Verify generated/mirrored repository invariants (AGENTS.md + versions)
#   --check-clean-tree  Ensure working tree, index, and untracked files are clean

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

ROOT="$(repo_root)"
cd "$ROOT"

run_setup() {
    git config core.hooksPath .githooks
    log_info "Configured core.hooksPath = $(git config --get core.hooksPath)"

    require_cmd rustc
    require_cmd cargo
    log_info "rustc: $(rustc --version)"
    log_info "cargo: $(cargo --version)"

    local missing=0
    for tool in cargo-machete cargo-deny cargo-audit; do
        if command -v "$tool" >/dev/null 2>&1; then
            log_info "Found $tool: $(command -v "$tool")"
        else
            log_warn "Missing recommended tool: $tool"
            missing=$((missing + 1))
        fi
    done

    if command -v git-cliff >/dev/null 2>&1; then
        log_info "Found git-cliff: $(command -v git-cliff)"
    else
        log_info "Optional release tool not installed: git-cliff"
    fi

    if [ "$missing" -gt 0 ]; then
        log_warn "$missing recommended cargo subcommand(s) missing; install them before running full verification."
    fi
}

run_fmt() {
    cargo fmt --all
    if [ -f "$ROOT/driver-wdk/Cargo.toml" ]; then
        cargo fmt --manifest-path "$ROOT/driver-wdk/Cargo.toml" --all
    fi
}

run_fmt_check() {
    cargo fmt --all -- --check
    if [ -f "$ROOT/driver-wdk/Cargo.toml" ]; then
        cargo fmt --manifest-path "$ROOT/driver-wdk/Cargo.toml" --all -- --check
    fi
}

run_check_win() {
    local candidates=(zerovdd-client zerovdd-installer zerovdd-cli zerovdd-svc zerovdd-tray)
    local args=()
    local crate=""
    for crate in "${candidates[@]}"; do
        if [ -f "$ROOT/crates/$crate/Cargo.toml" ]; then
            args+=(-p "$crate")
        fi
    done

    if [ "${#args[@]}" -eq 0 ]; then
        log_info "check:win skipped (no Windows user-mode crates present yet)."
        return 0
    fi

    log_info "Running cargo check --target x86_64-pc-windows-msvc ${args[*]}"
    cargo check --target x86_64-pc-windows-msvc "${args[@]}"
}

run_check_nostd() {
    local candidates=(zerovdd-proto zerovdd-edid zerovdd-modes zerovdd-engine)
    local args=()
    local crate=""
    for crate in "${candidates[@]}"; do
        if [ -f "$ROOT/crates/$crate/Cargo.toml" ]; then
            args+=(-p "$crate")
        fi
    done

    if [ "${#args[@]}" -eq 0 ]; then
        log_info "check:nostd skipped (no no_std domain crates present yet)."
        return 0
    fi

    log_info "Running cargo check --no-default-features ${args[*]}"
    cargo check --no-default-features "${args[@]}"
}

run_check_drift() {
    if [ ! -e "$ROOT/AGENTS.md" ]; then
        die "AGENTS.md is missing at repository root."
    fi

    if cmp -s "$ROOT/CLAUDE.md" "$ROOT/AGENTS.md"; then
        log_info "AGENTS.md matches CLAUDE.md."
    else
        # Support Windows clones with core.symlinks=false where git writes a 120000 stub file.
        local mode_entry
        mode_entry="$(git ls-files -s -- AGENTS.md 2>/dev/null || true)"
        local stub_target
        stub_target="$(tr -d '\r\n' < "$ROOT/AGENTS.md")"
        if [[ "$mode_entry" == "120000 "* ]] && [ "$stub_target" = "CLAUDE.md" ]; then
            log_info "AGENTS.md is a tracked git symlink stub pointing to CLAUDE.md."
        else
            die "AGENTS.md content drifted from CLAUDE.md."
        fi
    fi

    "$BASH" "$SCRIPT_DIR/check-versions.sh"
}

run_check_clean_tree() {
    local status_out
    status_out="$(git status --porcelain)"
    if [ -n "$status_out" ]; then
        die "Working tree is not clean (modified, staged, or untracked files exist). Clean or commit changes before release preflight."
    fi
    log_info "Git working tree is clean."
}

run_all_repo_lints() {
    log_info "Running rustfmt check..."
    run_fmt_check

    log_info "Running clippy..."
    cargo clippy --workspace --all-targets -- -D warnings

    run_check_win
    run_check_nostd

    log_info "Running compiled static lints..."
    cargo test -p zerovdd-checks --test static_lints

    log_info "Running hook and script tests..."
    cargo test -p zerovdd-checks --test script_tests

    log_info "Running em-dash diff check..."
    "$BASH" "$SCRIPT_DIR/check-emdash-diff.sh"

    log_info "Running drift and version checks..."
    run_check_drift
}

case "${1:---all}" in
    --all)
        run_all_repo_lints
        ;;
    --setup)
        run_setup
        ;;
    --fmt)
        run_fmt
        ;;
    --fmt-check)
        run_fmt_check
        ;;
    --check-win)
        run_check_win
        ;;
    --check-nostd)
        run_check_nostd
        ;;
    --check-drift)
        run_check_drift
        ;;
    --check-clean-tree)
        run_check_clean_tree
        ;;
    *)
        die "Unknown subcommand: $1"
        ;;
esac
