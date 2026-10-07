#!/usr/bin/env sh
set -eu

exec cargo crap "$@" \
    --exclude 'tests/**' \
    --exclude '**/*_tests.rs' \
    --exclude 'src/clients/tools/sandbox/linux.rs'
