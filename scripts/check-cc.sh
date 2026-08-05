#!/usr/bin/env bash
set -euo pipefail

# Cyclomatic-complexity (CC) gate.
#
# Enforces the complexity targets in docs/guardrails/complexity-targets.md:
#   - A function absent from the baseline (new) may not exceed the CC target.
#   - A function present in the baseline (existing) may not exceed the CC it
#     had when the baseline was generated (ratchet; reductions are tracked in
#     the per-function reduction tasks, see the guardrails doc).
#
# Cyclomatic complexity is coverage-independent, so this check runs without a
# coverage pass (`just cc-check`). scripts/check-coverage.sh reuses the lcov
# file it already produced and runs the same check through this script.
#
# Usage:
#   scripts/check-cc.sh [--lcov lcov.info] [--baseline ci/cargo-crap-baseline.json]
#                       [--target 10]

usage() {
    cat <<'EOF'
Usage: scripts/check-cc.sh [--lcov FILE] [--baseline FILE] [--target N]

Runs the per-function cyclomatic-complexity gate.
  --lcov FILE      LCOV coverage file (optional; CC is coverage-independent)
  --baseline FILE  JSON baseline with per-function cyclomatic values
                   (default: ci/cargo-crap-baseline.json)
  --target N       CC ceiling for functions absent from the baseline (default: 10)
EOF
}

lcov=""
baseline="ci/cargo-crap-baseline.json"
target=10

while [ "$#" -gt 0 ]; do
    case "$1" in
        --lcov)
            if [ "$#" -lt 2 ]; then
                echo "ERROR: --lcov requires a value" >&2
                exit 2
            fi
            lcov="$2"
            shift 2
            ;;
        --baseline)
            if [ "$#" -lt 2 ]; then
                echo "ERROR: --baseline requires a value" >&2
                exit 2
            fi
            baseline="$2"
            shift 2
            ;;
        --target)
            if [ "$#" -lt 2 ]; then
                echo "ERROR: --target requires a value" >&2
                exit 2
            fi
            target="$2"
            shift 2
            ;;
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

if [ ! -f "$baseline" ]; then
    echo "ERROR: baseline not found: $baseline (regenerate with 'just change-risk-baseline')" >&2
    exit 1
fi

report="$(mktemp)"
trap 'rm -f "$report"' EXIT

if [ -n "$lcov" ]; then
    scripts/cargo-crap.sh --lcov "$lcov" --format json > "$report"
else
    scripts/cargo-crap.sh --format json > "$report"
fi

python3 - "$report" "$baseline" "$target" <<'PY'
import json
import sys

report_path, baseline_path, target = sys.argv[1], sys.argv[2], int(sys.argv[3])

with open(report_path) as f:
    report = json.load(f)
with open(baseline_path) as f:
    baseline = json.load(f)

# Function identity is (file, function). Multiple entries may share a name
# across generic instantiations; take the highest CC as the binding value.
baseline_cc: dict[tuple[str, str], float] = {}
for entry in baseline.get("entries", []):
    key = (entry["file"], entry["function"])
    baseline_cc[key] = max(baseline_cc.get(key, 0.0), float(entry["cyclomatic"]))

failed = 0
new_count = 0
for entry in report.get("entries", []):
    key = (entry["file"], entry["function"])
    cc = float(entry["cyclomatic"])
    if key in baseline_cc:
        allowed = baseline_cc[key]
        status = "regressed"
    else:
        allowed = float(target)
        status = "new"
        new_count += 1
    if cc > allowed:
        failed += 1
        print(
            f"FAIL: {entry['file']}: {entry['function']} "
            f"has cyclomatic complexity {cc:g}, exceeding allowed {allowed:g} "
            f"({status}; target {target:g})"
        )

total = len(report.get("entries", []))
print(f"CC gate: {total} functions checked, {new_count} new, {failed} over allowed")

if failed:
    print(
        "Functions over the allowed CC must be reduced below the target, or the "
        "baseline must be deliberately regenerated (just change-risk-baseline) "
        "and the change reviewed against the complexity guardrails."
    )
    sys.exit(1)

print("PASS: No cyclomatic complexity exceedances")
PY
