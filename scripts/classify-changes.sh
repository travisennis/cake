#!/usr/bin/env sh
# classify-changes.sh — classify the changed file set of the current branch.
#
# The pre-push gate routes by changed path class so a documentation-only push
# pays for targeted documentation checks instead of the full Rust gate. The
# GitHub Actions `changes` job in .github/workflows/ci.yml shares this
# classifier so the hook and CI cannot drift apart.
#
# Usage:
#   scripts/classify-changes.sh [--base <ref>] [--head <ref>] [--files|--check]
#
# With --base/--head (CI passes these), the changed set is
# `git diff --name-only <base>...<head>`. Without them, the base is the
# upstream branch when one is set, otherwise origin/master. `just branch` and
# `just worktree` create branches with --no-track, so the origin/master
# fallback is the common local case.
#
# Output, one word on stdout:
#   docs    — every changed file is Markdown
#   code    — every changed file is a code, manifest, toolchain, or CI path
#   mixed   — both Markdown and code paths changed
#   unknown — at least one changed file matches neither class (fail closed:
#             the caller runs the full gate)
#   none    — no changed files between base and head
#
# --files prints the changed Markdown files that still exist (deletions are
# skipped), one per line, for targeted panache checks. Code-class files are
# excluded even when they carry a .md extension (for example src/prompts/).
# --check runs `git diff --check` over the same <base>...<head> range the
# classifier measures, for the pre-push whitespace gate. Both --files and
# --check skip silently when no base can be resolved; class mode fails closed
# to "unknown" instead.
set -eu

base=""
head="HEAD"
mode="class"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --base)
            base="${2:-}"
            if [ "$#" -ge 2 ]; then shift 2; else shift 1; fi
            ;;
        --head)
            head="${2:-}"
            if [ "$#" -ge 2 ]; then shift 2; else shift 1; fi
            ;;
        --files) mode="files"; shift ;;
        --check) mode="check"; shift ;;
        *)
            echo "ERROR: unknown argument '$1'" >&2
            echo "usage: scripts/classify-changes.sh [--base <ref>] [--head <ref>] [--files|--check]" >&2
            exit 2
            ;;
    esac
done

# Code class: any path that can break or exercise the Rust gate. Mirrors the
# path filter the `changes` job in .github/workflows/ci.yml used before this
# script existed, with `.github/workflows/` widened from ci.yml to every
# workflow. Everything else that is not Markdown is "unknown" and fails closed.
# The alternation lives in exactly one case statement below, shared by the
# class and --files modes: a pattern read from a variable is one literal glob
# on some POSIX shells, so a new code path must be added here only.

if [ -z "$base" ]; then
    if base="$(git rev-parse --abbrev-ref --symbolic-full-name @{upstream} 2>/dev/null)"; then
        :
    elif git rev-parse --verify --quiet origin/master >/dev/null 2>&1; then
        base="origin/master"
    else
        # No base to diff against (for example a fresh clone with no origin);
        # fail closed so the caller runs the full gate (class mode) or skips
        # quietly (--files and --check modes).
        [ "$mode" = "class" ] && echo "unknown"
        exit 0
    fi
fi

if ! changed="$(git diff --name-only "$base...$head" 2>/dev/null)"; then
    # Unresolvable base (for example a first push where the CI base sha is the
    # all-zeros sha); fail closed so the caller runs the full gate (class mode)
    # or skips quietly (--files and --check modes).
    [ "$mode" = "class" ] && echo "unknown"
    exit 0
fi

if [ "$mode" = "check" ]; then
    # Same range the classifier measures, so the pre-push whitespace gate sees
    # the pushed commits rather than (as with a bare `git diff --check`) only
    # unstaged worktree changes. git exits 2 on whitespace errors.
    git diff --check "$base...$head"
    exit 0
fi

code=0
docs=0
unknown=0
markdown_files=""

if [ -n "$changed" ]; then
    IFS='
'
    for file in $changed; do
        case "$file" in
            src/*|tests/*|Cargo.toml|Cargo.lock|rust-toolchain.toml|.cargo/*|.github/workflows/*|justfile|scripts/*|ci/*)
                code=1
                ;;
            *.md|*.markdown)
                docs=1
                # Accumulate existing Markdown files for --files mode; deletions
                # are skipped so removed documents are not checked.
                [ -f "$file" ] && markdown_files="$markdown_files$file
"
                ;;
            *)
                unknown=1
                ;;
        esac
    done
fi

if [ "$mode" = "files" ]; then
    printf '%s' "$markdown_files"
    exit 0
fi

if [ "$unknown" -eq 1 ]; then
    echo "unknown"
elif [ "$code" -eq 1 ] && [ "$docs" -eq 1 ]; then
    echo "mixed"
elif [ "$code" -eq 1 ]; then
    echo "code"
elif [ "$docs" -eq 1 ]; then
    echo "docs"
else
    echo "none"
fi
