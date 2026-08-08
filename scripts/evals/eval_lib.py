#!/usr/bin/env python3
"""Core library for the controlled model evaluation harness.

Runs identical fixture coding tasks across selected model configurations and
judges the resulting repository state with deterministic, repository-owned
verifier commands. This is contributor tooling only: it shells out to the
installed `cake` CLI and never changes production behavior.

Outcome vocabulary (deliberately stable within this tool):

- correct         verifier passed
- incorrect       verifier ran and failed
- cake_error      cake exited 1 (agent or tool execution error)
- provider_error  cake exited 2 (authentication, rate-limit, or network)
- timeout         the cake process exceeded the case timeout
- harness_error   malformed completion JSON, a broken invocation (cake exit 3,
                  invalid flags/configuration/missing credentials), a verifier
                  that could not run, or any harness defect

Correctness is decided by the fixture verifier alone; cake's session `success`
subtype is never treated as correctness.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
TOOL_NAME = "cake-eval"
TOOL_VERSION = "0.1.0"

OUTCOME_CORRECT = "correct"
OUTCOME_INCORRECT = "incorrect"
OUTCOME_CAKE_ERROR = "cake_error"
OUTCOME_PROVIDER_ERROR = "provider_error"
OUTCOME_TIMEOUT = "timeout"
OUTCOME_HARNESS_ERROR = "harness_error"
OUTCOMES = (
    OUTCOME_CORRECT,
    OUTCOME_INCORRECT,
    OUTCOME_CAKE_ERROR,
    OUTCOME_PROVIDER_ERROR,
    OUTCOME_TIMEOUT,
    OUTCOME_HARNESS_ERROR,
)

# Verifiers are small, deterministic, network-free commands; a generous fixed
# cap keeps a broken fixture from stalling the whole run.
VERIFIER_TIMEOUT_SECONDS = 60
# Grace period between SIGTERM and SIGKILL when terminating a timed-out cake
# process group.
KILL_GRACE_SECONDS = 10

# Captured text embedded in the aggregate JSON is truncated so generated files
# stay lean and diagnostics stay readable.
MAX_TEXT_CHARS = 4_000
MAX_RESULT_PREVIEW_CHARS = 200

REQUIRED_MANIFEST_FIELDS = ("prompt", "verify", "timeout_seconds", "tags", "expected")


class HarnessError(Exception):
    """Configuration or harness defect that should fail the whole run."""


@dataclass(frozen=True)
class Case:
    """One committed or temporary evaluation fixture."""

    name: str
    prompt: str
    verify: str
    timeout_seconds: float
    tags: tuple[str, ...]
    expected: str
    description: str
    directory: Path

    @property
    def repo_dir(self) -> Path:
        """The fixture's initial repository state (not copied to the model yet)."""
        return self.directory / "repo"


# ---------------------------------------------------------------------------
# Case loading and validation
# ---------------------------------------------------------------------------

