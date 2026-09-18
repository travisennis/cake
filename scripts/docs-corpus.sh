#!/usr/bin/env bash
# docs-corpus.sh — run the documentation gates over the corpus the repository
# ships, not over every file the working tree happens to hold.
#
# The corpus is the Markdown git tracks (the index, so staged additions count).
# Untracked scratch notes are not part of the repository, so the gates must not
# fail on them and must not reformat them; a clean CI checkout holds exactly
# this set, which is what keeps the local gates and CI measuring the same
# corpus. Format an untracked draft explicitly with `panache format <path>`.
#
# Modes:
#   --list   print the corpus, NUL-separated, for callers that pipe it
#   --check  panache format --check, then panache lint, over the corpus
#   --lint   panache lint over the corpus
#   --fmt    panache format over the corpus
#
# A tracked path removed from the worktree is still listed: panache warns that
# the path is missing and exits 0, so deletions need no filtering here. An empty
# corpus runs nothing and succeeds, so a scratch repository never invokes
# panache at all (xargs would run it once with no paths).
set -euo pipefail

mode="${1:---list}"
case "$mode" in
    --list | --check | --lint | --fmt) ;;
    *)
        echo "ERROR: unknown mode '$mode' (expected --list, --check, --lint, or --fmt)" >&2
        exit 2
        ;;
esac

corpus() {
    git ls-files -z -- '*.md' '*.markdown'
}

run_over_corpus() {
    # Read the NUL-separated corpus from stdin and run the command only when
    # something is in it.
    local paths=()
    while IFS= read -r -d '' path; do
        paths+=("$path")
    done
    if [ "${#paths[@]}" -eq 0 ]; then
        echo "docs-corpus: no tracked Markdown"
        return 0
    fi
    "$@" "${paths[@]}"
}

case "$mode" in
    --list)
        corpus
        ;;
    --check)
        corpus | run_over_corpus panache format --check --force-exclude --quiet
        corpus | run_over_corpus panache lint --force-exclude --quiet
        ;;
    --lint)
        corpus | run_over_corpus panache lint --force-exclude --quiet
        ;;
    --fmt)
        corpus | run_over_corpus panache format
        ;;
esac
