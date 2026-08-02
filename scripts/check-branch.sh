#!/usr/bin/env sh
# Reject commits made on a protected branch and pushes that target one.
#
# Invoked by prek at the pre-commit and pre-push stages. The GitHub ruleset on
# `master` is the authoritative enforcement; this hook fails fast and locally so
# an agent or a human learns before spending a full verification gate on work
# that cannot be pushed.
set -eu

protected="master main"

is_protected() {
    for branch in $protected; do
        [ "$1" = "$branch" ] && return 0
    done
    return 1
}

fail() {
    echo "ERROR: $1" >&2
    echo "" >&2
    echo "Work happens on a branch. Create one from an up-to-date master:" >&2
    echo "" >&2
    echo "    git fetch origin && git switch -c <type>/<slug> origin/master" >&2
    echo "" >&2
    echo "See docs/runbooks/parallel-worktrees.md." >&2
    exit 1
}

# pre-push: prek passes the remote name and URL as arguments. Git supplies one
# "<local_ref> <local_sha> <remote_ref> <remote_sha>" line per pushed ref on
# stdin, but a hook runner is free to consume stdin itself rather than forward
# it. When the ref lines are available, reject any push whose destination is
# protected regardless of which branch is checked out; when they are not, fall
# through to the checked-out-branch check below.
action="commit on"
if [ "$#" -ge 2 ]; then
    action="push from"
    saw_ref=0
    while read -r _local_ref _local_sha remote_ref _remote_sha; do
        [ -n "${remote_ref:-}" ] || continue
        saw_ref=1
        remote_branch="${remote_ref#refs/heads/}"
        if is_protected "$remote_branch"; then
            fail "direct push to protected branch '$remote_branch'"
        fi
    done
    [ "$saw_ref" -eq 1 ] && exit 0
fi

# pre-commit, and pre-push without forwarded ref lines: reject work on a
# protected branch. A detached HEAD has no branch name and is left alone.
current="$(git symbolic-ref --quiet --short HEAD || true)"
if [ -n "$current" ] && is_protected "$current"; then
    fail "$action protected branch '$current'"
fi
