#!/usr/bin/env sh
# Fixture tests for the absolute CRAP gate (scripts/check-crap.sh).
#
# Synthetic cargo-crap reports and baselines are written to a scratch directory
# and the gate is pointed at them through a `cargo`/`cargo-crap` PATH stub, so
# the fixtures need no coverage pass and no Rust build.

set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
script="$here/scripts/check-crap.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/check-crap-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fail() {
    echo "test-check-crap: FAIL: $*" >&2
    exit 1
}

mkdir "$tmp/bin"
printf '#!/bin/sh\nexit 0\n' >"$tmp/bin/cargo-crap"
printf '#!/bin/sh\ncat "$CHECK_CRAP_REPORT"\n' >"$tmp/bin/cargo"
chmod +x "$tmp/bin/cargo-crap" "$tmp/bin/cargo"

# run_case <name> <report-entry> <baseline-entry> <expected status> <want> [<forbid>]
run_case() {
    name="$1"
    report_entry="$2"
    baseline_entry="$3"
    expected="$4"
    want="$5"
    forbid="${6:-}"
    printf '{"version":"0.0.0","entries":[%s]}\n' "$report_entry" >"$tmp/report.json"
    printf '{"version":"0.0.0","entries":[%s]}\n' "$baseline_entry" >"$tmp/baseline.json"
    set +e
    output="$(PATH="$tmp/bin:$PATH" CHECK_CRAP_REPORT="$tmp/report.json" \
        "$script" --baseline "$tmp/baseline.json" 2>&1)"
    status=$?
    set -e
    [ "$status" -eq "$expected" ] || fail "$name: expected exit $expected, got $status: $output"
    printf '%s\n' "$output" | grep -qF "$want" || fail "$name: expected output to contain '$want': $output"
    if [ -n "$forbid" ]; then
        if printf '%s\n' "$output" | grep -qF "$forbid"; then
            fail "$name: output should not contain '$forbid': $output"
        fi
    fi
}

report_entry() { # <function> <cc> <crap>
    printf '{"file":"src/example.rs","function":"%s","cyclomatic":%s,"crap":%s}' "$1" "$2" "$3"
}

baseline_cc() { # <function> <cc> -- a baseline entry with no CRAP allowance
    printf '{"file":"src/example.rs","function":"%s","cyclomatic":%s}' "$1" "$2"
}

baseline_allowance() { # <function> <cc> <crap>
    printf '{"file":"src/example.rs","function":"%s","cyclomatic":%s,"crap":%s}' "$1" "$2" "$3"
}

# A function under the threshold passes.
run_case "under threshold" "$(report_entry fine 5 12)" '' 0 "PASS: No CRAP exceedances"

# A function above the threshold with no allowance fails and names the function.
run_case "over threshold" "$(report_entry bloated 10 61.2)" '' 1 \
    "src/example.rs: bloated has CRAP 61.2, exceeding allowed 30"

# A function above the CRAP 15 stretch target at CC 10 or lower warns but passes.
run_case "stretch warning" "$(report_entry stretchy 4 20)" '' 0 "WARN: src/example.rs: stretchy"

# A function above the stretch target but also above the CC target does not warn.
run_case "no warning above CC target" "$(report_entry complex_clean 14 25)" '' 0 \
    "PASS: No CRAP exceedances" "WARN"

# A baseline entry without a `crap` field sets no allowance, so it fails like a new function.
run_case "baseline without allowance" "$(report_entry documented 10 61.2)" \
    "$(baseline_cc documented 10)" 1 "exceeding allowed 30"

# A grandfathered function may exceed the threshold up to its recorded allowance.
run_case "grandfathered" "$(report_entry legacy 14 210)" \
    "$(baseline_allowance legacy 14 210)" 0 "1 grandfathered"

# A grandfathered function still fails once it exceeds its allowance.
run_case "grandfather exceeded" "$(report_entry legacy 14 215)" \
    "$(baseline_allowance legacy 14 210)" 1 "exceeding allowed 210"

echo "test-check-crap: all cases passed"
