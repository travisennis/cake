#!/usr/bin/env sh
# test-hermetic-coverage.sh — fixture tests for scripts/hermetic-coverage.sh.
#
# The recipe that produces ci/cargo-crap-baseline.json must not measure coverage
# through the developer's real home directory: a test that reads, say,
# ~/.agents/skills makes the baseline record values the CI runner never
# measures (#699). A stub `cargo` records the environment the wrapper exposes,
# and each case asserts the contract that keeps measurement hermetic: HOME
# points at an empty scratch directory that is removed afterward, CARGO_HOME and
# RUSTUP_HOME keep the real install, the arguments pass through unchanged, and
# the exit code propagates. Run in CI via the `changes` job in
# .github/workflows/ci.yml and locally via `just hermetic-coverage-check`.

set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
script="$here/scripts/hermetic-coverage.sh"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/hermetic-coverage-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fail() {
    echo "test-hermetic-coverage: FAIL: $*" >&2
    exit 1
}

stub_bin="$tmp/bin"
mkdir "$stub_bin"
cat >"$stub_bin/cargo" <<'EOF'
#!/bin/sh
# Record what the wrapper exposed, then exit with the code the case asks for
# (default 0). The HOME listing is what a real leaking test would read, so an
# empty listing is the property under test.
printf '%s\n' "$HOME" >"$PROBE_HOME"
find "$HOME" -mindepth 1 >"$PROBE_HOME_CONTENTS"
printf '%s\n' "${CARGO_HOME-<unset>}" >"$PROBE_CARGO_HOME"
printf '%s\n' "${RUSTUP_HOME-<unset>}" >"$PROBE_RUSTUP_HOME"
printf '%s\n' "$*" >"$PROBE_ARGS"
exit "${PROBE_EXIT:-0}"
EOF
chmod +x "$stub_bin/cargo"

# A real-style home that would leak if the wrapper did not override HOME: it
# holds a populated ~/.agents/skills, the tree the skill loader scans.
populated_home="$tmp/populated-home"
mkdir -p "$populated_home/.agents/skills/debugging"
printf -- '---\nname: debugging\ndescription: has a colon: here\n---\n' \
    >"$populated_home/.agents/skills/debugging/SKILL.md"
empty_home="$tmp/empty-home"
mkdir "$empty_home"

# run_case <name> <home> <expected exit>; env for the invocation comes from the
# caller through the exported PROBE_* variables.
probe_home="$tmp/probe-home"
probe_contents="$tmp/probe-contents"
probe_cargo_home="$tmp/probe-cargo-home"
probe_rustup_home="$tmp/probe-rustup-home"
probe_args="$tmp/probe-args"

run_case() { # <name> <home> <expected exit>
    name="$1"
    home="$2"
    expected="$3"
    rm -f "$probe_home" "$probe_contents" "$probe_cargo_home" "$probe_rustup_home" "$probe_args"
    set +e
    PATH="$stub_bin:$PATH" HOME="$home" PROBE_HOME="$probe_home" \
        PROBE_HOME_CONTENTS="$probe_contents" PROBE_CARGO_HOME="$probe_cargo_home" \
        PROBE_RUSTUP_HOME="$probe_rustup_home" PROBE_ARGS="$probe_args" \
        "$script" --lcov --output-path lcov.info >"$tmp/out" 2>"$tmp/err"
    status=$?
    set -e
    [ "$status" -eq "$expected" ] \
        || fail "$name: expected exit $expected, got $status: $(cat "$tmp/err")"

    seen_home="$(cat "$probe_home")"
    [ "$seen_home" != "$home" ] \
        || fail "$name: cargo saw the real HOME ($seen_home); the run was not hermetic"
    case "$seen_home" in
        "${TMPDIR:-/tmp}"/*) ;;
        *) fail "$name: scratch HOME '$seen_home' is not under TMPDIR" ;;
    esac
    [ ! -e "$seen_home" ] \
        || fail "$name: scratch HOME '$seen_home' was not removed after the run"
    [ ! -s "$probe_contents" ] \
        || fail "$name: scratch HOME was not empty: $(cat "$probe_contents")"
    [ "$(cat "$probe_args")" = "llvm-cov --lcov --output-path lcov.info" ] \
        || fail "$name: arguments were not passed through: $(cat "$probe_args")"
}

# Case 1: a populated real home. The scratch view must still be empty, so a test
# that scans HOME cannot see the developer's skills.
export CARGO_HOME="$populated_home/cargo"
export RUSTUP_HOME="$populated_home/rustup"
run_case "populated home" "$populated_home" 0
[ "$(cat "$probe_cargo_home")" = "$CARGO_HOME" ] \
    || fail "populated home: CARGO_HOME was not preserved: $(cat "$probe_cargo_home")"
[ "$(cat "$probe_rustup_home")" = "$RUSTUP_HOME" ] \
    || fail "populated home: RUSTUP_HOME was not preserved: $(cat "$probe_rustup_home")"
unset CARGO_HOME RUSTUP_HOME

# Case 2: an empty real home produces the identical (empty) scratch view, so
# both runs are home-independent.
run_case "empty home" "$empty_home" 0

# Case 3: with CARGO_HOME and RUSTUP_HOME unset, they resolve to the real
# install's defaults ($HOME/.cargo and $HOME/.rustup), not the scratch HOME.
env -u CARGO_HOME -u RUSTUP_HOME \
    PATH="$stub_bin:$PATH" HOME="$empty_home" PROBE_HOME="$probe_home" \
    PROBE_HOME_CONTENTS="$probe_contents" PROBE_CARGO_HOME="$probe_cargo_home" \
    PROBE_RUSTUP_HOME="$probe_rustup_home" PROBE_ARGS="$probe_args" \
    "$script" --lcov >/dev/null 2>&1
[ "$(cat "$probe_cargo_home")" = "$empty_home/.cargo" ] \
    || fail "unset CARGO_HOME: expected $empty_home/.cargo, got $(cat "$probe_cargo_home")"
[ "$(cat "$probe_rustup_home")" = "$empty_home/.rustup" ] \
    || fail "unset RUSTUP_HOME: expected $empty_home/.rustup, got $(cat "$probe_rustup_home")"

# Case 4: a failing measurement still propagates its exit code.
export PROBE_EXIT=7
run_case "exit propagation" "$empty_home" 7
unset PROBE_EXIT

# Case 5: an unset HOME is a hard error rather than an empty-path CARGO_HOME.
set +e
env -u HOME "$script" --lcov >"$tmp/out" 2>"$tmp/err"
status=$?
set -e
[ "$status" -eq 1 ] || fail "unset HOME: expected exit 1, got $status"
grep -q "HOME is unset" "$tmp/err" || fail "unset HOME: missing diagnostic: $(cat "$tmp/err")"

echo "test-hermetic-coverage: all hermeticity cases passed"
