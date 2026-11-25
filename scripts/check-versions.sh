#!/usr/bin/env bash
# Validates version lockstep across root Cargo.toml and any present driver/installer manifests
# (driver-wdk/Cargo.toml, driver-wdk/zerovdd.inf, installer/zerovdd.wxs) plus optional --tag vX.Y.Z.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

TAG_ARG=""
ROOT_OVERRIDE=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --tag)
            shift
            [ "$#" -gt 0 ] || die "--tag requires a value (e.g. v0.1.0)"
            TAG_ARG="$1"
            ;;
        --root)
            shift
            [ "$#" -gt 0 ] || die "--root requires a directory path"
            ROOT_OVERRIDE="$1"
            ;;
        -h|--help)
            printf 'Usage: %s [--tag vX.Y.Z] [--root DIR]\n' "$0"
            exit 0
            ;;
        *)
            die "Unknown argument: $1"
            ;;
    esac
    shift
done

if [ -n "$ROOT_OVERRIDE" ]; then
    ROOT="$ROOT_OVERRIDE"
else
    ROOT="$(repo_root)"
fi

extract_cargo_version() {
    local file="$1"
    awk '
        /^\[workspace\.package\]/ { in_sec = 1; next }
        /^\[package\]/ { in_sec = 1; next }
        /^\[/ { in_sec = 0; next }
        in_sec && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"/ {
            line = $0
            sub(/^[^"]*"/, "", line)
            sub(/".*$/, "", line)
            print line
            exit
        }
    ' "$file"
}

extract_inf_version() {
    local file="$1"
    # POSIX awk: matches DriverVer = MM/DD/YYYY,X.Y.Z.W case-insensitively and returns X.Y.Z.
    awk '
        tolower($0) ~ /^[[:space:]]*driverver[[:space:]]*=/ {
            line = $0
            sub(/\r$/, "", line)
            sub(/^[^,]*,[[:space:]]*/, "", line)
            sub(/[[:space:]]*;.*$/, "", line)
            sub(/[[:space:]]*$/, "", line)
            n = split(line, parts, ".")
            if (n >= 3) {
                print parts[1] "." parts[2] "." parts[3]
                exit
            }
        }
    ' "$file"
}

extract_wxs_version() {
    local file="$1"
    # POSIX awk: matches Version="X.Y.Z[.W]" strictly inside <Package ...> or <Product ...>,
    # or resolves `<?define Version = "X.Y.Z[.W]" ?>` when Version="$(var.Version)" is used.
    awk '
        match($0, /<\?define[[:space:]]+Version[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+(\.[0-9]+)?"/) {
            def = substr($0, RSTART, RLENGTH)
            sub(/^[^"]*"/, "", def)
            sub(/".*$/, "", def)
            def_ver = def
        }
        /<(Package|Product)([[:space:]>]|$)/ { in_pkg = 1 }
        in_pkg {
            if (match($0, /[[:space:]]Version="[0-9]+\.[0-9]+\.[0-9]+(\.[0-9]+)?"/)) {
                ver = substr($0, RSTART, RLENGTH)
                sub(/^.*Version="/, "", ver)
                sub(/".*$/, "", ver)
                n = split(ver, parts, ".")
                if (n >= 3) {
                    print parts[1] "." parts[2] "." parts[3]
                    exit
                }
            } else if (match($0, /[[:space:]]Version="\$\(var\.Version\)"/) && def_ver != "") {
                n = split(def_ver, parts, ".")
                if (n >= 3) {
                    print parts[1] "." parts[2] "." parts[3]
                    exit
                }
            }
            if (index($0, ">") > 0) {
                in_pkg = 0
            }
        }
    ' "$file"
}

ROOT_CARGO="$ROOT/Cargo.toml"
[ -f "$ROOT_CARGO" ] || die "Root Cargo.toml not found at $ROOT_CARGO"

EXPECTED_VERSION="$(extract_cargo_version "$ROOT_CARGO")"
if ! [[ "$EXPECTED_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    die "Invalid or missing version in $ROOT_CARGO: '$EXPECTED_VERSION'"
fi

EXPECTED_CORE="${EXPECTED_VERSION%%-*}"
mismatches=0

check_match() {
    local label="$1"
    local actual="$2"
    local expected="$3"
    if [ -z "$actual" ]; then
        log_error "Could not parse version from $label"
        mismatches=$((mismatches + 1))
    elif [ "$actual" != "$expected" ]; then
        log_error "Version mismatch in $label: expected '$expected', found '$actual'"
        mismatches=$((mismatches + 1))
    else
        log_info "$label version matches ($actual)"
    fi
}

log_info "Root Cargo.toml version: $EXPECTED_VERSION"

if [ -f "$ROOT/driver-wdk/Cargo.toml" ]; then
    driver_ver="$(extract_cargo_version "$ROOT/driver-wdk/Cargo.toml")"
    check_match "driver-wdk/Cargo.toml" "$driver_ver" "$EXPECTED_VERSION"
fi

if [ -f "$ROOT/driver-wdk/zerovdd.inf" ]; then
    inf_ver="$(extract_inf_version "$ROOT/driver-wdk/zerovdd.inf")"
    check_match "driver-wdk/zerovdd.inf" "$inf_ver" "$EXPECTED_CORE"
fi

if [ -f "$ROOT/installer/zerovdd.wxs" ]; then
    wxs_ver="$(extract_wxs_version "$ROOT/installer/zerovdd.wxs")"
    check_match "installer/zerovdd.wxs" "$wxs_ver" "$EXPECTED_CORE"
fi

if [ -n "$TAG_ARG" ]; then
    if [[ "$TAG_ARG" != v* ]]; then
        log_error "Tag '$TAG_ARG' must start with 'v' (expected v$EXPECTED_VERSION)"
        mismatches=$((mismatches + 1))
    else
        tag_ver="${TAG_ARG#v}"
        check_match "git tag ($TAG_ARG)" "$tag_ver" "$EXPECTED_VERSION"
    fi
fi

if [ "$mismatches" -gt 0 ]; then
    die "Version lockstep check failed with $mismatches mismatch(es)."
fi

log_info "Version lockstep check passed ($EXPECTED_VERSION)."
