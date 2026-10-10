#!/usr/bin/env bash
# test-just-pr.sh — fixture tests for the `just pr` recipe.
#
# Stubs `gh` and `ahm` on PATH and drives the real recipe through `just`, asserting the
# exact argv each stubbed call receives: fail-fast ordering before creation,
# label normalization into the single CSV element gh parses, the title/--fill
# interaction (explicit title wins; body file falls back to the HEAD subject),
# the base default and the `base=` override for stacked pull requests, and the
# task comment-back. Run locally via `just test-just-pr` and in CI via the
# `changes` job in .github/workflows/ci.yml.

set -euo pipefail

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
sep="$(printf '\037')"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/just-pr-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fail() {
    echo "test-just-pr: FAIL: $*" >&2
    exit 1
}

stub_dir="$tmp/stub"
mkdir "$stub_dir"
GH_LOG="$tmp/gh.log"
OUT_LOG="$tmp/out.log"
ERR_LOG="$tmp/err.log"
export GH_LOG

# Stub gh: log one line per invocation, arguments separated by \037, then act.
cat > "$stub_dir/gh" <<'EOF'
#!/usr/bin/env bash
for arg in "$@"; do
    printf '%s\037' "$arg" >> "$GH_LOG"
done
printf '\n' >> "$GH_LOG"
if [ "${1:-} ${2:-}" = "pr create" ]; then
    if [[ " $* " == *" fail-create "* ]]; then
        echo "remote creation failed" >&2
        exit 1
    fi
    printf 'https://example.com/pr/1\n'
fi
exit 0
EOF
chmod +x "$stub_dir/gh"

# ahm shares the call log so fixtures pin validation/create/comment ordering.
cat > "$stub_dir/ahm" <<'EOF'
#!/usr/bin/env bash
for arg in "$@"; do
    printf '%s\037' "$arg" >> "$GH_LOG"
done
printf '\n' >> "$GH_LOG"
if [ "${1:-} ${2:-} ${4:-}" = "task show missing" ]; then
    echo "task not found: missing" >&2
    exit 1
fi
if [ "${1:-} ${2:-} ${4:-}" = "task comment fail-comment" ]; then
    echo "store write failed" >&2
    exit 1
fi
EOF
chmod +x "$stub_dir/ahm"

run_pr() { # <option>... — run `just pr` from the repository root with the stub on PATH
    : > "$GH_LOG"; : > "$OUT_LOG"; : > "$ERR_LOG"
    rc=0
    (cd -- "$here" && PATH="$stub_dir:$PATH" just --quiet pr "$@") >"$OUT_LOG" 2>"$ERR_LOG" || rc=$?
}

invocations() { grep -c . "$GH_LOG" || true; }

show_args() { printf '%s' "$1" | tr '\037' '|'; }

expect_invocation() { # <index> <expected argv, sep-separated>
    actual="$(sed -n "${1}p" "$GH_LOG")"
    if [ "$actual" != "$2" ]; then
        fail "tool invocation $1: expected '$(show_args "$2")', got '$(show_args "$actual")'"
    fi
}

expect_no_creation() {
    [ "$(invocations)" -eq 0 ] || fail "expected gh to never be called, got $(invocations) invocation(s): $(tr '\037' '|' < "$GH_LOG")"
}

expect_failure() { # <context> — assert nonzero exit with stderr context
    [ "$rc" -ne 0 ] || fail "$1: expected failure, got success"
    grep -q "$2" "$ERR_LOG" || fail "$1: expected stderr to mention '$2', got: $(cat "$ERR_LOG")"
}

head_subject="$(git -C "$here" log -1 --pretty=%s)"

# No options: plain --fill, nothing else, URL echoed
run_pr
[ "$rc" -eq 0 ] || fail "no options: expected success, got $rc: $(cat "$ERR_LOG")"
expect_invocation 1 "pr${sep}create${sep}--base${sep}master${sep}--fill${sep}"
[ "$(invocations)" -eq 1 ] || fail "no options: expected one gh invocation"
grep -q "https://example.com/pr/1" "$OUT_LOG" || fail "no options: expected URL on stdout"

# base= retargets a stacked pull request at the branch below it
run_pr "base=feat/stacked-below"
[ "$rc" -eq 0 ] || fail "base override: expected success, got $rc: $(cat "$ERR_LOG")"
expect_invocation 1 "pr${sep}create${sep}--base${sep}feat/stacked-below${sep}--fill${sep}"

