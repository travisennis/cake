#!/usr/bin/env python3
"""Synchronize repository labels with `.github/labels.yml`.

`labels.yml` is the single source of truth for the repository's label
vocabulary. This script creates, updates, and optionally deletes repository
labels so the repo matches the file, and enforces the vocabulary at runtime
on issues and pull requests.

Exit code:
  0 – operation succeeded (or, with `--check`, the repo matches the file)
  1 – drift found by `--check`, an out-of-vocabulary label that `--enforce`
      attempted to remove could not be removed, or a runtime error
  2 – usage error

Usage:
    python3 scripts/sync-labels.py                    # create/update labels from the file
    python3 scripts/sync-labels.py --check            # verify the repo matches; exit 1 on drift
    python3 scripts/sync-labels.py --prune            # create/update, then delete unlisted labels
    python3 scripts/sync-labels.py --check-file       # validate the file only (no network)
    python3 scripts/sync-labels.py --enforce --number 123   # remove out-of-vocabulary labels from an issue

Requires the GitHub CLI (`gh`), authenticated. In GitHub Actions `gh` is
preinstalled and uses `GITHUB_TOKEN`; locally it uses the logged-in account.
Set `GH_REPO` (owner/repo) to override repository detection.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.parse

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LABELS_FILE = os.path.join(PROJECT_ROOT, ".github", "labels.yml")

NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9 ._:-]*$")
COLOR_RE = re.compile(r"^[0-9A-Fa-f]{6}$")


def err(msg: str) -> None:
    print(f"ERROR: {msg}", file=sys.stderr)


# ---------------------------------------------------------------------------
# Vocabulary file
# ---------------------------------------------------------------------------

def _strip_quotes(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
        return value[1:-1]
    return value


def parse_labels_file(path: str) -> list[dict[str, str]]:
    """Parse the restricted YAML subset used by labels.yml.

    Accepts full-line comments, a single top-level ``labels:`` key, and
    entries of the form ``- name: X`` / ``color: X`` / ``description: X``.
    Values may be plain scalars or single/double quoted. Anything else is
    rejected so drift in the file fails loudly instead of parsing
    ambiguously.
    """
    labels: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    seen_top_key = False
    with open(path, encoding="utf-8") as fh:
        for lineno, raw in enumerate(fh, start=1):
            line = raw.rstrip("\n").strip()
            if not line or line.startswith("#"):
                continue
            if line == "labels:" and not seen_top_key:
                seen_top_key = True
                continue
            if not seen_top_key:
                raise ValueError(f"{path}:{lineno}: expected `labels:` before entries")
            if line.startswith("- name:"):
                current = {"name": _strip_quotes(line[len("- name:"):].strip())}
                labels.append(current)
                continue
            if current is None:
                raise ValueError(f"{path}:{lineno}: entry field before any `- name:` entry")
            for key in ("color:", "description:"):
                if line.startswith(key):
                    raw_value = line[len(key):].strip()
                    if not raw_value:
                        raise ValueError(f"{path}:{lineno}: empty {key}")
                    if raw_value[0] in ('"', "'"):
                        if len(raw_value) < 2 or raw_value[-1] != raw_value[0]:
                            raise ValueError(
                                f"{path}:{lineno}: trailing content after quoted {key} value"
                            )
                        value = raw_value[1:-1]
                    else:
                        if " #" in raw_value:
                            raise ValueError(
                                f"{path}:{lineno}: inline comments are not supported in {key} values"
                            )
                        value = raw_value
                    field = key[:-1]
                    if field in current:
                        raise ValueError(f"{path}:{lineno}: duplicate {key}")
                    current[field] = value
                    break
            else:
                raise ValueError(f"{path}:{lineno}: unrecognized line {line!r}")
    return labels


def validate_vocabulary(labels: list[dict[str, str]]) -> None:
    names: set[str] = set()
    for entry in labels:
        missing = [k for k in ("name", "color", "description") if k not in entry]
        if missing:
            raise ValueError(
                f"label {entry.get('name', '<no name>')!r} missing {', '.join(missing)}"
            )
        if not NAME_RE.match(entry["name"]):
            raise ValueError(f"label name {entry['name']!r} must match {NAME_RE.pattern}")
        if not COLOR_RE.match(entry["color"]):
            raise ValueError(
                f"label {entry['name']!r}: color {entry['color']!r} must be 6 hex digits"
            )
        if entry["name"] in names:
            raise ValueError(f"duplicate label name {entry['name']!r}")
        names.add(entry["name"])


# ---------------------------------------------------------------------------
# GitHub CLI helpers
# ---------------------------------------------------------------------------

def gh(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["gh", *args], check=False, capture_output=True, text=True)


def repo_slug() -> str:
    env = os.environ.get("GH_REPO")
    if env:
        return env
    proc = gh(["repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"])
    if proc.returncode != 0:
        err(f"cannot determine repository: {proc.stderr.strip()}")
        raise SystemExit(1)
    return proc.stdout.strip()


def quoted_segment(name: str) -> str:
    return urllib.parse.quote(name, safe="")


def fetch_repo_labels(repo: str) -> dict[str, dict[str, str]]:
    proc = gh(["api", "--paginate", f"repos/{repo}/labels"])
    if proc.returncode != 0:
        err(f"failed to list labels: {proc.stderr.strip()}")
        raise SystemExit(1)
    return {
        item["name"]: {
            "name": item["name"],
            "color": item["color"],
            "description": item.get("description") or "",
        }
        for item in json.loads(proc.stdout)
    }


def create_label(repo: str, entry: dict[str, str]) -> None:
    proc = gh(
        [
            "api", "-X", "POST", f"repos/{repo}/labels",
            "-f", f"name={entry['name']}",
            "-f", f"color={entry['color']}",
            "-f", f"description={entry['description']}",
        ]
    )
    if proc.returncode != 0:
        err(f"failed to create label {entry['name']}: {proc.stderr.strip()}")
        raise SystemExit(1)
    print(f"created {entry['name']}")


def update_label(repo: str, name: str, entry: dict[str, str]) -> None:
    proc = gh(
        [
            "api", "-X", "PATCH", f"repos/{repo}/labels/{quoted_segment(name)}",
            "-f", f"color={entry['color']}",
            "-f", f"description={entry['description']}",
        ]
    )
    if proc.returncode != 0:
        err(f"failed to update label {name}: {proc.stderr.strip()}")
        raise SystemExit(1)
    print(f"updated {name}")


def delete_label(repo: str, name: str) -> None:
    proc = gh(["api", "-X", "DELETE", f"repos/{repo}/labels/{quoted_segment(name)}"])
    if proc.returncode != 0:
        err(f"failed to delete label {name}: {proc.stderr.strip()}")
        raise SystemExit(1)
    print(f"deleted {name}")


def remove_issue_label(repo: str, number: int, name: str) -> bool:
    proc = gh(
        ["api", "-X", "DELETE", f"repos/{repo}/issues/{number}/labels/{quoted_segment(name)}"]
    )
    if proc.returncode != 0:
        err(f"failed to remove label {name} from #{number}: {proc.stderr.strip()}")
        return False
    print(f"removed {name} from #{number}")
    return True


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------

def enforce(vocabulary: dict[str, dict[str, str]], repo: str, number: int) -> int:
    proc = gh(["api", "--paginate", f"repos/{repo}/issues/{number}/labels"])
    if proc.returncode != 0:
        err(f"failed to list labels on #{number}: {proc.stderr.strip()}")
        return 1
    applied = [item["name"] for item in json.loads(proc.stdout)]
    unknown = [name for name in applied if name not in vocabulary]
    if not unknown:
        print(f"OK: all labels on #{number} are in the vocabulary")
        return 0
    failed: list[str] = []
    for name in unknown:
        if not remove_issue_label(repo, number, name):
            failed.append(name)
    removed = [name for name in unknown if name not in failed]
    if removed:
        body = (
            "Automated label governance removed out-of-vocabulary label(s): "
            f"{', '.join(removed)}. The vocabulary lives in `.github/labels.yml`; "
            "see `python3 scripts/sync-labels.py --help`."
        )
        proc = gh(["api", f"repos/{repo}/issues/{number}/comments", "-f", f"body={body}"])
        if proc.returncode != 0:
            err(f"failed to comment on #{number}: {proc.stderr.strip()}")
            return 1
    if failed:
        err(f"failed to remove out-of-vocabulary label(s): {', '.join(failed)}")
        return 1
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="verify the repo matches the file; exit 1 on drift")
    mode.add_argument("--check-file", action="store_true", help="validate the file only (no network)")
    mode.add_argument("--prune", action="store_true", help="create/update, then delete labels not in the file")
    mode.add_argument("--enforce", action="store_true", help="remove out-of-vocabulary labels from an issue")
    parser.add_argument("--number", type=int, help="issue or pull request number for --enforce")
    args = parser.parse_args()

    try:
        vocabulary = parse_labels_file(LABELS_FILE)
        validate_vocabulary(vocabulary)
    except (OSError, ValueError) as exc:
        err(str(exc))
        return 1
    vocab_by_name = {entry["name"]: entry for entry in vocabulary}

    if args.check_file:
        print(f"OK: {LABELS_FILE} defines {len(vocabulary)} labels")
        return 0

    if args.enforce:
        if not args.number:
            err("--enforce requires --number")
            return 2
        return enforce(vocab_by_name, repo_slug(), args.number)

    repo = repo_slug()
    current = fetch_repo_labels(repo)

    missing = [entry["name"] for entry in vocabulary if entry["name"] not in current]
    changed = [
        entry["name"]
        for entry in vocabulary
        if entry["name"] in current and current[entry["name"]] != entry
    ]
    extra = [name for name in current if name not in vocab_by_name]

    if args.check:
        if missing or changed or extra:
            for name in missing:
                err(f"missing label: {name}")
            for name in changed:
                err(f"label differs from file: {name}")
            for name in extra:
                err(f"label not in vocabulary: {name}")
            return 1
        print(f"OK: {len(vocabulary)} labels match the vocabulary")
        return 0

    for name in missing:
        create_label(repo, vocab_by_name[name])
    for name in changed:
        update_label(repo, name, vocab_by_name[name])
    if args.prune:
        for name in extra:
            delete_label(repo, name)
    else:
        for name in extra:
            print(f"note: {name} exists but is not in the vocabulary (run --prune to delete)")
    print(f"synced: {len(vocabulary)} labels")
    return 0


if __name__ == "__main__":
    sys.exit(main())
