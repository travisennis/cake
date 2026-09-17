#!/usr/bin/env bash
set -euo pipefail

# Remove coverage artifacts from an earlier run and prove the directory is clean.
#
# `cargo llvm-cov clean` cannot be trusted to remove them on its own: `--profraw-only`
# keeps the merged `.profdata`, and `--workspace` keeps the previous run's profraw
# list (measured 2026-09-17). Residue merges into the next run and reports a false
# total (#520), so this runs the strongest clean, removes what the clean left through
# the guard's documented policy, and re-runs the guard so a caller never measures
# coverage without proving the directory is clean first.
#
# Coverage artifacts live in cargo-llvm-cov's target directory, which depends on the
# environment: `CARGO_LLVM_COV_TARGET_DIR` when set (artifacts directly inside it),
# otherwise `<cargo target dir>/llvm-cov-target`. `scripts/coverage-guard.py` accepts
# a missing directory, so deriving the wrong path would make the guard pass without
# checking anything.
#
# Usage: scripts/coverage-clean.sh

profiles_dir="${CARGO_LLVM_COV_TARGET_DIR:-${CARGO_TARGET_DIR:-target}/llvm-cov-target}"

cargo llvm-cov clean --workspace
python3 scripts/coverage-guard.py --profiles-dir "$profiles_dir" --remove-residual
python3 scripts/coverage-guard.py --profiles-dir "$profiles_dir"
