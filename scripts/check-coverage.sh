#!/usr/bin/env bash
set -euo pipefail

format="summary"

usage() {
    cat <<'EOF'
Usage: scripts/check-coverage.sh [--cargo-crap-format summary|github]

Runs the local/GitHub coverage and cargo-crap change-risk gate.
Set COVERAGE_THRESHOLD to override the default threshold of 90.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --cargo-crap-format)
            if [ "$#" -lt 2 ]; then
                echo "ERROR: --cargo-crap-format requires a value" >&2
                exit 2
            fi
            format="$2"
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

case "$format" in
    summary|github) ;;
    *)
        echo "ERROR: unsupported cargo-crap format: $format" >&2
        exit 2
        ;;
esac

threshold="${COVERAGE_THRESHOLD:-90}"
output="$(cargo llvm-cov --summary-only)"
printf '%s\n' "$output"

coverage="$(printf '%s\n' "$output" | grep "^TOTAL" | grep -oE '[0-9]+\.[0-9]+%' | tail -1 | tr -d '%')"
if [ -z "$coverage" ]; then
    echo "ERROR: could not read TOTAL coverage from cargo llvm-cov output" >&2
    exit 1
fi

echo "Coverage: ${coverage}%"
if [ "$(echo "$coverage < $threshold" | bc -l)" = "1" ]; then
    echo "Coverage below ${threshold}%"
    exit 1
fi

cargo llvm-cov --lcov --output-path lcov.info

if [ "$format" = "summary" ]; then
    scripts/cargo-crap.sh \
        --lcov lcov.info \
        --baseline ci/cargo-crap-baseline.json \
        --fail-regression \
        --summary
else
    scripts/cargo-crap.sh \
        --lcov lcov.info \
        --baseline ci/cargo-crap-baseline.json \
        --fail-regression \
        --format "$format"
fi
