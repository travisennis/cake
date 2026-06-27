#!/usr/bin/env sh
set -eu

exec cargo crap "$@" \
    --exclude 'tests/**' \
    --exclude 'src/clients/agent/agent_tests.rs' \
    --exclude 'src/clients/chat_completions_tests.rs' \
    --exclude 'src/config/settings_tests.rs' \
    --exclude 'src/clients/tools/sandbox/linux.rs'
