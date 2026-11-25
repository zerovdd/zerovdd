#!/usr/bin/env bash
# Shared POSIX/Git-Bash helpers for zerovdd scripts and hooks.

set -euo pipefail

# Construct special UTF-8 code points programmatically so helper scripts never
# embed restricted literal characters.
EMDASH_CHAR="$(printf '\xe2\x80\x94')"
ROBOT_EMOJI_CHAR="$(printf '\xf0\x9f\xa4\x96')"

log_info() {
    printf '[info] %s\n' "$*"
}

log_warn() {
    printf '[warn] %s\n' "$*" >&2
}

log_error() {
    printf '[error] %s\n' "$*" >&2
}

die() {
    log_error "$*"
    exit 1
}

repo_root() {
    if git rev-parse --show-toplevel >/dev/null 2>&1; then
        git rev-parse --show-toplevel
    else
        pwd
    fi
}

require_cmd() {
    local cmd="$1"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        die "Required command not found on PATH: $cmd"
    fi
}

is_emdash_exempt_path() {
    local path="$1"
    case "$path" in
        plans/*|*/plans/*|CHANGELOG.md)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}
