#!/usr/bin/env sh
# test-dev-tools.sh — fixture tests for scripts/dev-tools.sh.
#
# A stub cargo keeps these fixtures off the machine's real toolchain. Covers an
# all-present pass, a missing package, an installed package whose binary is
# gone, a version mismatch, --install recovery, and an aggregated failure when
# one install fails while a later install succeeds. Run in CI's `changes` job
# and locally via `just test-dev-tools`.

set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
script="$here/scripts/dev-tools.sh"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/dev-tools-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

stub_bin="$tmp/stub-bin"
state="$tmp/state"
list_dir="$state/list"
cargo_home="$state/cargo-home"
calls="$state/install-calls"
mkdir -p "$stub_bin" "$list_dir" "$cargo_home/bin"
: > "$calls"

cat > "$stub_bin/cargo" <<'STUB'
#!/usr/bin/env sh
set -eu
list_dir=${FAKE_LIST_DIR:?}
cargo_home=${FAKE_CARGO_HOME:?}
case "${1:-}" in
    install)
        shift
        if [ "${1:-}" = "--list" ]; then
            for entry in "$list_dir"/*; do
                if [ -e "$entry" ]; then
                    cat "$entry"
                fi
            done
            exit 0
        fi
        package=$1
        shift
        version=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --version)
                    version=$2
                    shift 2
                    ;;
                *)
                    shift
                    ;;
            esac
        done
        printf '%s %s\n' "$package" "$version" >> "${FAKE_INSTALL_CALLS:?}"
        case " ${FAKE_FAIL_PACKAGES:-} " in
            *" $package "*)
                echo "stub cargo: install failed for $package" >&2
                exit 1
                ;;
        esac
        case "$package" in
            cargo-edit) binary=cargo-add ;;
            cocogitto) binary=cog ;;
            *) binary=$package ;;
        esac
        : > "$cargo_home/bin/$binary"
        chmod +x "$cargo_home/bin/$binary"
        printf '%s v%s:\n    %s\n' "$package" "$version" "$binary" > "$list_dir/$package"
        ;;
    *)
        echo "stub cargo: unsupported command: $*" >&2
        exit 2
        ;;
esac
STUB
chmod +x "$stub_bin/cargo"

fail() {
    echo "test-dev-tools: FAIL: $*" >&2
    exit 1
}

contains() { printf '%s\n' "$1" | grep -q -- "$2"; }
expect_contains() { contains "$1" "$2" || fail "expected output to contain: $2"; }
expect_missing() { if contains "$1" "$2"; then fail "expected output not to contain: $2"; fi; }
expect_status() { [ "$1" -eq "$2" ] || fail "expected exit $2, got $1"; }

seed() { # <package> <version> <binary>
    printf '%s v%s:\n    %s\n' "$1" "$2" "$3" > "$list_dir/$1"
    : > "$cargo_home/bin/$3"
    chmod +x "$cargo_home/bin/$3"
}

seed_all() {
    seed cargo-edit 0.13.13 cargo-add
    seed cargo-deny 0.20.2 cargo-deny
    seed cargo-insta 1.48.0 cargo-insta
    seed cargo-llvm-cov 0.9.0 cargo-llvm-cov
    seed cargo-crap 0.2.2 cargo-crap
    seed panache 3.0.0 panache
    seed prek 0.4.13 prek
    seed cocogitto 7.0.0 cog
}

reset_state() {
    rm -rf "$list_dir" "$cargo_home"
    mkdir -p "$list_dir" "$cargo_home/bin"
    : > "$calls"
}

run_check() {
    FAKE_LIST_DIR="$list_dir" \
    FAKE_CARGO_HOME="$cargo_home" \
    FAKE_INSTALL_CALLS="$calls" \
    CARGO_HOME="$cargo_home" \
    PATH="$stub_bin:$PATH" \
        "$script" "$@"
}

# All pinned tools present: pass and name each tool.
reset_state
seed_all
output=$(run_check)
expect_contains "$output" "ok: cargo-deny 0.20.2 (cargo-deny)"
expect_contains "$output" "ok: cargo-edit 0.13.13 (cargo-add)"
expect_contains "$output" "ok: cocogitto 7.0.0 (cog)"

# Missing package: fail with the package, its version, and the install hint.
reset_state
seed_all
rm "$list_dir/cargo-deny" "$cargo_home/bin/cargo-deny"
set +e
stderr=$(run_check 2>&1 >/dev/null)
rc=$?
set -e
expect_status "$rc" 1
expect_contains "$stderr" "cargo-deny 0.20.2: not installed"
expect_contains "$stderr" "cargo install cargo-deny --version 0.20.2 --locked"
expect_missing "$stderr" "cargo install cargo-crap"

# Package recorded but binary missing: fail as a broken install.
reset_state
seed_all
rm "$cargo_home/bin/cargo-crap"
set +e
stderr=$(run_check 2>&1 >/dev/null)
rc=$?
set -e
expect_status "$rc" 1
expect_contains "$stderr" "package is installed but $cargo_home/bin/cargo-crap is missing"

# Wrong version: fail read-only, then recover with --install.
reset_state
seed_all
seed cargo-deny 0.19.0 cargo-deny
set +e
stderr=$(run_check 2>&1 >/dev/null)
rc=$?
set -e
expect_status "$rc" 1
expect_contains "$stderr" "cargo-deny 0.20.2: installed at 0.19.0"
run_check --install > /dev/null || fail "--install failed to recover the wrong version"
expect_contains "$(cat "$calls")" "cargo-deny 0.20.2"
run_check > /dev/null || fail "--install should restore a passing check"

# One install fails: aggregate the failure, keep installing the rest, and exit 1.
reset_state
seed_all
rm "$list_dir/cargo-deny" "$cargo_home/bin/cargo-deny"
rm "$list_dir/panache" "$cargo_home/bin/panache"
FAKE_FAIL_PACKAGES=cargo-deny
export FAKE_FAIL_PACKAGES
set +e
stderr=$(run_check --install 2>&1 >/dev/null)
rc=$?
set -e
unset FAKE_FAIL_PACKAGES
expect_status "$rc" 1
expect_contains "$stderr" "cargo-deny 0.20.2: the install command failed; not installed"
expect_contains "$stderr" "cargo install cargo-deny --version 0.20.2 --locked"
expect_missing "$stderr" "cargo install panache"
[ -f "$list_dir/panache" ] || fail "panache install should have run after the cargo-deny failure"
set +e
run_check >/dev/null 2>&1
rc=$?
set -e
expect_status "$rc" 1

# Unknown argument exits 2.
set +e
run_check --bogus >/dev/null 2>&1
rc=$?
set -e
expect_status "$rc" 2

echo "test-dev-tools: all fixture cases passed"
