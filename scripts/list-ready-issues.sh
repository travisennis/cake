#!/usr/bin/env sh
# list-ready-issues.sh — print the Ready queue of the Cake Backlog board.
#
# One line per issue, highest priority first ("P0 #123 Title"). Priority is the
# board's Priority single-select field (P0..P4); the tie-break is issue number
# ascending. This is the machine-readable form of the `gh project item-list
# --query 'status:Ready'` command documented in docs/workflow/tasks.md, with the
# board's own priority ordering applied (GitHub issue search does not index
# Projects v2 Status fields, so the queue must come from the board).
#
# Usage: scripts/list-ready-issues.sh
#
# Requirements: gh (authenticated) and jq.
set -eu

command -v gh >/dev/null 2>&1 || { echo "ERROR: gh not found on PATH" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "ERROR: jq not found on PATH" >&2; exit 1; }

# Draft items have no issue number and are filtered out; an unset or unrecognized
# Priority sorts after P4 rather than failing the pipeline.
gh project item-list 1 --owner @me --query 'status:Ready' --limit 200 --format json |
    jq -r '.items
        | map(select(.content.number != null))
        | map({ p: ((.priority // "P9") as $p | { "P0": 0, "P1": 1, "P2": 2, "P3": 3, "P4": 4 }[$p] // 9), n: .content.number, t: .content.title })
        | sort_by(.p, .n)
        | .[] | "P\(.p) #\(.n) \(.t)"'