def load_case(case_dir: Path) -> Case:
    """Load and validate one fixture case from a directory."""
    manifest_path = case_dir / "manifest.json"
    try:
        manifest = json.loads(manifest_path.read_text())
    except OSError as exc:
        raise HarnessError(f"{case_dir}: cannot read manifest.json: {exc}") from exc
    except json.JSONDecodeError as exc:
        raise HarnessError(f"{case_dir}: manifest.json is not valid JSON: {exc}") from exc
    if not isinstance(manifest, dict):
        raise HarnessError(f"{case_dir}: manifest.json must be a JSON object")

    name = str(manifest.get("name") or case_dir.name)
    missing = [field for field in REQUIRED_MANIFEST_FIELDS if field not in manifest]
    if missing:
        raise HarnessError(
            f"case {name}: manifest.json missing required field(s): {', '.join(missing)}"
        )

    expected = str(manifest["expected"])
    if expected not in OUTCOMES:
        raise HarnessError(
            f"case {name}: expected {expected!r} is not one of {', '.join(OUTCOMES)}"
        )

    tags = manifest["tags"]
    if not isinstance(tags, list) or not all(isinstance(t, str) and t for t in tags):
        raise HarnessError(
            f"case {name}: tags must be a non-empty list of non-empty strings"
        )

    try:
        timeout = float(manifest["timeout_seconds"])
    except (TypeError, ValueError) as exc:
        raise HarnessError(f"case {name}: timeout_seconds must be a number") from exc
    if timeout <= 0:
        raise HarnessError(f"case {name}: timeout_seconds must be positive")

    repo_dir = case_dir / "repo"
    if not repo_dir.is_dir() or not any(repo_dir.iterdir()):
        raise HarnessError(f"case {name}: repo/ directory must exist and contain files")

    verify = str(manifest["verify"]).strip()
    if not verify:
        raise HarnessError(f"case {name}: verify must be a non-empty shell command")
    if "verify.sh" in verify and not (case_dir / "verify.sh").is_file():
        raise HarnessError(
            f"case {name}: verify references verify.sh but "
            f"{case_dir / 'verify.sh'} does not exist"
        )

    return Case(
        name=name,
        prompt=str(manifest["prompt"]),
        verify=verify,
        timeout_seconds=timeout,
        tags=tuple(tags),
        expected=expected,
        description=str(manifest.get("description", "")),
        directory=case_dir.resolve(),
    )


def load_cases(cases_dir: Path) -> list[Case]:
    """Load every fixture case under a case directory, sorted by name."""
    if not cases_dir.is_dir():
        raise HarnessError(f"cases directory not found: {cases_dir}")
    cases = []
    for entry in sorted(cases_dir.iterdir()):
        if entry.is_dir() and (entry / "manifest.json").is_file():
            cases.append(load_case(entry))
    return cases


def discover_cases(case_dirs: list[Path]) -> list[Case]:
    """Load cases from several roots, rejecting duplicate names."""
    cases: list[Case] = []
    seen: set[str] = set()
    for cases_dir in case_dirs:
        for case in load_cases(cases_dir):
            if case.name in seen:
                raise HarnessError(
                    f"duplicate case name {case.name!r} across case directories"
                )
            seen.add(case.name)
            cases.append(case)
    return cases


# ---------------------------------------------------------------------------
# Trial execution
# ---------------------------------------------------------------------------

def run_trial(
    case: Case, model: str, repetition: int, cake_command: list[str], results_dir: Path
) -> dict[str, Any]:
    """Run one (case, model, repetition) trial in a fresh isolated git repo.

    The temporary repository is deleted on every path, including errors and
    interrupts, so neither the fixture sources nor the cake source tree are
    ever modified.
    """
    record: dict[str, Any] = {
        "case": case.name,
        "case_tags": list(case.tags),
        "model": model,
        "repetition": repetition,
        "outcome": OUTCOME_HARNESS_ERROR,
        "exit_code": None,
        "duration_ms": None,
        "turns": None,
        "tool_calls": None,
        "tool_failures": None,
        "usage": None,
        "model_reported": None,
        "cake_elapsed_ms": None,
        "result_preview": None,
        "error": None,
        "verifier": None,
        "session_id": None,
        "session_file": None,
        "cake_stderr": None,
    }
    data_dir = results_dir / "data"
    started = time.monotonic()
    # run_cake records the live subprocess here so an interrupt or error can
    # terminate it even when run_cake raises before returning.
    proc_holder: dict[str, Any] = {}
    try:
        with tempfile.TemporaryDirectory(prefix="cake-eval-") as tmp:
            work = Path(tmp) / "work"
            create_repo(work, case.repo_dir)
            run_cake(case, model, cake_command, work, data_dir, record, proc_holder)
    except KeyboardInterrupt:
        terminate_live_group(proc_holder)
        raise
    except Exception as exc:  # noqa: BLE001 - any harness defect is a harness_error trial
        terminate_live_group(proc_holder)
        record["outcome"] = OUTCOME_HARNESS_ERROR
        record["error"] = f"harness error: {exc}"
    record["duration_ms"] = int((time.monotonic() - started) * 1000)
    return record


