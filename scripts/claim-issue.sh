#!/usr/bin/env sh
# claim-issue.sh — claim a Cake Backlog issue by moving its board Status field.
#
# The Projects v2 Status field is the claim record for the development queue:
# Ready -> In Progress when work starts, In Progress -> Ready on handback. This
# script makes that move one command, so an agent picking up the next Ready task
# (see docs/workflow/tasks.md) can claim it without hand-rolling the
# updateProjectV2ItemFieldValue mutation for the Status field.
#
# Usage:
#   scripts/claim-issue.sh <issue-number>            # claim: Ready -> In Progress
#   scripts/claim-issue.sh --unclaim <issue-number>  # hand back: In Progress -> Ready
#   scripts/claim-issue.sh --assign <issue-number>   # claim and assign the current gh user
#
# Requirements: gh (authenticated) and jq. GITHUB_TOKEN cannot reach Projects
# v2; use your own gh auth or export GH_TOKEN with a classic PAT carrying repo +
# project scopes (the CAKE_BACKLOG_PAT pattern).
set -eu

PROJECT_NUMBER="1"                                   # Cake Backlog board (project 1)
STATUS_FIELD_ID="PVTSSF_lAHNFebOAX0lXs4WZke6"
READY_OPTION_ID="4960c4d2"
IN_PROGRESS_OPTION_ID="63fc6230"
OWNER_REF="@me"                                      # viewer-only query; works under any PAT

unclaim=""
assign=""

usage() {
    cat <<'EOF'
usage: scripts/claim-issue.sh [--unclaim] [--assign] <issue-number>

Claim a Cake Backlog issue by moving its board Status field:
  Ready -> In Progress when work starts, In Progress -> Ready on handback.

  --unclaim   hand the issue back (In Progress -> Ready)
  --assign    also assign the current gh user (claim only)

Requires gh (authenticated) and jq. GITHUB_TOKEN cannot reach Projects v2; use
your own gh auth or export GH_TOKEN with repo + project scopes.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --unclaim) unclaim="1"; shift ;;
        --assign) assign="1"; shift ;;
        -h|--help) usage; exit 0 ;;
        *)
            case "$1" in
                *[!0-9]*|'') echo "ERROR: expected an issue number, got '$1'" >&2; exit 2 ;;
                *) break ;;
            esac
            ;;
    esac
done

issue="${1:-}"
if [ -z "$issue" ]; then
    usage >&2
    exit 2
fi

command -v gh >/dev/null 2>&1 || { echo "ERROR: gh not found on PATH" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "ERROR: jq not found on PATH" >&2; exit 1; }

# Resolve the project item id for the issue number. item-list returns every item
# on the board (--limit 200; the personal board is far below that) and its JSON
# carries the item id, Status, and issue number, so no GraphQL pagination loop
# is needed here.
board_json="$(gh project item-list "$PROJECT_NUMBER" --owner "$OWNER_REF" --limit 200 --format json 2>&1)" || {
    echo "ERROR: could not read the Cake Backlog board; is gh authenticated and can it see the board? (GITHUB_TOKEN cannot reach Projects v2; export GH_TOKEN with repo + project scopes)" >&2
    exit 1
}

item_id="$(printf '%s' "$board_json" | jq -r --argjson n "$issue" '.items[] | select(.content.number == $n) | .id' | head -n1)"
current_status="$(printf '%s' "$board_json" | jq -r --argjson n "$issue" '.items[] | select(.content.number == $n) | .status' | head -n1)"

if [ -z "$item_id" ]; then
    echo "ERROR: #${issue} is not on the Cake Backlog board (project 1)" >&2
    echo "hint: add it with \`gh project item-add 1 --owner @me --url https://github.com/travisennis/cake/issues/${issue}\`" >&2
    exit 1
fi

if [ -n "$unclaim" ]; then
    if [ "$current_status" = "Ready" ]; then
        echo "#${issue} is already Ready; nothing to do"
        exit 0
    fi
    [ "$current_status" = "In Progress" ] || echo "warning: #${issue} is currently '${current_status}', moving to Ready anyway" >&2
    option_id="$READY_OPTION_ID"
    verb="handed back #${issue} (${current_status} -> Ready)"
else
    if [ "$current_status" = "In Progress" ]; then
        echo "#${issue} is already In Progress; nothing to do"
        exit 0
    fi
    option_id="$IN_PROGRESS_OPTION_ID"
    verb="claimed #${issue} (${current_status} -> In Progress)"
fi

mutation='mutation($pid: ID!, $iid: ID!, $fid: ID!, $oid: String!) { updateProjectV2ItemFieldValue(input: {projectId: $pid, itemId: $iid, fieldId: $fid, value: {singleSelectOptionId: $oid}}) { projectV2Item { id } } }'
if ! out="$(gh api graphql -f query="$mutation" -f pid="PVT_kwHNFebOAX0lXg" -f iid="$item_id" -f fid="$STATUS_FIELD_ID" -f oid="$option_id" 2>&1 >/dev/null)"; then
    echo "ERROR: status move failed: ${out}" >&2
    exit 1
fi

if [ -n "$assign" ] && [ -z "$unclaim" ]; then
    gh issue edit "$issue" --add-assignee @me >/dev/null
    verb="${verb} and assigned @me"
fi

echo "${verb}"
