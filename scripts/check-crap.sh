#!/usr/bin/env bash
set -euo pipefail

# Absolute CRAP gate.
#
# Enforces the CRAP targets in docs/guardrails/complexity-targets.md (ADR 039):
#   - No function may exceed the CRAP threshold (default 30).
#   - A function at or below the CC target (10) whose CRAP exceeds the warning
#     threshold (default 15) prints a non-failing warning.
#   - A function listed in the baseline with a `crap` allowance (a grandfather,
#     tracked by a reduction issue) may not exceed that allowance instead of the
#     threshold.
#
# CRAP is CC^2 * (1 - coverage)^3 + CC, so at 100% coverage it equals CC. This
# check replaces the former per-function CRAP delta ratchet against
# ci/cargo-crap-baseline.json: a delta gate whose tolerance was tighter than the
# documented CC target contradicted the per-function CC gate and required a
# baseline edit for growth the guardrail permits (#658).
#
# CRAP needs a coverage pass, so unlike scripts/check-cc.sh this gate runs from
# the LCOV file scripts/check-coverage.sh already produced.
#
# Usage:
#   scripts/check-crap.sh [--lcov lcov.info] [--baseline ci/cargo-crap-baseline.json]
#                         [--threshold 30] [--warn 15]

CC_TARGET=10

usage() {
    cat <<'EOF'
Usage: scripts/check-crap.sh [--lcov FILE] [--baseline FILE] [--threshold N] [--warn N]

Runs the per-function absolute CRAP gate.
  --lcov FILE       LCOV coverage file (omit to score every function at 0% coverage)
  --baseline FILE   JSON baseline; an entry's `crap` field is that function's
                    allowed ceiling (a grandfather)
                    (default: ci/cargo-crap-baseline.json)
  --threshold N     CRAP ceiling for functions without an allowance (default: 30)
  --warn N          Print a non-failing warning above this CRAP score for a
                    function at or below CC 10 (default: 15)
EOF
}

lcov=""
baseline="ci/cargo-crap-baseline.json"
threshold=30
warn=15

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
        --threshold)
            if [ "$#" -lt 2 ]; then
                echo "ERROR: --threshold requires a value" >&2
                exit 2
            fi
            threshold="$2"
            shift 2
            ;;
        --warn)
            if [ "$#" -lt 2 ]; then
                echo "ERROR: --warn requires a value" >&2
                exit 2
            fi
            warn="$2"
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

# The Coverage gate runs this check, so a missing tool must name its remedy
# instead of failing as an opaque exec error from scripts/cargo-crap.sh.
if ! command -v cargo-crap >/dev/null 2>&1; then
    echo "ERROR: cargo-crap not found — run \`just setup\` (installs cargo-crap 0.2.2)" >&2
    exit 1
fi

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

python3 - "$report" "$baseline" "$threshold" "$warn" "$CC_TARGET" <<'PY'
import json
import sys

report_path, baseline_path = sys.argv[1], sys.argv[2]
threshold, warn, cc_target = (float(value) for value in sys.argv[3:6])

with open(report_path) as f:
    report = json.load(f)
with open(baseline_path) as f:
    baseline = json.load(f)

# Function identity is (file, function). Multiple entries may share a name
# across generic instantiations; take the highest allowance as the binding one.
# Only an entry that carries `crap` is a grandfather; every other baseline entry
# documents a CC ceiling for scripts/check-cc.sh and sets no CRAP allowance.
allowance: dict[tuple[str, str], float] = {}
for entry in baseline.get("entries", []):
    if "crap" not in entry:
        continue
    key = (entry["file"], entry["function"])
    allowance[key] = max(allowance.get(key, 0.0), float(entry["crap"]))

failed = 0
grandfathered = 0
warned = 0
for entry in report.get("entries", []):
    key = (entry["file"], entry["function"])
    crap = float(entry.get("crap", 0.0))
    cc = float(entry.get("cyclomatic", 0.0))
    if key in allowance:
        allowed = allowance[key]
        grandfathered += 1
    else:
        allowed = threshold
    if crap > allowed:
        failed += 1
        print(
            f"FAIL: {entry['file']}: {entry['function']} "
            f"has CRAP {crap:g}, exceeding allowed {allowed:g}"
        )
    elif cc <= cc_target and crap > warn:
        warned += 1
        print(
            f"WARN: {entry['file']}: {entry['function']} "
            f"has CRAP {crap:g} above the {warn:g} stretch target (CC {cc:g})"
        )

total = len(report.get("entries", []))
print(
    f"CRAP gate: {total} functions checked, {grandfathered} grandfathered, "
    f"{warned} warned, {failed} over allowed"
)

if failed:
    print(
        "Functions above the allowed CRAP must be covered or simplified, or "
        "deliberately grandfathered in ci/cargo-crap-baseline.json with a "
        "reduction issue."
    )
    sys.exit(1)

print("PASS: No CRAP exceedances")
PY
