#!/usr/bin/env python3
"""Fake cake executable for eval-harness tests and credential-free smoke runs.

Reads FAKE_CAKE_SCRIPT, a path to a JSON file describing one behavior:

- exit_code: int, process exit status (default 0)
- stdout: "json" (default) | "malformed" | "empty"
- result: string, completion result text (default "done")
- error: string, completion error text (sets result to null)
- turns, elapsed_time, usage, session_id: completion JSON fields
- model: session_meta.model (default: the --model argument)
- tool_calls: task_complete.tool_call_count (default 3)
- total_tool_calls: number of tool_call telemetry records (default tool_calls)
- tool_failures: how many of those records have was_error true (default 0)
- write_session: bool, write session JSONL + telemetry sidecar (default true)
- write_files: {relative path: content}, overwrite files under cwd (default {})
- sleep_seconds: float, sleep before responding (for timeout tests)
- record_file: path to append one JSON line per invocation (argv, cwd,
  repo fingerprint, and model, for presentation-identity assertions)

The fake mirrors the completion JSON shape and the session/telemetry record
shapes that the harness consumes, so the harness's parsing paths are exercised
without any provider or network access.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time
import uuid
from pathlib import Path


def _argv_value(argv: list[str], flag: str) -> str | None:
    if flag in argv:
        idx = argv.index(flag)
        if idx + 1 < len(argv):
            return argv[idx + 1]
    return None


def _fingerprint(root: Path) -> dict[str, str]:
    """Map of relative path to sha256 over non-git files under root."""
    digest: dict[str, str] = {}
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d != ".git"]
        for name in sorted(filenames):
            path = Path(dirpath) / name
            rel = path.relative_to(root).as_posix()
            digest[rel] = hashlib.sha256(path.read_bytes()).hexdigest()
    return digest


def _write_session(
    data_dir: str,
    session_id: str,
    model: str,
    tool_calls: int,
    total_tool_calls: int,
    tool_failures: int,
    turns: int,
    usage: dict,
) -> Path:
    sessions_dir = Path(data_dir) / "sessions"
    sessions_dir.mkdir(parents=True, exist_ok=True)
    session_path = sessions_dir / f"{session_id}.jsonl"
    with session_path.open("w") as f:
        f.write(json.dumps({
            "type": "session_meta",
            "session_id": session_id,
            "format_version": 4,
            "cake_version": "0.0.0-fake",
            "model": model,
            "working_directory": os.getcwd(),
            "timestamp": "2026-01-01T00:00:00Z",
        }) + "\n")
        f.write(json.dumps({
            "type": "task_complete",
            "session_id": session_id,
            "task_id": str(uuid.uuid4()),
            "duration_ms": 1234,
            "turn_count": turns,
            "tool_call_count": tool_calls,
            "subtype": "success",
            "is_error": False,
            "usage": usage,
        }) + "\n")

    telemetry_dir = Path(data_dir) / "session-telemetry"
    telemetry_dir.mkdir(parents=True, exist_ok=True)
    telemetry_path = telemetry_dir / f"{session_id}.ndjson"
    with telemetry_path.open("w") as f:
        f.write(json.dumps({
            "type": "telemetry_init",
            "session_id": session_id,
            "invocation_id": str(uuid.uuid4()),
            "timestamp": "2026-01-01T00:00:00Z",
            "mode": "new",
            "working_directory": os.getcwd(),
            "model": model,
            "api_type": "chat",
            "output_format": "json",
            "tools": [],
            "settings": {},
        }) + "\n")
        for i in range(total_tool_calls):
            f.write(json.dumps({
                "type": "tool_call",
                "session_id": session_id,
                "invocation_id": str(uuid.uuid4()),
                "timestamp": "2026-01-01T00:00:00Z",
                "tool_call": {
                    "turn_index": i,
                    "call_id": f"call-{i}",
                    "name": "bash",
                    "duration_ms": 10,
                    "output_bytes": 4,
                    "was_error": i < tool_failures,
                },
            }) + "\n")
        f.write(json.dumps({
            "type": "session_summary",
            "session_id": session_id,
            "invocation_id": str(uuid.uuid4()),
            "timestamp": "2026-01-01T00:00:00Z",
            "success": True,
            "duration_ms": 1234,
            "turn_count": turns,
            "usage": usage,
        }) + "\n")
    return session_path


def main() -> int:
    script_path = os.environ.get("FAKE_CAKE_SCRIPT")
    script: dict = {}
    if script_path:
        with open(script_path) as f:
            script = json.load(f)

    model = script.get("model") or _argv_value(sys.argv, "--model") or "fake-model"

    record_file = script.get("record_file")
    if record_file:
        record = {
            "argv": sys.argv[1:],
            "cwd": os.getcwd(),
            "repo_fingerprint": _fingerprint(Path.cwd()),
            "model": model,
            "pid": os.getpid(),
        }
        with open(record_file, "a") as f:
            f.write(json.dumps(record) + "\n")

    for rel, content in (script.get("write_files") or {}).items():
        target = Path.cwd() / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)

    sleep_seconds = script.get("sleep_seconds", 0)
    if sleep_seconds:
        time.sleep(sleep_seconds)

    stdout_kind = script.get("stdout", "json")
    if stdout_kind == "malformed":
        print("{not json")
    elif stdout_kind == "empty":
        pass
    else:
        session_id = script.get("session_id") or str(uuid.uuid4())
        turns = script.get("turns", 3)
        tool_calls = script.get("tool_calls", 3)
        usage = script.get("usage", {
            "input_tokens": 100,
            "output_tokens": 50,
            "total_tokens": 150,
            "input_tokens_details": {"cached_tokens": 20},
            "output_tokens_details": {"reasoning_tokens": 10},
        })
        session_file = None
        data_dir = os.environ.get("CAKE_DATA_DIR")
        if data_dir and script.get("write_session", True):
            session_file = str(_write_session(
                data_dir,
                session_id,
                model,
                tool_calls,
                script.get("total_tool_calls", tool_calls),
                script.get("tool_failures", 0),
                turns,
                usage,
            ))
        completion = {
            "session_id": session_id,
            "usage": usage,
            "cwd": os.getcwd(),
            "session_file": session_file,
            "turns": turns,
            "elapsed_time": script.get("elapsed_time", 1000),
        }
        if script.get("error"):
            completion["result"] = None
            completion["error"] = script["error"]
        else:
            completion["result"] = script.get("result", "done")
        print(json.dumps(completion))

    return script.get("exit_code", 0)


if __name__ == "__main__":
    sys.exit(main())
