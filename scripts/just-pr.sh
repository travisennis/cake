#!/usr/bin/env bash
set -euo pipefail

labels=""
body_file=""
title=""
issue=""

for option in "$@"; do
    [[ -z "$option" ]] && continue
    case "$option" in
        labels=*) labels="${option#labels=}" ;;
        body=*)   body_file="${option#body=}" ;;
        title=*)  title="${option#title=}" ;;
        issue=*)  issue="${option#issue=}" ;;
        *) echo "ERROR: unknown option '$option' (expected labels=..., body=<file>, title=..., issue=<number>)" >&2; exit 1 ;;
    esac
done

args=(--base master)
# Trim each label and drop empties before validating, so the checks see
# exactly the argv element gh receives; its CSV split keeps inner spaces.
labels=$(printf '%s\n' "$labels" | tr ',' '\n' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' | sed '/^$/d' | paste -sd, -)
if [[ -n "$labels" ]]; then
    known_labels=$(sed -n 's/^[[:space:]]*- name:[[:space:]]*//p' .github/labels.yml)
    while IFS= read -r label; do
        [[ -z "$label" ]] && continue
        if ! grep -Fxq "$label" <<< "$known_labels"; then
            echo "ERROR: label '$label' is not in .github/labels.yml (see 'just labels-check-file')" >&2
            exit 1
        fi
    done < <(printf '%s\n' "$labels" | tr ',' '\n')
    args+=(--label "$labels")
fi
if [[ -n "$body_file" ]]; then
    [[ -f "$body_file" ]] || { echo "ERROR: pull request body file not found: $body_file" >&2; exit 1; }
    args+=(--body-file "$body_file")
    # Non-interactive gh needs an explicit title; fall back to the HEAD subject.
    if [[ -z "$title" ]]; then
        # --body-file alone would prompt; default to the HEAD commit subject.
        title=$(git log -1 --pretty=%s)
    fi
else
    args+=(--fill)
fi
# An explicit title wins over --fill autofill (see gh pr create --help);
# --fill still supplies the body when no body file was given.
if [[ -n "$title" ]]; then
    args+=(--title "$title")
fi
if [[ -n "$issue" ]]; then
    [[ "$issue" =~ ^[0-9]+$ ]] || { echo "ERROR: issue must be a number, got: $issue" >&2; exit 1; }
fi
url=$(gh pr create "${args[@]}")
printf '%s\n' "$url"
if [[ -n "$issue" ]]; then
    gh issue comment "$issue" --body "PR: $url"
fi