def terminate_live_group(proc_holder: dict[str, Any]) -> None:
    """Terminate a recorded cake subprocess that is still running."""
    proc = proc_holder.get("proc")
    if proc is not None and proc.poll() is None:
        terminate_group(proc)


def create_repo(work: Path, repo_src: Path) -> None:
    """Copy the fixture's initial state into a fresh git repository."""
    work.mkdir(parents=True)
    for entry in sorted(repo_src.iterdir()):
        dst = work / entry.name
        if entry.is_dir():
            shutil.copytree(entry, dst)
        else:
            shutil.copy2(entry, dst)
    git(work, "init", "-q")
    git(work, "config", "user.name", "cake-eval")
    git(work, "config", "user.email", "cake-eval@localhost")
    # Mirror the repo's fixture convention (src/config/git.rs pins
    # core.hooksPath locally): the initial commit must not run hooks from the
    # developer's global git configuration, and a global commit.gpgSign must
    # not break (or stall on a signing prompt) every trial's commit.
    git(work, "config", "core.hooksPath", "/dev/null")
    git(work, "config", "commit.gpgSign", "false")
    git(work, "add", "-A")
    git(work, "commit", "-q", "-m", "initial state")


def git(repo: Path, *args: str) -> None:
    subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )


def run_cake(
    case: Case,
    model: str,
    cake_command: list[str],
    work: Path,
    data_dir: Path,
    record: dict[str, Any],
    proc_holder: dict[str, Any],
) -> None:
    """Invoke cake once and classify the trial outcome into ``record``."""
    data_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["CAKE_DATA_DIR"] = str(data_dir)
    command = [*cake_command, "--output-format", "json", "--model", model, case.prompt]
    proc = subprocess.Popen(
        command,
        cwd=work,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    proc_holder["proc"] = proc
    try:
        stdout, stderr = proc.communicate(timeout=case.timeout_seconds)
    except subprocess.TimeoutExpired as exc:
        terminate_group(proc)
        record["outcome"] = OUTCOME_TIMEOUT
        record["exit_code"] = None
        record["cake_stderr"] = truncate(exc.stderr or "")
        record["error"] = f"exceeded the {case.timeout_seconds:.0f}s case timeout"
        return

    record["exit_code"] = proc.returncode
    record["cake_stderr"] = truncate(stderr)

    if proc.returncode == 0:
        try:
            completion = parse_completion(stdout)
        except ValueError as exc:
            record["outcome"] = OUTCOME_HARNESS_ERROR
            record["error"] = str(exc)
            return
        record.update(completion_fields(completion, data_dir))
        if completion.get("result") is None:
            record["outcome"] = OUTCOME_CAKE_ERROR
            record["error"] = completion.get("error") or "cake returned no result text"
            return
        verifier = run_verifier(case, work)
        record["verifier"] = verifier
        if verifier["timed_out"]:
            record["outcome"] = OUTCOME_HARNESS_ERROR
            record["error"] = (
                f"verifier exceeded the {VERIFIER_TIMEOUT_SECONDS}s verifier timeout"
            )
        elif verifier["exit_code"] in (126, 127):
            # 126/127 mean the verifier command itself could not launch (not
            # executable, or not found) - a fixture/harness defect, not a
            # model result.
            record["outcome"] = OUTCOME_HARNESS_ERROR
            record["error"] = (
                "verifier could not run (exit 126/127: command not found or not "
                "executable)"
            )
        elif verifier["exit_code"] == 0:
            record["outcome"] = OUTCOME_CORRECT
        else:
            record["outcome"] = OUTCOME_INCORRECT
    elif proc.returncode == 1:
        record["outcome"] = OUTCOME_CAKE_ERROR
        record["error"] = "cake exited 1 (agent or tool execution error)"
    elif proc.returncode == 2:
        record["outcome"] = OUTCOME_PROVIDER_ERROR
        record["error"] = "cake exited 2 (authentication, rate-limit, or network error)"
    elif proc.returncode == 3:
        record["outcome"] = OUTCOME_HARNESS_ERROR
        record["error"] = (
            "cake exited 3 (invalid flags, configuration, or missing credentials)"
        )
    else:
        record["outcome"] = OUTCOME_HARNESS_ERROR
        record["error"] = f"unexpected cake exit code {proc.returncode}"


def terminate_group(proc: subprocess.Popen) -> None:
    """Terminate a timed-out cake process group, escalating to SIGKILL."""
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        proc.communicate(timeout=KILL_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.communicate()


def parse_completion(stdout: str) -> dict[str, Any]:
    """Parse cake's completion JSON; raise ValueError when malformed."""
    try:
        value = json.loads(stdout)
    except json.JSONDecodeError as exc:
        raise ValueError(f"malformed completion JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError("completion JSON is not an object")
    return value


def completion_fields(completion: dict[str, Any], data_dir: Path) -> dict[str, Any]:
    """Extract trial metrics from completion JSON plus session/sidecar files."""
    fields: dict[str, Any] = {
        "session_id": completion.get("session_id"),
        "session_file": completion.get("session_file"),
        "turns": completion.get("turns"),
        "cake_elapsed_ms": completion.get("elapsed_time"),
        "usage": completion.get("usage"),
        "result_preview": preview(completion.get("result")),
    }
    session_file = completion.get("session_file")
    if isinstance(session_file, str) and session_file:
        try:
            session = parse_session_file(Path(session_file))
        except (OSError, json.JSONDecodeError):
            session = {}
        fields["model_reported"] = session.get("model")
        if fields["turns"] is None:
            fields["turns"] = session.get("turn_count")
        if fields["usage"] is None:
            fields["usage"] = session.get("usage")
        fields["tool_calls"] = session.get("tool_call_count")
    session_id = completion.get("session_id")
    if isinstance(session_id, str) and session_id:
        fields["tool_failures"] = tool_failures_from_sidecar(session_id, data_dir)
    return {key: value for key, value in fields.items() if value is not None}


def parse_session_file(path: Path) -> dict[str, Any]:
    """Extract model and task metrics from a cake session JSONL file."""
    result: dict[str, Any] = {}
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        record = json.loads(line)
        rtype = record.get("type")
        if rtype == "session_meta" and result.get("model") is None:
            result["model"] = record.get("model")
        elif rtype == "task_complete":
            result["tool_call_count"] = record.get("tool_call_count")
            result["turn_count"] = record.get("turn_count")
            result["usage"] = record.get("usage")
    return result


def tool_failures_from_sidecar(session_id: str, data_dir: Path) -> int | None:
    """Count failed tool calls from the telemetry sidecar; None when absent."""
    sidecar = data_dir / "session-telemetry" / f"{session_id}.ndjson"
    if not sidecar.is_file():
        return None
    failures = 0
    for line in sidecar.read_text().splitlines():
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("type") == "tool_call" and record.get("tool_call", {}).get("was_error"):
            failures += 1
    return failures


def run_verifier(case: Case, work: Path) -> dict[str, Any]:
    """Run the fixture's trusted verifier in the work repository."""
    env = os.environ.copy()
    env["EVAL_CASE_DIR"] = str(case.directory)
    try:
        proc = subprocess.run(
            ["bash", "-c", case.verify],
            cwd=work,
            env=env,
            capture_output=True,
            text=True,
            timeout=VERIFIER_TIMEOUT_SECONDS,
        )
        return {
            "exit_code": proc.returncode,
            "timed_out": False,
            "stdout": truncate(proc.stdout),
            "stderr": truncate(proc.stderr),
        }
    except subprocess.TimeoutExpired as exc:
        return {
            "exit_code": None,
            "timed_out": True,
            "stdout": truncate(exc.stdout or ""),
            "stderr": truncate(exc.stderr or ""),
        }


# ---------------------------------------------------------------------------
# Summary aggregation
# ---------------------------------------------------------------------------

def percentile(values: list[float], pct: float) -> float | None:
    """Nearest-rank percentile; None for empty input."""
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, round(pct / 100 * len(ordered)))
    return ordered[min(rank, len(ordered)) - 1]


def trial_stats(trials: list[dict[str, Any]]) -> dict[str, Any]:
    """Aggregate correctness and median/p90 metrics over a set of trials."""
    counts = {outcome: 0 for outcome in OUTCOMES}
    turns: list[float] = []
    tokens: list[float] = []
    durations: list[float] = []
    tool_failures: list[float] = []
    tool_calls_total = 0
    tool_failures_total = 0
    for trial in trials:
        outcome = trial.get("outcome")
        counts[outcome if outcome in counts else OUTCOME_HARNESS_ERROR] += 1
        if trial.get("turns") is not None:
            turns.append(float(trial["turns"]))
        usage = trial.get("usage") or {}
        if usage.get("total_tokens") is not None:
            tokens.append(float(usage["total_tokens"]))
        if trial.get("duration_ms") is not None:
            durations.append(float(trial["duration_ms"]))
        if trial.get("tool_calls") is not None:
            tool_calls_total += int(trial["tool_calls"])
        if trial.get("tool_failures") is not None:
            tool_failures.append(float(trial["tool_failures"]))
            tool_failures_total += int(trial["tool_failures"])
    n = len(trials)
    return {
        "trials": n,
        "outcomes": counts,
        "correctness_rate": (counts[OUTCOME_CORRECT] / n) if n else None,
        "median_turns": percentile(turns, 50),
        "p90_turns": percentile(turns, 90),
        "median_tokens": percentile(tokens, 50),
        "p90_tokens": percentile(tokens, 90),
        "median_duration_ms": percentile(durations, 50),
        "p90_duration_ms": percentile(durations, 90),
        "tool_calls": tool_calls_total,
        "tool_failures": tool_failures_total,
        "median_tool_failures": percentile(tool_failures, 50),
        "p90_tool_failures": percentile(tool_failures, 90),
    }


def summarize(trials: list[dict[str, Any]]) -> dict[str, Any]:
    """Build overall, by-model, and by-tag summary blocks."""
    by_model: dict[str, list[dict[str, Any]]] = {}
    by_tag: dict[str, list[dict[str, Any]]] = {}
    for trial in trials:
        by_model.setdefault(trial["model"], []).append(trial)
        for tag in trial.get("case_tags", []):
            by_tag.setdefault(tag, []).append(trial)
    return {
        "overall": trial_stats(trials),
        "by_model": {model: trial_stats(ts) for model, ts in sorted(by_model.items())},
        "by_tag": {tag: trial_stats(ts) for tag, ts in sorted(by_tag.items())},
    }


# ---------------------------------------------------------------------------
# Text helpers
# ---------------------------------------------------------------------------

def truncate(text: str | None) -> str | None:
    """Trim and bound captured text for the aggregate JSON."""
    if text is None:
        return None
    text = text.strip()
    if len(text) > MAX_TEXT_CHARS:
        return text[:MAX_TEXT_CHARS] + f"...[truncated {len(text) - MAX_TEXT_CHARS} chars]"
    return text


def preview(text: Any) -> str | None:
    """Single-line bounded preview of the model's final result text."""
    if text is None:
        return None
    compact = " ".join(str(text).split())
    if len(compact) > MAX_RESULT_PREVIEW_CHARS:
        return compact[:MAX_RESULT_PREVIEW_CHARS] + "..."
    return compact
