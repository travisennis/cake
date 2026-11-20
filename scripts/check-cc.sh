#!/usr/bin/env bash
set -euo pipefail

# Cyclomatic-complexity (CC) gate.
#
# Enforces the complexity targets in docs/guardrails/complexity-targets.md:
#   - A function not listed in the baseline may not exceed the CC target.
#   - A function listed in the baseline (a grandfather) may not exceed the
#     greater of the CC target and the CC it had when it was recorded.
#     The committed baseline lists only the grandfathers, because for a
#     function at or below the target both ceilings are the target.
#
# Usage:
#   scripts/check-cc.sh [--baseline ci/complexity-baseline.json] [--target 10]

usage() {
    cat <<'EOF'
Usage: scripts/check-cc.sh [--baseline FILE] [--target N]

Runs the per-function cyclomatic-complexity gate.
  --baseline FILE  JSON baseline with per-function cyclomatic values
                   (default: ci/complexity-baseline.json)
  --target N       CC ceiling for functions absent from the baseline (default: 10)
EOF
}

baseline="ci/complexity-baseline.json"
target=10

while [ "$#" -gt 0 ]; do
    case "$1" in
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

# The fast local gate runs this check, so a missing tool must name its remedy
# instead of failing as an opaque exec error from scripts/cargo-crap.sh.
if ! command -v cargo-crap >/dev/null 2>&1; then
    echo "ERROR: cargo-crap not found — run \`just setup\` (installs cargo-crap 0.2.2)" >&2
    exit 1
fi

if [ ! -f "$baseline" ]; then
    echo "ERROR: baseline not found: $baseline" >&2
    exit 1
fi

report="$(mktemp)"
trap 'rm -f "$report"' EXIT

scripts/cargo-crap.sh --format json > "$report"

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
grandfathered = 0
for entry in report.get("entries", []):
    key = (entry["file"], entry["function"])
    cc = float(entry["cyclomatic"])
    if key in baseline_cc:
        allowed = max(float(target), baseline_cc[key])
        status = "grandfathered"
        grandfathered += 1
    else:
        allowed = float(target)
        status = "unlisted"
    if cc > allowed:
        failed += 1
        print(
            f"FAIL: {entry['file']}: {entry['function']} "
            f"has cyclomatic complexity {cc:g}, exceeding allowed {allowed:g} "
            f"({status}; target {target:g})"
        )

total = len(report.get("entries", []))
print(f"CC gate: {total} functions checked, {grandfathered} grandfathered, {failed} over allowed")

if failed:
    print(
        "Functions over the allowed CC must be reduced to their allowed ceiling, "
        "or the baseline entry must be raised deliberately and reviewed against "
        "the complexity guardrails."
    )
    sys.exit(1)

print("PASS: No cyclomatic complexity exceedances")
PY
