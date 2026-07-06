"""Shared loading, pairing, classification, and formatting for cake session metrics.

Data sources:
- Session transcripts: ~/.local/share/cake/sessions/{uuid}.jsonl
  (or {CAKE_DATA_DIR}/sessions). Record types span format versions:
  session_meta/session_start/init, message, function_call,
  function_call_output, reasoning, hook_event, prompt_context,
  task_start, task_complete, result, skill_activated.
- Telemetry sidecars: ~/.cache/cake/session-telemetry/{uuid}.ndjson
  Record types: telemetry_init, api_attempt, retry_scheduled, tool_call,
  session_summary. One file can span multiple invocations (continue/resume).
"""

from __future__ import annotations

import argparse
import json
import os
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path

DEFAULT_SESSIONS_DIR = Path.home() / ".local/share/cake/sessions"
DEFAULT_TELEMETRY_DIR = Path.home() / ".cache/cake/session-telemetry"


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def build_arg_parser(description: str) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument(
        "--days", type=int, default=30,
        help="Only include files modified in the last N days (0 = all time). Default 30.",
    )
    parser.add_argument(
        "--sessions-dir", type=Path, default=None,
        help="Session transcripts directory (default: $CAKE_DATA_DIR/sessions or ~/.local/share/cake/sessions)",
    )
    parser.add_argument(
        "--telemetry-dir", type=Path, default=None,
        help="Telemetry sidecar directory (default: ~/.cache/cake/session-telemetry)",
    )
    parser.add_argument(
        "--model", default=None,
        help="Only include sessions/invocations whose model contains this substring",
    )
    parser.add_argument(
        "--project", default=None,
        help="Only include sessions/invocations whose working directory contains this substring",
    )
    return parser


def resolve_sessions_dir(ns: argparse.Namespace) -> Path:
    if ns.sessions_dir:
        return ns.sessions_dir
    data_dir = os.environ.get("CAKE_DATA_DIR")
    if data_dir:
        return Path(data_dir) / "sessions"
    return DEFAULT_SESSIONS_DIR


def resolve_telemetry_dir(ns: argparse.Namespace) -> Path:
    if ns.telemetry_dir:
        return ns.telemetry_dir
    return DEFAULT_TELEMETRY_DIR


# ---------------------------------------------------------------------------
# Data model
# ---------------------------------------------------------------------------

@dataclass
class ToolCall:
    """A paired function_call + function_call_output from a session transcript."""
    seq: int
    name: str
    call_id: str
    arguments: str
    output: str
    ok: bool
    timestamp: str | None

    @property
    def path(self) -> str | None:
        """Target path for file tools, when the arguments parse."""
        try:
            return json.loads(self.arguments).get("path")
        except (json.JSONDecodeError, AttributeError):
            return None


@dataclass
class Session:
    """One session transcript file."""
    id: str
    path: Path
    size: int
    mtime: datetime
    records: list[dict]

    # populated by load_sessions
    model: str = "unknown"
    cake_version: str = "unknown"
    working_directory: str = "unknown"
    format_version: int | None = None
    parse_errors: int = 0

    _tool_calls: list[ToolCall] | None = field(default=None, repr=False)

    def by_type(self, *types: str) -> list[dict]:
        return [r for r in self.records if r.get("type") in types]

    @property
    def tool_calls(self) -> list[ToolCall]:
        if self._tool_calls is None:
            self._tool_calls = pair_tool_calls(self.records)
        return self._tool_calls


@dataclass
class Invocation:
    """One cake invocation within a telemetry sidecar."""
    session_id: str
    invocation_id: str
    init: dict | None = None
    attempts: list[dict] = field(default_factory=list)
    retries: list[dict] = field(default_factory=list)
    tool_calls: list[dict] = field(default_factory=list)
    summary: dict | None = None

    @property
    def model(self) -> str:
        return (self.init or {}).get("model", "unknown")

    @property
    def working_directory(self) -> str:
        return (self.init or {}).get("working_directory", "unknown")


@dataclass
class Dataset:
    sessions: list[Session]
    invocations: list[Invocation]
    sessions_dir: Path
    telemetry_dir: Path
    cutoff: datetime | None
    session_parse_errors: int = 0
    telemetry_parse_errors: int = 0


# ---------------------------------------------------------------------------
# Loading
# ---------------------------------------------------------------------------

def _cutoff(ns: argparse.Namespace) -> datetime | None:
    if ns.days and ns.days > 0:
        return datetime.now(timezone.utc) - timedelta(days=ns.days)
    return None


