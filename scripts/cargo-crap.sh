#!/usr/bin/env sh
set -eu

epsilon="${CRAP_REGRESSION_EPSILON:-0.5}"
has_epsilon=0

for arg in "$@"; do
    case "$arg" in
        --epsilon|--epsilon=*)
            has_epsilon=1
            ;;
    esac
done

if [ "$has_epsilon" -eq 0 ]; then
    set -- "$@" --epsilon "$epsilon"
fi

exec cargo crap "$@" \
    --exclude 'tests/**' \
    --exclude '**/*_tests.rs' \
    --exclude 'src/clients/tools/sandbox/linux.rs'
