#!/usr/bin/env python3
"""Migrate ahm task records to GitHub Issues.

Reads `.ahm/tasks/active/*.md` (ahm front matter plus body) and creates one
GitHub issue per task with:

- title from front matter;
- body: the task body (duplicate title heading and `## Comments` removed),
  a `## Depends on` section, and a `## Migrated from ahm` metadata section;
- labels mapped onto the `.github/labels.yml` vocabulary (see LABEL_MAP);
- Projects v2 fields (Priority, Effort, Status) set on the Cake Backlog
  project, with status mapped Open -> Backlog, Pending -> Ready,
  Blocked -> Blocked.

Dependencies are a two-pass operation: issues are created in ahm-id order,
their GitHub numbers captured, then `ahm#<id>` tokens in `## Depends on`
sections are rewritten to `#<number>`. Dependency targets that are not part
of the migrated set (completed/cancelled tasks) are kept as text.

Requires the GitHub CLI (`gh`), authenticated. In GitHub Actions `gh` is
preinstalled and uses `GITHUB_TOKEN`; locally it uses the logged-in account.
Set `GH_REPO` (owner/repo) to override repository detection.

Exit code:
  0 – success (or `--dry-run`)
  1 – validation or runtime error
  2 – usage error

Usage:
    python3 scripts/migrate-tasks-to-gh.py --dry-run
    python3 scripts/migrate-tasks-to-gh.py [--limit N] [--no-comments] [--map-out FILE]
    python3 scripts/migrate-tasks-to-gh.py --map-in migrated.json [--map-out FILE]
    python3 scripts/migrate-tasks-to-gh.py --help

Without `--map-in` the tool does not deduplicate: issues are created from the
ahm records as they exist at run time, so rerunning duplicates them. Pass
`--map-in` (a JSON map written by `--map-out`, or `--map-in` again after a
smoke batch) to skip already-migrated tasks and resolve their dependency
links. Run `--dry-run` and review before creating. `--limit` is for
smoke-testing a small batch, not for batched migration -- dependencies on
tasks outside the batch render as text rather than links.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TASKS_DIR = os.path.join(PROJECT_ROOT, ".ahm", "tasks", "active")
LABELS_FILE = os.path.join(PROJECT_ROOT, ".github", "labels.yml")

VALID_STATUSES = {"Open", "Pending", "In Progress", "Blocked", "Tracking"}
VALID_PRIORITIES = {"P0", "P1", "P2", "P3", "P4"}
VALID_EFFORTS = {"XS", "S", "M", "L", "XL"}
STATUS_TO_FIELD = {"Open": "Backlog", "Pending": "Ready", "Blocked": "Blocked"}
TASK_FILE_RE = re.compile(r"^\d+[a-z]?\.md$")

# ahm labels that are not part of the GitHub vocabulary, mapped to their
# canonical replacement. Anything else that is not in `.github/labels.yml`
# is dropped (and reported in the dry run).
LABEL_MAP = {
    "type:feat": "type:feature",
    "enhancement": "type:feature",
    "hooks": "area:hooks",
    "area:testing": "area:test",
    "area:documentation": "area:docs",
}

KNOWN_KEYS = {
    "id", "title", "status", "priority", "effort", "labels", "exec_plan",
    "depends_on", "created", "updated",
}


def err(msg: str) -> None:
    print(f"ERROR: {msg}", file=sys.stderr)


# ---------------------------------------------------------------------------
# Vocabulary file (mirrors scripts/sync-labels.py; import it once PR #44
# merges and the module is on master)
# ---------------------------------------------------------------------------

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


def _strip_quotes(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
        return value[1:-1]
    return value


def load_vocabulary() -> set[str] | None:
    """Return the label vocabulary, or None when labels.yml is not present.

    labels.yml lands on master with the label-governance change (PR #44);
    until then the strict vocabulary check is skipped and labels pass through
    the LABEL_MAP aliases only.
    """
    try:
        return {entry["name"] for entry in parse_labels_file(LABELS_FILE)}
    except FileNotFoundError:
        return None


# ---------------------------------------------------------------------------
# ahm task records
# ---------------------------------------------------------------------------

def parse_front_matter(text: str, path: str) -> dict[str, str]:
    fm: dict[str, str] = {}
    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#") or ":" not in line:
            raise ValueError(f"{path}: front matter line {lineno}: expected `key: value`")
        key, _, value = line.partition(":")
        key = key.strip()
        value = _strip_quotes(value.strip())
        if key in fm:
            raise ValueError(f"{path}: duplicate front matter key {key!r}")
        fm[key] = value
    return fm


def split_task_file(path: str) -> tuple[str, dict[str, str]]:
    """Return (body, front matter) for an ahm task file."""
    with open(path, encoding="utf-8") as fh:
        lines = fh.read().splitlines()
    if not lines or lines[0].strip() != "---":
        raise ValueError(f"{path}: missing leading `---`")
    end = None
    for i in range(1, len(lines)):
        if lines[i].strip() == "---":
            end = i
            break
    if end is None:
        raise ValueError(f"{path}: unterminated front matter")
    fm_text = "\n".join(lines[1:end])
    body = "\n".join(lines[end + 1:]).strip("\n")
    return body, parse_front_matter(fm_text, path)


def parse_depends_on(raw: str) -> list[int]:
    if raw.strip() in ("", "-"):
        return []
    out = []
    for token in raw.split(","):
        token = token.strip().strip('"').strip("'")
        if not token:
            continue
        if not token.isdigit():
            raise ValueError(f"depends_on target {token!r} is not a task id")
        out.append(int(token))
    return out


def parse_labels(raw: str) -> list[str]:
    if raw.strip() in ("", "-"):
        return []
    return [token.strip() for token in raw.split(",") if token.strip()]


def load_tasks() -> list[dict[str, object]]:
    """Load and validate every active task file, sorted by ahm id."""
    tasks: list[dict[str, object]] = []
    for name in sorted(os.listdir(TASKS_DIR)):
        if not TASK_FILE_RE.match(name):
            continue
        path = os.path.join(TASKS_DIR, name)
        body, fm = split_task_file(path)
        task_id = name[:-3]
        errors = []
        if fm.get("id") != task_id:
            errors.append(f"id {fm.get('id')!r} != filename {task_id!r}")
        for key in ("title", "status", "priority", "effort", "labels", "exec_plan", "depends_on"):
            if key not in fm:
                errors.append(f"missing {key}")
        if not errors:
            if fm["status"] not in VALID_STATUSES:
                errors.append(f"status {fm['status']!r}")
            if fm["priority"] not in VALID_PRIORITIES:
                errors.append(f"priority {fm['priority']!r}")
            if fm["effort"] not in VALID_EFFORTS:
                errors.append(f"effort {fm['effort']!r}")
        if errors:
            raise ValueError(f"{path}: " + "; ".join(errors))
        extra = {k: v for k, v in fm.items() if k not in KNOWN_KEYS}
        tasks.append({
            "id": task_id,
            "title": fm["title"],
            "status": fm["status"],
            "priority": fm["priority"],
            "effort": fm["effort"],
            "labels": parse_labels(fm["labels"]),
            "depends_on": parse_depends_on(fm["depends_on"]),
            "exec_plan": None if fm["exec_plan"].strip() in ("", "-") else fm["exec_plan"],
            "created": fm.get("created"),
            "updated": fm.get("updated"),
            "extra": extra,
            "body": body,
            "path": os.path.relpath(path, PROJECT_ROOT),
        })
    tasks.sort(key=lambda t: (len(t["id"]), t["id"]))
    return tasks


def split_comments(body: str) -> tuple[str, str | None]:
    lines = body.splitlines()
    for i, line in enumerate(lines):
        if line.strip().lower() == "## comments":
            before = "\n".join(lines[:i]).rstrip()
            comments = "\n".join(lines[i:]).strip()
            return before, comments or None
    return body, None


def strip_title_heading(body: str, title: str) -> str:
    """Remove every heading line that duplicates the task title.

    The ahm template repeats `# <title>` at the top of the body and again
    after sections such as `## Blocker`; all of them are redundant with the
    issue title. Lines inside fenced code blocks are left untouched.
    """
    heading = f"# {title}"
    kept: list[str] = []
    in_fence = False
    for line in body.splitlines():
        if line.strip().startswith("```"):
            in_fence = not in_fence
        if in_fence or line.strip() != heading:
            kept.append(line)
    return "\n".join(kept).strip()


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


def issue_url(repo: str, number: int) -> str:
    return f"https://github.com/{repo}/issues/{number}"


# ---------------------------------------------------------------------------
# Issue body composition
# ---------------------------------------------------------------------------

def compose_body(task: dict[str, object], resolvable_ids: set[str],
                 active_ids: set[str]) -> tuple[str, str | None]:
    """Compose the issue body.

    Dependencies on tasks whose numbers are resolvable (created in this run
    or already migrated via `--map-in`) render as `ahm#N` tokens that the
    second pass rewrites to `#<number>`; dependencies on active tasks outside
    that set and on completed/cancelled tasks render as text.
    """
    body, comments = split_comments(str(task["body"]))
    body = strip_title_heading(body, str(task["title"]))

    deps: list[str] = []
    for dep in task["depends_on"]:  # type: ignore[union-attr]
        dep_id = str(dep)
        if dep_id in resolvable_ids:
            deps.append(f"- ahm#{dep_id}")
        elif dep_id in active_ids:
            deps.append(f"- ahm#{dep_id} (active; not migrated in this run)")
        else:
            deps.append(f"- ahm#{dep_id} (completed or cancelled; not migrated)")

    lines = [body]
    if deps:
        lines.append("## Depends on\n\n" + "\n".join(deps))
    meta = [
        "## Migrated from ahm",
        "",
        f"- ahm id: {task['id']}",
        f"- ahm status: {task['status']}",
        f"- ahm file: `{task['path']}`",
    ]
    if task.get("created"):
        meta.append(f"- ahm created: {task['created']}")
    if task.get("updated"):
        meta.append(f"- ahm updated: {task['updated']}")
    if task.get("exec_plan"):
        meta.append(f"- exec_plan: `{task['exec_plan']}` (kept in repo; not migrated)")
    for key, value in task["extra"].items():  # type: ignore[union-attr]
        meta.append(f"- {key}: {value}")
    lines.append("\n".join(meta))
    return "\n\n".join(lines).strip() + "\n", comments


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------

def dry_run(tasks: list[dict[str, object]], active_ids: set[str], vocabulary: set[str],
            skip_ids: set[str]) -> int:
    remaining = [t for t in tasks if str(t["id"]) not in skip_ids]
    total = len(remaining)
    by_status: dict[str, int] = {}
    dep_edges = dep_migrated = 0
    dropped_labels: list[str] = []
    comment_count = 0
    plan_links = 0
    print(f"Dry run: {total} ahm tasks -> GitHub issues (created in ahm-id order)")
    if skip_ids:
        print(f"  ({len(skip_ids)} already migrated, skipped)")
    print()
    for task in remaining:
        by_status[str(task["status"])] = by_status.get(str(task["status"]), 0) + 1
        deps = task["depends_on"]  # type: ignore[union-attr]
        dep_edges += len(deps)
        dep_migrated += sum(1 for d in deps if str(d) in active_ids)
        mapped, dropped = map_labels(task["labels"], vocabulary)  # type: ignore[arg-type]
        dropped_labels.extend(f"{task['id']}: {name}" for name in dropped)
        _, comments = split_comments(str(task["body"]))
        if comments:
            comment_count += 1
        if task.get("exec_plan"):
            plan_links += 1
        status_field = STATUS_TO_FIELD.get(str(task["status"]), str(task["status"]))
        print(
            f"  ahm {task['id']:>4}  Status={status_field:<8} P={task['priority']} "
            f"E={task['effort']:<2} labels={','.join(mapped) or '-'} "
            f"deps={len(deps)} title={task['title']}"
        )
    print()
    print("Summary:")
    print(f"  tasks: {total} ({', '.join(f'{k}: {v}' for k, v in sorted(by_status.items()))})")
    print(f"  dependency edges: {dep_edges} ({dep_migrated} resolve to migrated tasks)")
    print(f"  exec_plan references: {plan_links}")
    print(f"  tasks with a ## Comments section (migrated as one issue comment): {comment_count}")
    if dropped_labels:
        print(f"  labels dropped (not in vocabulary and no mapping): {', '.join(dropped_labels)}")
    else:
        print("  labels dropped: none")
    return 0


def map_labels(ahm_labels: list[str], vocabulary: set[str] | None) -> tuple[list[str], list[str]]:
    mapped: list[str] = []
    dropped: list[str] = []
    for label in ahm_labels:
        canonical = LABEL_MAP.get(label, label)
        if vocabulary is None or canonical in vocabulary:
            if canonical not in mapped:
                mapped.append(canonical)
        else:
            dropped.append(label)
    return mapped, dropped


def migrate(tasks: list[dict[str, object]], active_ids: set[str], vocabulary: set[str],
            repo: str, owner: str, project: int, limit: int | None,
            no_comments: bool, map_out: str | None, map_in: dict[str, int]) -> int:
    available = [t for t in tasks if str(t["id"]) not in map_in]
    selected = available if limit is None else available[:limit]
    token_ids = {str(t["id"]) for t in selected}
    resolvable_ids = token_ids | set(map_in)
    mapping = dict(map_in)
    created_count = 0

    def fail(msg: str) -> int:
        err(msg)
        if created_count:
            err(f"{created_count} issue(s) were created before the failure; "
                "delete them before rerunning, or pass --map-in to resume "
                "(issues that failed mid-provisioning are skipped)")
        if map_out and mapping:
            _write_map(mapping, map_out)
        return 1

    for task in selected:
        task_id = str(task["id"])
        body, comments = compose_body(task, resolvable_ids, active_ids)
        labels, dropped = map_labels(task["labels"], vocabulary)  # type: ignore[arg-type]
        for name in dropped:
            print(f"note: ahm {task_id}: dropping label {name} (not in vocabulary)")
        label_args: list[str] = []
        for label in labels:
            label_args += ["--label", label]
        proc = gh(
            ["issue", "create", "--repo", repo, "--title", str(task["title"]),
             "--body", body] + label_args
        )
        if proc.returncode != 0:
            return fail(f"failed to create issue for ahm {task_id}: {proc.stderr.strip()}")
        url = proc.stdout.strip()
        try:
            number = int(url.rsplit("/", 1)[1])
        except (IndexError, ValueError):
            return fail(f"unexpected `gh issue create` output for ahm {task_id}: {url!r}")
        mapping[task_id] = number
        created_count += 1
        print(f"created #{number} (ahm {task_id}): {task['title']}")
        if map_out:
            _write_map(mapping, map_out)

        proc = gh(["project", "item-add", str(project), "--owner", owner, "--url", url,
                   "--format", "json"])
        if proc.returncode != 0:
            return fail(f"failed to add #{number} to project {project}: {proc.stderr.strip()}")
        status_field = STATUS_TO_FIELD.get(str(task["status"]), str(task["status"]))
        for field, value in (("Status", status_field), ("Priority", str(task["priority"])),
                             ("Effort", str(task["effort"]))):
            proc = gh(["project", "item-edit", str(project), "--owner", owner, "--url", url,
                       "--field", field, "--value", value])
            if proc.returncode != 0:
                return fail(f"failed to set {field}={value} on #{number}: {proc.stderr.strip()}")
        print(f"  set Status={status_field} Priority={task['priority']} Effort={task['effort']}")

        if not no_comments and comments:
            comment = f"> Migrated comments from ahm task {task_id}.\n\n{comments}"
            proc = gh(["api", f"repos/{repo}/issues/{number}/comments", "-f", f"body={comment}"])
            if proc.returncode != 0:
                return fail(f"failed to post comments on #{number}: {proc.stderr.strip()}")
            print(f"  posted {len(comments.splitlines())} comment line(s)")

    # Second pass: rewrite ahm#N tokens to GitHub numbers in Depends on sections.
    # Longest ids first so a#29 cannot corrupt a#298 or a#124a-style tokens.
    for task in selected:
        task_id = str(task["id"])
        number = mapping[task_id]
        proc = gh(["api", f"repos/{repo}/issues/{number}", "--jq", ".body"])
        if proc.returncode != 0:
            return fail(f"failed to read body of #{number}: {proc.stderr.strip()}")
        body = proc.stdout
        deps = sorted(task["depends_on"], key=lambda d: len(str(d)), reverse=True)  # type: ignore[union-attr]
        for dep in deps:
            dep_id = str(dep)
            if dep_id in mapping:
                body = body.replace(f"ahm#{dep_id}", f"#{mapping[dep_id]}")
        proc = gh(["api", "-X", "PATCH", f"repos/{repo}/issues/{number}", "-f", f"body={body}"])
        if proc.returncode != 0:
            return fail(f"failed to update body of #{number}: {proc.stderr.strip()}")

    print(f"migrated {len(selected)} of {len(available)} remaining ahm tasks"
          + (f" ({len(map_in)} already migrated, skipped)" if map_in else ""))
    for task_id, number in sorted(mapping.items(), key=lambda kv: int(kv[0])):
        print(f"  ahm {task_id} -> #{number} ({issue_url(repo, number)})")
    if map_out:
        _write_map(mapping, map_out)
        print(f"mapping written to {map_out}")
    return 0


def _write_map(mapping: dict[str, int], path: str) -> None:
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(mapping, fh, indent=2)
        fh.write("\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true",
                        help="print the migration plan without creating anything")
    parser.add_argument("--limit", type=int,
                        help="create only the first N tasks in ahm-id order")
    parser.add_argument("--no-comments", action="store_true",
                        help="keep the ## Comments section in the body instead of posting it")
    parser.add_argument("--map-out", metavar="FILE",
                        help="write the ahm-id -> issue-number mapping as JSON")
    parser.add_argument("--map-in", metavar="FILE",
                        help="JSON map of already-migrated ahm-id -> issue-number; "
                             "those tasks are skipped and their dependencies resolve to links")
    parser.add_argument("--project", type=int, default=1,
                        help="Projects v2 project number (default: 1, Cake Backlog)")
    args = parser.parse_args()

    try:
        tasks = load_tasks()
    except (OSError, ValueError) as exc:
        err(str(exc))
        return 1
    try:
        vocabulary = load_vocabulary()
    except ValueError as exc:
        err(str(exc))
        return 1
    if vocabulary is None:
        print("note: .github/labels.yml not on this branch yet (PR #44); "
              "label vocabulary validation skipped")
    active_ids = {str(t["id"]) for t in tasks}

    map_in: dict[str, int] = {}
    if args.map_in:
        try:
            with open(args.map_in, encoding="utf-8") as fh:
                data = json.load(fh)
            if not isinstance(data, dict):
                raise ValueError("top-level JSON must be an object mapping ahm-id to issue-number")
            for key, value in data.items():
                if isinstance(value, bool) or not isinstance(value, int):
                    raise ValueError(f"value for ahm id {key!r} must be an issue number")
                map_in[str(key)] = value
        except (OSError, ValueError, TypeError) as exc:
            err(f"cannot read --map-in {args.map_in}: {exc}")
            return 1

    if args.dry_run:
        return dry_run(tasks, active_ids, vocabulary, set(map_in))

    repo = repo_slug()
    owner = repo.split("/")[0]
    return migrate(tasks, active_ids, vocabulary, repo, owner, args.project,
                   args.limit, args.no_comments, args.map_out, map_in)


if __name__ == "__main__":
    sys.exit(main())
