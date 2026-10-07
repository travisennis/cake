#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/check-coverage.sh

Runs the coverage, absolute CRAP, and per-function cyclomatic-complexity gates
from one instrumented run.
Set COVERAGE_THRESHOLD to override the default threshold of 90.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

threshold="${COVERAGE_THRESHOLD:-90}"

failed=0

# === Gate 1: Total Coverage ===
echo "=== Total Coverage Gate ==="

# Run the suite once under instrumentation and retain the profile data.
# The summary below and the LCOV export for gates 2 and 3 both derive
# from that single run.
#
# Measurement runs through scripts/hermetic-coverage.sh, which points HOME at a
# scratch directory so a test that reads the developer's home (the skill loader
# scans ~/.agents/skills, for example) cannot move this run's coverage away from
# what the CI runner measures (#699).
#
# Profile data from an earlier run is removed before measuring, and the gate does
# not delegate that removal to the tool: no `cargo llvm-cov clean` mode removes
# everything, so the strongest clean cannot stand alone. `scripts/coverage-clean.sh`
# runs that clean, removes what it left, and proves the directory is clean. Residue
# and a report that spells one source file under two roots have produced false
# totals (#520); rather than print a verdict nobody can trust, this stops before
# the threshold is evaluated.
echo "=== Coverage Artifact Guard ==="
scripts/coverage-clean.sh || exit 1

echo ""
scripts/hermetic-coverage.sh --no-report

output="$(scripts/hermetic-coverage.sh report)"
printf '%s\n' "$output"

coverage="$(printf '%s\n' "$output" | grep "^TOTAL" | grep -oE '[0-9]+\.[0-9]+%' | tail -1 | tr -d '%' || true)"
if [ -z "$coverage" ]; then
    echo "ERROR: could not read TOTAL coverage from cargo llvm-cov output" >&2
    exit 1
fi

echo "Coverage: ${coverage}%"
if [ "$(echo "$coverage < $threshold" | bc -l)" = "1" ]; then
    echo "FAIL: Total coverage (${coverage}%) is below threshold (${threshold}%)"
    failed=1
else
    echo "PASS: Total coverage (${coverage}%) meets threshold (${threshold}%)"
fi

# === Gate 2: Absolute CRAP ===
echo ""
echo "=== CRAP Gate ==="

# Extracted test modules (*_tests.rs) have no LCOV entries because they
# contain only test-only code with no instrumented coverage data. The
# missing-source warning for these files is non-fatal — they are already
# excluded from CRAP scoring via `--exclude '**/*_tests.rs'` in
# scripts/cargo-crap.sh (task 226). Use --ignore-filename-regex to
# suppress the warning for these expected cases.
scripts/hermetic-coverage.sh report --lcov --output-path lcov.info --ignore-filename-regex '_tests\.rs$'
python3 scripts/coverage-guard.py --lcov lcov.info || exit 1

# scripts/check-crap.sh enforces the absolute CRAP targets in
# docs/guardrails/complexity-targets.md (fail above CRAP 30, warn above 15 for
# CC 10 or lower, grandfather allowances from the baseline). It replaced the
# per-function CRAP delta ratchet, which contradicted the CC gate (#658).
crap_exit=0
scripts/check-crap.sh --lcov lcov.info --baseline ci/cargo-crap-baseline.json || crap_exit=$?

if [ "$crap_exit" -ne 0 ]; then
    echo "FAIL: CRAP gate failed (exit code ${crap_exit})"
    failed=1
else
    echo "PASS: No CRAP exceedance"
fi

# === Gate 3: Cyclomatic Complexity ===
echo ""
echo "=== Cyclomatic Complexity Gate ==="

cc_exit=0
scripts/check-cc.sh --lcov lcov.info --baseline ci/cargo-crap-baseline.json || cc_exit=$?

if [ "$cc_exit" -ne 0 ]; then
    echo "FAIL: Cyclomatic complexity gate failed (exit code ${cc_exit})"
    failed=1
else
    echo "PASS: No cyclomatic complexity exceedance"
fi

# === Summary ===
echo ""
if [ "$failed" -eq 1 ]; then
    echo "FAIL: One or more coverage gates failed" >&2
    exit 1
fi
echo "PASS: All coverage gates passed"
