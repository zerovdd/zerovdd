#!/usr/bin/env bash
# Diff-scoped em-dash gate with safe git ref fallback chain.
#
# Ref resolution order when no argument is provided:
#   1. origin/main...HEAD (when origin/main exists and differs from HEAD)
#   2. main...HEAD        (when main exists and differs from HEAD)
#   3. --cached           (staged changes, including unborn HEAD or HEAD == main)
#   4. full tracked scan  (when no diff/staged changes exist on an initial repo)
#
# Explicit arguments supported:
#   --cached       Check staged diff only
#   --all          Check all tracked files
#   <git-diff-arg> Check explicit git diff range (e.g. origin/main...HEAD)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

ROOT="$(repo_root)"
cd "$ROOT"

violations=0

scan_diff_stream() {
    local current_file=""
    local line=""
    while IFS= read -r line; do
        # Strip trailing CR if checking CRLF files on Windows.
        line="${line%$'\r'}"
        case "$line" in
            "+++ b/"*)
                current_file="${line#+++ b/}"
                continue
                ;;
            "+++ /dev/null")
                current_file=""
                continue
                ;;
            "+++"*|"---"*|"@@"*|"diff --git"*|"index "*)
                continue
                ;;
        esac

        if [ -z "$current_file" ]; then
            continue
        fi
        if is_emdash_exempt_path "$current_file"; then
            continue
        fi

        case "$line" in
            "+"*)
                local added="${line#+}"
                if [[ "$added" == *"$EMDASH_CHAR"* ]] && [[ "$added" != *"ok-emdash"* ]]; then
                    printf 'em-dash violation in %s: %s\n' "$current_file" "$added" >&2
                    violations=$((violations + 1))
                fi
                ;;
        esac
    done
}

scan_git_diff() {
    local tmp_diff
    tmp_diff="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$tmp_diff'" RETURN

    git -c diff.noprefix=false -c diff.mnemonicPrefix=false -c core.quotepath=false \
        diff --no-color --no-ext-diff --unified=0 "$@" -- . ':!plans' ':!CHANGELOG.md' >"$tmp_diff"
    scan_diff_stream <"$tmp_diff"
}

scan_tracked_files() {
    local file=""
    while IFS= read -r -d '' file; do
        if is_emdash_exempt_path "$file"; then
            continue
        fi
        if [ ! -f "$file" ]; then
            continue
        fi
        # Skip binary files and empty files cleanly.
        if ! grep -Iq . "$file"; then
            continue
        fi
        local line_no=0
        local line=""
        while IFS= read -r line || [ -n "$line" ]; do
            line_no=$((line_no + 1))
            line="${line%$'\r'}"
            if [[ "$line" == *"$EMDASH_CHAR"* ]] && [[ "$line" != *"ok-emdash"* ]]; then
                printf 'em-dash violation in %s:%d: %s\n' "$file" "$line_no" "$line" >&2
                violations=$((violations + 1))
            fi
        done <"$file"
    done < <(git ls-files -z)
}

run_check() {
    if [ "$#" -gt 0 ]; then
        case "$1" in
            --cached)
                scan_git_diff --cached
                return
                ;;
            --all)
                scan_tracked_files
                return
                ;;
            *)
                scan_git_diff "$1"
                return
                ;;
        esac
    fi

    if ! git rev-parse --verify --quiet HEAD >/dev/null 2>&1; then
        # Unborn HEAD: check staged changes.
        scan_git_diff --cached
        return
    fi

    local head_sha
    head_sha="$(git rev-parse HEAD)"

    if git rev-parse --verify --quiet origin/main >/dev/null 2>&1 \
        && git merge-base origin/main HEAD >/dev/null 2>&1; then
        local origin_sha
        origin_sha="$(git rev-parse origin/main)"
        if [ "$head_sha" != "$origin_sha" ]; then
            scan_git_diff origin/main...HEAD
            scan_git_diff --cached
            scan_git_diff
            return
        fi
    fi

    if git rev-parse --verify --quiet main >/dev/null 2>&1 \
        && git merge-base main HEAD >/dev/null 2>&1; then
        local main_sha
        main_sha="$(git rev-parse main)"
        if [ "$head_sha" != "$main_sha" ]; then
            scan_git_diff main...HEAD
            scan_git_diff --cached
            scan_git_diff
            return
        fi
    fi

    if ! git diff --cached --quiet -- . 2>/dev/null || ! git diff --quiet -- . 2>/dev/null; then
        scan_git_diff --cached
        scan_git_diff
        return
    fi

    scan_tracked_files
}

run_check "$@"

if [ "$violations" -gt 0 ]; then
    die "Found $violations em-dash violation(s). Replace with '--', ':', or parentheses (or add 'ok-emdash' if intentional)."
fi

log_info "Em-dash check passed."