def _files_in_window(directory: Path, suffix: str, cutoff: datetime | None) -> list[Path]:
    if not directory.is_dir():
        return []
    files = []
    for f in directory.glob(f"*{suffix}"):
        mtime = datetime.fromtimestamp(f.stat().st_mtime, timezone.utc)
        if cutoff is None or mtime >= cutoff:
            files.append(f)
    return sorted(files, key=lambda f: f.stat().st_mtime)


def load(ns: argparse.Namespace) -> Dataset:
    """Load sessions and telemetry once; individual scripts share the result."""
    cutoff = _cutoff(ns)
    sessions_dir = resolve_sessions_dir(ns)
    telemetry_dir = resolve_telemetry_dir(ns)

    sessions, session_errors = load_sessions(sessions_dir, cutoff, ns.model, ns.project)
    invocations, telemetry_errors = load_telemetry(telemetry_dir, cutoff, ns.model, ns.project)

    return Dataset(
        sessions=sessions,
        invocations=invocations,
        sessions_dir=sessions_dir,
        telemetry_dir=telemetry_dir,
        cutoff=cutoff,
        session_parse_errors=session_errors,
        telemetry_parse_errors=telemetry_errors,
    )


def load_sessions(
    directory: Path,
    cutoff: datetime | None,
    model_filter: str | None = None,
    project_filter: str | None = None,
) -> tuple[list[Session], int]:
    sessions: list[Session] = []
    total_parse_errors = 0

    for f in _files_in_window(directory, ".jsonl", cutoff):
        records: list[dict] = []
        parse_errors = 0
        try:
            with open(f, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    try:
                        records.append(json.loads(line))
                    except json.JSONDecodeError:
                        parse_errors += 1
        except OSError:
            continue
        total_parse_errors += parse_errors

        session = Session(
            id=f.stem,
            path=f,
            size=f.stat().st_size,
            mtime=datetime.fromtimestamp(f.stat().st_mtime, timezone.utc),
            records=records,
            parse_errors=parse_errors,
        )
        for rec in records:
            t = rec.get("type")
            if t in ("session_meta", "session_start", "init"):
                session.model = rec.get("model", session.model)
                session.working_directory = rec.get("working_directory", session.working_directory)
                session.format_version = rec.get("format_version", session.format_version)
                if t == "session_meta":
                    session.cake_version = rec.get("cake_version", session.cake_version)
                break

        if model_filter and model_filter not in session.model:
            continue
        if project_filter and project_filter not in session.working_directory:
            continue
        sessions.append(session)

    return sessions, total_parse_errors


def load_telemetry(
    directory: Path,
    cutoff: datetime | None,
    model_filter: str | None = None,
    project_filter: str | None = None,
) -> tuple[list[Invocation], int]:
    grouped: dict[tuple[str, str], Invocation] = {}
    parse_errors = 0

    for f in _files_in_window(directory, ".ndjson", cutoff):
        try:
            with open(f, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError:
                        parse_errors += 1
                        continue
                    key = (rec.get("session_id", f.stem), rec.get("invocation_id", "?"))
                    inv = grouped.setdefault(key, Invocation(*key))
                    t = rec.get("type")
                    if t == "telemetry_init":
                        inv.init = rec
                    elif t == "api_attempt":
                        inv.attempts.append(rec)
                    elif t == "retry_scheduled":
                        inv.retries.append(rec)
                    elif t == "tool_call":
                        inv.tool_calls.append(rec)
                    elif t == "session_summary":
                        inv.summary = rec
        except OSError:
            continue

    invocations = []
    for inv in grouped.values():
        if model_filter and model_filter not in inv.model:
            continue
        if project_filter and project_filter not in inv.working_directory:
            continue
        invocations.append(inv)
    return invocations, parse_errors


# ---------------------------------------------------------------------------
# Pairing and classification
# ---------------------------------------------------------------------------

def pair_tool_calls(records: list[dict]) -> list[ToolCall]:
    """Pair function_call records with their function_call_output by call_id."""
    pending: dict[str, dict] = {}
    calls: list[ToolCall] = []
    seq = 0
    for rec in records:
        t = rec.get("type")
        if t == "function_call":
            pending[rec.get("call_id", "")] = rec
        elif t == "function_call_output":
            call = pending.pop(rec.get("call_id", ""), None)
            if call is None:
                continue
            output = rec.get("output") or ""
            calls.append(ToolCall(
                seq=seq,
                name=call.get("name", "unknown"),
                call_id=call.get("call_id", ""),
                arguments=call.get("arguments", ""),
                output=output,
                ok=not output.startswith("Error"),
                timestamp=call.get("timestamp"),
            ))
            seq += 1
    return calls


def classify_tool_error(name: str, output: str) -> str:
    """Bucket a failed tool output into a stable category for aggregation."""
    first = output.splitlines()[0] if output else ""

    # Cross-tool categories
    if "Rejected this" in first and "already issued" in output:
        return "duplicate-mutation guard"
    if "BLOCKED" in first:
        return "hook-blocked"
    if "read-only" in output:
        return "read-only path"
    if "Invalid" in first and "arguments" in first:
        return "invalid arguments/JSON"

    if name == "Edit":
        if "could not find the exact text to replace" in output:
            return "no-match (old_text not found)"
        if "locations but must match exactly 1" in output:
            return "ambiguous (multiple matches)"
        if "overlap" in output:
            return "overlapping edits"
        if "binary file" in output or "invalid UTF-8" in output:
            return "binary/encoding"
        if "Failed to access file" in output or "not a file" in output:
            return "path/file access"
    elif name == "Bash":
        if "timed out" in first:
            return "timeout"
        if "sandbox" in first.lower():
            return "sandbox unavailable/denied"
        if "os error" in first:
            return "os error"
    elif name == "Read":
        if "Failed to" in first or "No such file" in output or "not found" in first:
            return "path/file access"
        if "binary" in first.lower():
            return "binary/encoding"
    elif name == "Write":
        if "Failed to" in first:
            return "write/filesystem"
        if "exists" in first:
            return "already exists"

    return "other"


# ---------------------------------------------------------------------------
# Aggregation helpers
# ---------------------------------------------------------------------------

def percentile(values: list[float], pct: float) -> float:
    """Nearest-rank percentile; 0 for empty input."""
    if not values:
        return 0
    ordered = sorted(values)
    rank = max(1, round(pct / 100 * len(ordered)))
    return ordered[min(rank, len(ordered)) - 1]


def parse_ts(ts: str | None) -> datetime | None:
    if not ts:
        return None
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except ValueError:
        return None


def usage_totals(usages: list[dict]) -> dict[str, int]:
    """Sum a list of Usage dicts into input/cached/output/reasoning/total."""
    totals = {"input": 0, "cached": 0, "output": 0, "reasoning": 0, "total": 0}
    for u in usages:
        if not u:
            continue
        totals["input"] += u.get("input_tokens", 0)
        totals["cached"] += (u.get("input_tokens_details") or {}).get("cached_tokens", 0)
        totals["output"] += u.get("output_tokens", 0)
        totals["reasoning"] += (u.get("output_tokens_details") or {}).get("reasoning_tokens", 0)
        totals["total"] += u.get("total_tokens", 0)
    return totals


def group_by(items, key):
    grouped = defaultdict(list)
    for item in items:
        grouped[key(item)].append(item)
    return grouped


# ---------------------------------------------------------------------------
# Formatting
# ---------------------------------------------------------------------------

def fmt_int(n: float) -> str:
    return f"{int(n):,}"


def fmt_pct(numerator: float, denominator: float) -> str:
    return f"{100 * numerator / denominator:.1f}%" if denominator else "-"


def fmt_ms(ms: float) -> str:
    if ms >= 60_000:
        return f"{ms / 60_000:.1f}m"
    if ms >= 1_000:
        return f"{ms / 1_000:.1f}s"
    return f"{int(ms)}ms"


def fmt_bytes(n: float) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.0f}{unit}"
        n /= 1024
    return f"{n:.1f}TB"


def print_table(headers: list[str], rows: list[list[str]], indent: int = 2) -> None:
    if not rows:
        print(" " * indent + "(no data)")
        return
    widths = [len(h) for h in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(str(cell)))
    pad = " " * indent

    def line(cells):
        return pad + "  ".join(str(c).ljust(widths[i]) for i, c in enumerate(cells)).rstrip()

    print(line(headers))
    print(pad + "  ".join("-" * w for w in widths))
    for row in rows:
        print(line(row))


def print_header(title: str) -> None:
    print(f"\n{'=' * 66}\n{title}\n{'=' * 66}")


def describe_window(data: Dataset) -> str:
    window = f"last {data.cutoff and (datetime.now(timezone.utc) - data.cutoff).days} days" if data.cutoff else "all time"
    return (
        f"Window: {window} | sessions: {len(data.sessions)} ({data.sessions_dir}) | "
        f"telemetry invocations: {len(data.invocations)} ({data.telemetry_dir})"
    )
