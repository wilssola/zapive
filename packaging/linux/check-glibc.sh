#!/usr/bin/env bash
# Fails when a Linux binary needs a newer glibc or libstdc++ than the
# Ubuntu 22.04 baseline (glibc 2.35, GLIBCXX 3.4.30) the release is built
# for. Usage: check-glibc.sh <binary> [max-glibc] [max-glibcxx]
set -euo pipefail

BIN="${1:?usage: check-glibc.sh <binary> [max-glibc] [max-glibcxx]}"
MAX_GLIBC="${2:-2.35}"
MAX_GLIBCXX="${3:-3.4.30}"

command -v objdump >/dev/null || { echo "::error::objdump (binutils) is required" >&2; exit 2; }
# Read the dynamic symbols once, up front: if objdump fails (missing file,
# not an ELF) the script must fail instead of reporting "none required".
SYMBOLS="$(objdump -T "$BIN")" || { echo "::error::objdump could not read $BIN" >&2; exit 2; }

# Highest version of one symbol-version family the binary requires.
highest() {
    grep -o "$1_[0-9][0-9.]*" <<<"$SYMBOLS" | sed "s/^$1_//" | sort -uV | tail -n1 || true
}

status=0
check() {
    local family="$1" max="$2" found
    found="$(highest "$family")"
    echo "$family: requires up to ${found:-none} (limit $max)"
    if [ -n "$found" ] && [ "$(printf '%s\n%s\n' "$found" "$max" | sort -V | tail -n1)" != "$max" ]; then
        echo "::error::$BIN needs $family $found but the baseline is $max" >&2
        grep "${family}_$found" <<<"$SYMBOLS" >&2 || true
        status=1
    fi
}

check GLIBC "$MAX_GLIBC"
check GLIBCXX "$MAX_GLIBCXX"
exit $status
