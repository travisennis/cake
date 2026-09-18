#!/usr/bin/env sh
# Fixture tests for the per-function cyclomatic-complexity ceiling.

set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
script="$here/scripts/check-cc.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/check-cc-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fail() {
    echo "test-check-cc: FAIL: $*" >&2
    exit 1
}

mkdir "$tmp/bin"
printf '#!/bin/sh\nexit 0\n' >"$tmp/bin/cargo-crap"
printf '#!/bin/sh\ncat "$CHECK_CC_REPORT"\n' >"$tmp/bin/cargo"
chmod +x "$tmp/bin/cargo-crap" "$tmp/bin/cargo"

run_case() { # <name> <report-entry> <baseline-entry> <expected status>
    name="$1"
    report_entry="$2"
    baseline_entry="$3"
    expected="$4"
    printf '{"entries":[%s]}\n' "$report_entry" >"$tmp/report.json"
    printf '{"entries":[%s]}\n' "$baseline_entry" >"$tmp/baseline.json"
    set +e
    output="$(PATH="$tmp/bin:$PATH" CHECK_CC_REPORT="$tmp/report.json" \
        "$script" --baseline "$tmp/baseline.json" 2>&1)"
    status=$?
    set -e
    [ "$status" -eq "$expected" ] || fail "$name: expected exit $expected, got $status: $output"
}

entry() {
    printf '{"file":"src/example.rs","function":"%s","cyclomatic":%s}' "$1" "$2"
}

# Existing functions below the target may grow up to the target, but not past it.
run_case "below-target growth" "$(entry below_target 9)" "$(entry below_target 8)" 0
run_case "crossing target" "$(entry below_target 11)" "$(entry below_target 8)" 1

# Existing functions above the target retain their historical ceiling.
run_case "above-target growth" "$(entry above_target 13)" "$(entry above_target 12)" 1
run_case "above-target no growth" "$(entry above_target 12)" "$(entry above_target 12)" 0

# New functions always use the target ceiling.
run_case "new function at target" "$(entry new_function 10)" '' 0
run_case "new function over target" "$(entry new_function 11)" '' 1

echo "test-check-cc: all cases passed"
