#!/usr/bin/env sh
# test-docs-corpus.sh — fixture tests for scripts/docs-corpus.sh.
#
# Builds scratch repositories and a stub panache, then asserts which files the
# documentation gates are given: tracked and staged Markdown in, untracked
# scratch notes out wherever they sit, an empty corpus invoking panache zero
# times, and a tracked bad document still failing the check. Run locally via
# `just docs-corpus-check` and in CI via the `changes` job in
# .github/workflows/ci.yml.

set -eu

# git exports repository-pinning variables (GIT_DIR and friends) into hook
# processes and everything those hooks spawn, and they outrank the working
# directory, so without this the scratch repositories below would resolve to the
# enclosing checkout instead. src/config/git.rs holds the list cake never
# inherits and tests/support/mod.rs strips the same set for the integration
# tests; these fixtures are the third place that spawns git. See
# scripts/test-fixture-isolation.sh.
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_PREFIX \
    GIT_CONFIG_PARAMETERS GIT_CONFIG GIT_CONFIG_COUNT \
    GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_AUTHOR_DATE \
    GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL GIT_COMMITTER_DATE

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
script="$here/scripts/docs-corpus.sh"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/docs-corpus-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fail() {
    echo "test-docs-corpus: FAIL: $*" >&2
    exit 1
}

stub="$tmp/stub"
mkdir "$stub"
PANACHE_LOG="$tmp/panache.log"
export PANACHE_LOG

# Stub panache: record one line per invocation and fail on a document named
# bad.md, so the fixture can prove that a tracked bad document still fails.
cat > "$stub/panache" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >> "$PANACHE_LOG"
for arg in "$@"; do
    case "$arg" in
        *bad.md) echo "panache: $arg is not formatted" >&2; exit 1 ;;
    esac
done
exit 0
EOF
chmod +x "$stub/panache"

run_corpus() { # <repo> <mode...>
    repo="$1"
    shift
    : > "$PANACHE_LOG"
    rc=0
    (cd "$repo" && PATH="$stub:$PATH" "$script" "$@") >"$tmp/out" 2>"$tmp/err" || rc=$?
}

new_repo() { # <name> — an empty repository with one commit
    repo="$tmp/$1"
    git init -q -b master "$repo"
    (
        cd "$repo"
        git config user.email test@example.com
        git config user.name "docs-corpus test"
        git config commit.gpgsign false
    )
    printf '%s\n' "$repo"
}

invocations() { grep -c . "$PANACHE_LOG" || true; }

# --- a repository with tracked, staged, untracked, and deleted Markdown ------

repo="$(new_repo clean)"
mkdir -p "$repo/docs"
printf '# ok\n' > "$repo/docs/ok.md"
printf '# gone\n' > "$repo/docs/gone.md"
(cd "$repo" && git add docs/ok.md docs/gone.md && git commit -qm base)
printf '# staged\n' > "$repo/docs/staged.md"
(cd "$repo" && git add docs/staged.md)
rm "$repo/docs/gone.md"
mkdir -p "$repo/scratch"
printf '#   unformatted scratch note\n\n* bullet\n' > "$repo/scratch/notes.md"

run_corpus "$repo" --list
[ "$rc" -eq 0 ] || fail "--list: expected success, got $rc: $(cat "$tmp/err")"
listed="$(tr '\0' '\n' < "$tmp/out" | sort | tr '\n' ' ')"
[ "$listed" = "docs/gone.md docs/ok.md docs/staged.md " ] \
    || fail "--list: expected the tracked and staged documents only, got: $listed"

run_corpus "$repo" --check
[ "$rc" -eq 0 ] || fail "--check: expected success, got $rc: $(cat "$tmp/err")"
[ "$(invocations)" -eq 2 ] || fail "--check: expected two panache invocations, got $(invocations)"
grep -q 'docs/ok.md' "$PANACHE_LOG" || fail "--check: tracked document missing from the corpus"
grep -q 'docs/staged.md' "$PANACHE_LOG" || fail "--check: staged document missing from the corpus"
grep -q 'notes.md' "$PANACHE_LOG" && fail "--check: untracked scratch note was passed to panache"
grep -q '^format --check --force-exclude --quiet' "$PANACHE_LOG" \
    || fail "--check: expected a format check invocation, got: $(head -1 "$PANACHE_LOG")"
grep -q '^lint --force-exclude --quiet' "$PANACHE_LOG" \
    || fail "--check: expected a lint invocation, got: $(tail -1 "$PANACHE_LOG")"

run_corpus "$repo" --fmt
[ "$rc" -eq 0 ] || fail "--fmt: expected success, got $rc: $(cat "$tmp/err")"
[ "$(invocations)" -eq 1 ] || fail "--fmt: expected one panache invocation, got $(invocations)"
[ "$(cat "$PANACHE_LOG")" = "format docs/gone.md docs/ok.md docs/staged.md" ] \
    || fail "--fmt: unexpected argv: $(cat "$PANACHE_LOG")"
[ -f "$repo/scratch/notes.md" ] || fail "--fmt: scratch note disappeared"

run_corpus "$repo" --lint
[ "$rc" -eq 0 ] || fail "--lint: expected success, got $rc: $(cat "$tmp/err")"
[ "$(invocations)" -eq 1 ] || fail "--lint: expected one panache invocation, got $(invocations)"
[ "$(cat "$PANACHE_LOG")" = "lint --force-exclude --quiet docs/gone.md docs/ok.md docs/staged.md" ] \
    || fail "--lint: unexpected argv: $(cat "$PANACHE_LOG")"

# --- a tracked document that fails the check ---------------------------------

repo="$(new_repo bad)"
mkdir -p "$repo/docs"
printf '# bad\n' > "$repo/docs/bad.md"
(cd "$repo" && git add docs/bad.md && git commit -qm base)

run_corpus "$repo" --check
[ "$rc" -ne 0 ] || fail "--check: a tracked bad document must fail the check"
grep -q 'is not formatted' "$tmp/err" || fail "--check: expected panache's failure on stderr, got: $(cat "$tmp/err")"

# --- a repository with no Markdown at all ------------------------------------

repo="$(new_repo empty)"

run_corpus "$repo" --check
[ "$rc" -eq 0 ] || fail "empty corpus: expected success, got $rc: $(cat "$tmp/err")"
[ "$(invocations)" -eq 0 ] || fail "empty corpus: panache must not be invoked"
grep -q 'no tracked Markdown' "$tmp/out" || fail "empty corpus: expected an explanatory line, got: $(cat "$tmp/out")"

# --- invocation --------------------------------------------------------------

run_corpus "$repo" --bogus
[ "$rc" -eq 2 ] || fail "unknown mode: expected exit 2, got $rc"
grep -q 'unknown mode' "$tmp/err" || fail "unknown mode: expected a message on stderr"

echo "test-docs-corpus: all cases passed"
