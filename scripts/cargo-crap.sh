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
    --exclude 'src/clients/agent/agent_tests.rs' \
    --exclude 'src/clients/chat_completions_tests.rs' \
    --exclude 'src/clients/responses_tests.rs' \
    --exclude 'src/clients/responses_response_parsing_tests.rs' \
    --exclude 'src/clients/tools/bash_tests.rs' \
    --exclude 'src/config/settings_tests.rs' \
    --exclude 'src/config/skills_tests.rs' \
    --exclude 'src/hooks_tests.rs' \
    --exclude 'src/main_tests.rs' \
    --exclude 'src/types/session_tests.rs' \
    --exclude 'src/clients/tools/sandbox/linux.rs'
