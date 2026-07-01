#!/usr/bin/env sh
set -eu

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
