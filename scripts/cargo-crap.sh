#!/usr/bin/env sh
set -eu

# Report per-function cyclomatic complexity. The Linux sandbox module is
# excluded because its platform-only branches inflate the report.
exec cargo crap "$@" --exclude 'src/clients/tools/sandbox/linux.rs'