# An empty base fails before creation
run_pr "base="
expect_failure "empty base" "base= requires a ref"
expect_no_creation

# Explicit title without body: kept alongside --fill (title wins over autofill)
run_pr "title=My Title"
expect_invocation 1 "pr${sep}create${sep}--base${sep}master${sep}--fill${sep}--title${sep}My Title${sep}"

# Spaced CSV labels reach gh as one trimmed element (regression guard)
run_pr 'labels=type:feature, area:cli'
[ "$rc" -eq 0 ] || fail "spaced labels: expected success, got $rc: $(cat "$ERR_LOG")"
expect_invocation 1 "pr${sep}create${sep}--base${sep}master${sep}--label${sep}type:feature,area:cli${sep}--fill${sep}"

# Empty or all-separator labels are treated as unset
for value in "" " ," ",,"; do
    run_pr "labels=$value"
    [ "$rc" -eq 0 ] || fail "labels='$value': expected success, got $rc: $(cat "$ERR_LOG")"
    expect_invocation 1 "pr${sep}create${sep}--base${sep}master${sep}--fill${sep}"
done

# Unknown label fails validation before creation
run_pr 'labels=type:feature,bogus:label'
expect_failure "unknown label" "not in .github/labels.yml"
expect_no_creation

# Unknown option fails fast
run_pr bogus=1
expect_failure "unknown option" "unknown option"
expect_no_creation

# Body file without title falls back to the HEAD commit subject
printf 'body text\n' > "$tmp/body.md"
run_pr "body=$tmp/body.md"
[ "$rc" -eq 0 ] || fail "body only: expected success, got $rc: $(cat "$ERR_LOG")"
expect_invocation 1 "pr${sep}create${sep}--base${sep}master${sep}--body-file${sep}$tmp/body.md${sep}--title${sep}${head_subject}${sep}"

# Body file with explicit title keeps it
run_pr "body=$tmp/body.md" 'title=Explicit'
expect_invocation 1 "pr${sep}create${sep}--base${sep}master${sep}--body-file${sep}$tmp/body.md${sep}--title${sep}Explicit${sep}"

# Missing body file fails before creation
run_pr "body=$tmp/nope.md"
expect_failure "missing body file" "body file not found"
expect_no_creation

# task validates the destination before creation and comments the resulting URL.
for task_id in 001 033a; do
    run_pr "task=$task_id"
    [ "$rc" -eq 0 ] || fail "task link: expected success, got $rc: $(cat "$ERR_LOG")"
    [ "$(invocations)" -eq 3 ] || fail "task link: expected three tool invocations"
    expect_invocation 1 "task${sep}show${sep}--${sep}${task_id}${sep}"
    expect_invocation 2 "pr${sep}create${sep}--base${sep}master${sep}--fill${sep}"
    expect_invocation 3 "task${sep}comment${sep}--${sep}${task_id}${sep}PR: https://example.com/pr/1${sep}"
done

# Missing tasks stop before remote creation.
run_pr "task=missing"
expect_failure "missing task" "task not found"
[ "$(invocations)" -eq 1 ] || fail "missing task: expected only ahm validation"
expect_invocation 1 "task${sep}show${sep}--${sep}missing${sep}"

run_pr "task="
expect_failure "empty task" "requires an ahm task ID"
expect_no_creation

# A failed comment-back retains the PR URL and gives a recovery instruction.
run_pr "task=fail-comment"
expect_failure "comment failure" "PR created at"
[ "$(invocations)" -eq 3 ] || fail "comment failure: expected create and comment attempt"
grep -q "https://example.com/pr/1" "$OUT_LOG" || fail "comment failure: PR URL missing"

# Failed PR creation never comments on the task.
run_pr "task=001" "title=fail-create"
expect_failure "create failure" "remote creation failed"
[ "$(invocations)" -eq 2 ] || fail "create failure: expected validation and creation attempt only"

# Flag-shaped input remains an operand for ahm validation, not an option.
run_pr "task=--help"
expect_invocation 1 "task${sep}show${sep}--${sep}--help${sep}"

# The retired issue option cannot silently create an unlinked PR.
run_pr "issue=123"
expect_failure "retired issue option" "unknown option"
expect_no_creation

echo "test-just-pr: all pr recipe cases passed"
