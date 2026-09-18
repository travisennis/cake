#!/usr/bin/env sh
# test-fixture-isolation.sh — regression guard for the suites that build a
# scratch repository.
#
# git exports repository-pinning variables (GIT_DIR and friends) into hook
# processes and everything those hooks spawn, and the exported path is absolute,
# so a suite that builds its own repository has to drop them or every git
# command in it resolves to the checkout the gate was started from. That is the
# normal case for a push from a linked worktree: `just worktree` puts the
# pre-push gate behind a hook, and the hook hands the fixtures a GIT_DIR that
# points at the enclosing checkout. Observed before this guard existed: the
# enclosing config gained the fixture's core.bare and user.*, the enclosing
# index gained the fixture's files, and the evaluation harness moved the
# enclosing branch onto its own scratch commits.
#
# Each case below runs a suite with the environment a hook exports — cwd at the
# repository root, GIT_DIR set to a scratch repository's worktree gitdir — and
# asserts the suite succeeds and leaves that scratch repository untouched. The
# scratch repository is the target a missing neutralization would hit, so the
# guard cannot damage the checkout it runs in. Run locally via
# `just fixture-isolation-check` and in CI via the `changes` job in
# .github/workflows/ci.yml.

set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

# This guard is itself reached from the environment it describes, so it must not
# inherit one: its own git commands act on the checkout it lives in, and each
# case below sets GIT_DIR explicitly for one suite at a time.
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_PREFIX \
    GIT_CONFIG_PARAMETERS GIT_CONFIG GIT_CONFIG_COUNT \
    GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_AUTHOR_DATE \
    GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL GIT_COMMITTER_DATE

tmp="$(mktemp -d "${TMPDIR:-/tmp}/fixture-isolation-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fail() {
    echo "test-fixture-isolation: FAIL: $*" >&2
    exit 1
}

# --- the checkout a hook hands to a suite ------------------------------------

outer="$tmp/outer"
git init -q -b master "$outer"
(
    cd "$outer"
    git config user.email outer@example.com
    git config user.name "Enclosing Checkout"
    printf 'outer\n' > outer.txt
    git add outer.txt
    git commit -qm outer
)

# A linked worktree, so GIT_DIR resolves to a worktree gitdir the way it does
# under `just worktree` — the shape that also made git treat a re-initialized
# repository as bare.
worktree="$tmp/outer-worktree"
git -C "$outer" worktree add -q --detach "$worktree"
hook_git_dir="$(git -C "$worktree" rev-parse --absolute-git-dir)"

# The state a suite must leave alone: config bytes, index, HEAD, refs, and the
# tracked worktree.
snapshot() {
    printf 'config:\n'
    cat "$outer/.git/config"
    printf 'index:\n'
    git -C "$worktree" ls-files -s
    printf 'head:\n'
    git -C "$worktree" rev-parse HEAD
    printf 'refs:\n'
    git -C "$worktree" for-each-ref --format='%(refname) %(objectname)'
    printf 'status:\n'
    git -C "$worktree" status --porcelain
}

run_case() { # <name> <command...> — run it as a hook would, require a clean result
    name="$1"
    shift
    before="$(snapshot)"
    rc=0
    (cd "$here" && GIT_DIR="$hook_git_dir" "$@") >"$tmp/out" 2>"$tmp/err" || rc=$?
    [ "$rc" -eq 0 ] \
        || fail "$name: expected success under the hook environment, got exit $rc: $(tail -n 3 "$tmp/err")"
    after="$(snapshot)"
    if [ "$before" != "$after" ]; then
        printf '%s' "$before" >"$tmp/before"
        printf '%s' "$after" >"$tmp/after"
        diff -u "$tmp/before" "$tmp/after" | head -n 20 >&2
        fail "$name: changed the enclosing checkout under the hook environment"
    fi
    echo "test-fixture-isolation: $name leaves the enclosing checkout alone"
}

# --- the shell fixtures that build a scratch repository ----------------------

self="$here/scripts/test-fixture-isolation.sh"
covered=0
for fixture in "$here"/scripts/test-*.sh; do
    # Skip this guard: it runs `git init` for its own scratch checkout, and
    # running it from itself would recurse.
    if [ "$fixture" = "$self" ]; then continue; fi
    # Only the fixtures that build a scratch repository can resolve to the
    # enclosing checkout; the rest read the repository they live in, which under
    # a hook is the same repository.
    grep -q 'git init' "$fixture" || continue
    run_case "$(basename "$fixture")" "$fixture"
    covered=$((covered + 1))
done

[ "$covered" -gt 0 ] || fail "no fixture with 'git init' was found; the guard checked nothing"

# --- the evaluation harness, which builds a repository per case --------------

run_case "eval-check" python3 -m unittest discover -s scripts/evals/tests

echo "test-fixture-isolation: all cases passed"
