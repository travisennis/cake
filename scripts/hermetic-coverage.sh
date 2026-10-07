#!/usr/bin/env bash
set -euo pipefail

# Run `cargo llvm-cov` with a scratch HOME.
#
# Coverage measurement must not depend on files outside the checkout. Some tests
# read the developer's home directory --- the skill loader scans
# `~/.agents/skills`, for example --- so the same source measures different
# coverage on a developer machine than on a clean CI runner, and
# `just change-risk-baseline` records values CI never reproduces (#699). Pointing
# HOME at an empty scratch directory while the suite runs makes the measurement
# hermetic: the run sees no developer state, matching the runner.
#
# CARGO_HOME and RUSTUP_HOME stay on the real install. cargo and rustup resolve
# them from their environment variables, falling back to `$HOME/.cargo` and
# `$HOME/.rustup`, so overriding HOME alone would hide the installed toolchain,
# registry, and caches. Restoring the pre-override values keeps the build
# identical while HOME changes. The derived defaults assume the incoming HOME is
# the developer's real home; a caller that passes a different HOME must set
# CARGO_HOME and RUSTUP_HOME itself.
#
# Usage: scripts/hermetic-coverage.sh <cargo llvm-cov arguments...>

real_home="${HOME:-}"
if [ -z "$real_home" ]; then
    echo "ERROR: HOME is unset; hermetic-coverage.sh needs it to locate CARGO_HOME and RUSTUP_HOME" >&2
    exit 1
fi

scratch_home="$(mktemp -d "${TMPDIR:-/tmp}/cake-coverage-home.XXXXXX")"
trap 'rm -rf "$scratch_home"' EXIT

export HOME="$scratch_home"
export CARGO_HOME="${CARGO_HOME:-$real_home/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$real_home/.rustup}"

cargo llvm-cov "$@"
