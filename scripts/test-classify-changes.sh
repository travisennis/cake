#!/usr/bin/env sh
# test-classify-changes.sh — fixture tests for scripts/classify-changes.sh.
#
# Builds a scratch repository and asserts the documented classification for
# each changed-file matrix (docs / code / mixed / unknown / none), the --files
# exclusions (code-class .md, deletions, paths containing spaces), the
# fail-closed unresolvable base, and the --check whitespace gate over the
# classifier's range. Run in CI via the `changes` job in
# .github/workflows/ci.yml and locally via `just test-classify-changes`.

set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
script="$here/scripts/classify-changes.sh"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/classify-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fail() {
    echo "test-classify-changes: FAIL: $*" >&2
    exit 1
}

git init -q -b master "$tmp/repo"
cd "$tmp/repo"
git config user.email test@example.com
git config user.name "Classifier Test"
# Pin the whitespace policy so the --check case does not depend on the
# runner's global core.whitespace.
git config core.whitespace blank-at-eol
printf 'base\n' > base.txt
mkdir -p docs
printf 'base doc\n' > docs/deleted.md
git add base.txt docs/deleted.md
git commit -qm "base"

# commit_files <branch> <path> <content> [<path> <content> ...]
commit_files() {
    branch="$1"
    shift
    git checkout -q -b "$branch"
    while [ "$#" -gt 0 ]; do
        path="$1"; content="$2"; shift 2
        mkdir -p "$(dirname -- "$path")"
        printf '%s\n' "$content" > "$path"
        git add "$path"
    done
    git commit -qm "add $branch"
    git checkout -q master
}

class_of() { "$script" --base master --head "$1"; }
files_of() { "$script" --base master --head "$1" --files; }

expect_class() { # <branch> <expected-class>
    actual="$(class_of "$1")"
    [ "$actual" = "$2" ] || fail "branch '$1': expected class '$2', got '$actual'"
}

expect_files() { # <branch> <expected-files>
    # --files tests worktree existence (deletions are skipped), so run the
    # assertion with the branch checked out, as a real pre-push would.
    git checkout -q "$1"
    actual="$(files_of "$1")"
    git checkout -q master
    [ "$actual" = "$2" ] || fail "branch '$1': expected --files '$2', got '$actual'"
}

# docs: every changed file is Markdown
commit_files docs-1 docs/guide.md "guide"
expect_class docs-1 docs
expect_files docs-1 "docs/guide.md"

# code: every changed file is a code-class path
commit_files code-1 src/lib.rs "pub fn f() {}"
expect_class code-1 code
expect_files code-1 ""

# scripts: every changed file in scripts/ is a code-class path
commit_files scripts-1 scripts/helper.sh "helper"
expect_class scripts-1 code
expect_files scripts-1 ""

# mixed: both Markdown and code paths changed
commit_files mixed-1 scripts/helper.sh "helper" docs/readme.md "readme"
expect_class mixed-1 mixed
expect_files mixed-1 "docs/readme.md"

# unknown: a changed file in neither class fails closed
commit_files unknown-1 prek.toml "key = \"value\""
expect_class unknown-1 unknown
expect_files unknown-1 ""

# Markdown paths containing spaces survive --files (regression guard for the
# pre-push-docs quoting fix)
commit_files spaces-1 "docs/my file.md" "spaced path"
expect_class spaces-1 docs
expect_files spaces-1 "docs/my file.md"

# --files excludes code-class .md (src/prompts/) and skips deletions:
# docs/deleted.md exists on master and is removed on files-1, so the deletion
# is part of the master...files-1 diff and the [ -f ] guard is truly exercised.
git checkout -q -b files-1
mkdir -p src/prompts
printf 'a\n' > docs/a.md
printf 'code markdown\n' > src/prompts/system.md
git add docs/a.md src/prompts/system.md
git commit -qm "add files-1"
git rm -q docs/deleted.md
git commit -qm "delete docs/deleted.md"
git checkout -q master
expect_class files-1 mixed
expect_files files-1 "docs/a.md"

# none: no changed files between base and head
expect_class master none

# Unresolvable base (all-zeros sha) fails closed to unknown in class mode and
# skips silently in --files and --check modes
zeros="0000000000000000000000000000000000000000"
set +e
zeros_out="$("$script" --base "$zeros" --head master)"
zeros_rc=$?
set -e
[ "$zeros_rc" -eq 0 ] || fail "unresolvable base: expected exit 0, got $zeros_rc"
[ "$zeros_out" = "unknown" ] || fail "unresolvable base: expected 'unknown', got '$zeros_out'"

set +e
zeros_files="$(files_of "$zeros")"
zeros_files_rc=$?
set -e
[ "$zeros_files_rc" -eq 0 ] || fail "unresolvable base --files: expected exit 0, got $zeros_files_rc"
[ -z "$zeros_files" ] || fail "unresolvable base --files: expected no output, got '$zeros_files'"

# --check: passes on a clean pushed range, fails on committed trailing whitespace
"$script" --base master --head docs-1 --check

commit_files ws-1 docs/ws.md "trailing   "
set +e
"$script" --base master --head ws-1 --check >/dev/null 2>&1
ws_rc=$?
set -e
[ "$ws_rc" -ne 0 ] || fail "--check: expected failure on committed trailing whitespace"

set +e
"$script" --base "$zeros" --head master --check >/dev/null 2>&1
zeros_check_rc=$?
set -e
[ "$zeros_check_rc" -eq 0 ] || fail "--check unresolvable base: expected exit 0, got $zeros_check_rc"

# Unknown argument exits 2
set +e
"$script" --bogus >/dev/null 2>&1
bogus_rc=$?
set -e
[ "$bogus_rc" -eq 2 ] || fail "unknown argument: expected exit 2, got $bogus_rc"

echo "test-classify-changes: all classification, --files, and --check cases passed"
