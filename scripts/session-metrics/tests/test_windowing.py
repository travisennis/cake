#!/usr/bin/env python3
"""Unit tests for timestamp-based windowing and in-flight session detection.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import json
import os
import pathlib
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib

NOW = datetime(2026, 8, 15, 12, 0, 0, tzinfo=timezone.utc)


def T(**kw):
    return (NOW + timedelta(**kw)).isoformat()


def session(*records, mtime=NOW, sid="s1"):
    flat = []
    for rec in records:
        if isinstance(rec, list):
            flat.extend(rec)
        else:
            flat.append(rec)
    return cakelib.Session(
        id=sid,
        path=pathlib.Path(f"/tmp/{sid}.jsonl"),
        size=0,
        mtime=mtime,
        records=flat,
        parse_errors=0,
    )


def task_start(task_id, ts, **kw):
    return [{"type": "task_start", "task_id": task_id, "timestamp": ts, **kw}]


def task_complete(task_id, **kw):
    # task_complete records carry no timestamp.
    return [{"type": "task_complete", "task_id": task_id, **kw}]


class TasksInWindowTest(unittest.TestCase):
    def test_attribution_uses_task_start_timestamp(self):
        s = session(task_start("a", T(days=-30)) + task_complete("a")
                    + task_start("b", T(days=-1)) + task_complete("b"))
        cutoff = NOW - timedelta(days=7)
        got = s.tasks_in_window(cutoff)
        self.assertEqual([r["task_id"] for r in got], ["b"])

    def test_all_time_when_no_cutoff(self):
        s = session(task_start("a", T(days=-30)) + task_complete("a")
                    + task_start("b", T(days=-1)) + task_complete("b"))
        self.assertEqual(len(s.tasks_in_window(None)), 2)

    def test_incomplete_task_has_no_outcome_record(self):
        s = session(task_start("a", T(days=-1)))
        self.assertEqual(s.tasks_in_window(NOW - timedelta(days=7)), [])

    def test_legacy_result_record_uses_own_timestamp(self):
        result = {"type": "result", "timestamp": T(days=-2)}
        s = session([result])
        self.assertEqual(len(s.tasks_in_window(NOW - timedelta(days=7))), 1)
        self.assertEqual(s.tasks_in_window(NOW - timedelta(days=1)), [])


class InflightTest(unittest.TestCase):
    def test_final_task_uncompleted(self):
        s = session(task_start("a", T(days=-2)) + task_complete("a")
                    + task_start("b", T(hours=-1)))
        self.assertTrue(s.inflight)

    def test_all_tasks_completed(self):
        s = session(task_start("a", T(days=-2)) + task_complete("a")
                    + task_start("b", T(days=-1)) + task_complete("b"))
        self.assertFalse(s.inflight)

    def test_no_tasks(self):
        self.assertFalse(session([{"type": "session_meta"}]).inflight)

    def test_inflight_requires_task_start_in_window(self):
        # The session is in the window via later message activity, but the
        # uncompleted final task started before the cutoff; tasks attribute
        # to windows by task_start, so it is not in-window inflight.
        s = session(
            *task_start("a", T(days=-30)),
            {"type": "message", "role": "user", "timestamp": T(hours=-1)},
        )
        self.assertTrue(s.inflight)
        self.assertFalse(s.inflight_in_window(NOW - timedelta(days=7)))
        self.assertTrue(s.inflight_in_window(None))

    def test_inflight_task_start_in_window(self):
        s = session(task_start("a", T(days=-1)))
        self.assertTrue(s.inflight_in_window(NOW - timedelta(days=7)))


class RecordsInWindowTest(unittest.TestCase):
    def test_filters_by_record_timestamp(self):
        s = session([
            {"type": "function_call", "name": "Bash", "timestamp": T(days=-30)},
            {"type": "function_call", "name": "Bash", "timestamp": T(days=-1)},
        ])
        got = s.records_in_window(NOW - timedelta(days=7), "function_call")
        self.assertEqual(len(got), 1)

    def test_untimestamped_records_fall_back_to_mtime(self):
        mtime = NOW - timedelta(days=2)
        s = session([{"type": "message", "role": "user"}], mtime=mtime)
        self.assertEqual(len(s.records_in_window(NOW - timedelta(days=7), "message")), 1)
        self.assertEqual(s.records_in_window(NOW - timedelta(days=1), "message"), [])


class ToolCallsInWindowTest(unittest.TestCase):
    def test_filters_by_function_call_timestamp(self):
        s = session([
            {"type": "function_call", "name": "Bash", "call_id": "c1", "timestamp": T(days=-30)},
            {"type": "function_call_output", "call_id": "c1", "output": "x"},
            {"type": "function_call", "name": "Bash", "call_id": "c2", "timestamp": T(days=-1)},
            {"type": "function_call_output", "call_id": "c2", "output": "y"},
        ])
        got = s.tool_calls_in_window(NOW - timedelta(days=7))
        self.assertEqual([c.call_id for c in got], ["c2"])


class LastActivityTest(unittest.TestCase):
    def test_max_record_timestamp(self):
        s = session(
            *task_start("a", T(days=-5)),
            {"type": "message", "role": "user", "timestamp": T(days=-1)},
        )
        self.assertEqual(s.last_activity, NOW - timedelta(days=1))

    def test_falls_back_to_mtime(self):
        mtime = NOW - timedelta(days=3)
        s = session([{"type": "session_meta"}], mtime=mtime)
        self.assertEqual(s.last_activity, mtime)

    def test_mtime_counts_when_untimestamped_legacy_record_present(self):
        # A legacy result without its own timestamp is active at the file
        # mtime, even when earlier records carry timestamps, matching how
        # tasks_in_window attributes it.
        mtime = NOW - timedelta(days=2)
        s = session(
            {"type": "message", "role": "user", "timestamp": T(days=-30)},
            {"type": "result", "subtype": "success"},
            mtime=mtime,
        )
        self.assertEqual(s.last_activity, mtime)

    def test_task_complete_joins_to_task_start(self):
        # task_complete carries no timestamp; its activity is its task_start's
        # timestamp, so a restored file with fresh mtime stays on old records.
        s = session(
            *task_start("a", T(days=-30)),
            *task_complete("a"),
            mtime=NOW,
        )
        self.assertEqual(s.last_activity, NOW - timedelta(days=30))

    def test_task_complete_activity_includes_duration(self):
        # A task that started before the cutoff but ran into the window ends
        # inside it; the session's activity is the completion time, while
        # tasks_in_window still attributes the outcome to the start.
        s = session(
            *task_start("a", T(days=-8)),
            *task_complete("a", duration_ms=2 * 24 * 60 * 60 * 1000),
        )
        cutoff = NOW - timedelta(days=7)
        self.assertEqual(s.last_activity, NOW - timedelta(days=6))
        self.assertEqual(s.tasks_in_window(cutoff), [])


class LoaderWindowTest(unittest.TestCase):
    def _write(self, directory, name, records, mtime):
        path = os.path.join(directory, name)
        with open(path, "w", encoding="utf-8") as fh:
            for rec in records:
                fh.write(json.dumps(rec) + "\n")
        os.utime(path, (mtime.timestamp(), mtime.timestamp()))
        return path

    def test_old_records_with_fresh_mtime_are_excluded(self):
        with tempfile.TemporaryDirectory() as directory:
            self._write(directory, "old.jsonl", [
                {"type": "session_meta", "model": "m", "working_directory": "/p",
                 "timestamp": T(days=-30)},
                *task_start("a", T(days=-30)),
                *task_complete("a"),
            ], mtime=NOW)
            sessions, _ = cakelib.load_sessions(
                pathlib.Path(directory), NOW - timedelta(days=7), None, None,
            )
            self.assertEqual(len(sessions), 0)

    def test_new_records_with_old_mtime_are_included(self):
        # A copied/restored transcript keeps its old mtime; membership is by
        # record timestamps, so the session is still in the window.
        with tempfile.TemporaryDirectory() as directory:
            self._write(directory, "restored.jsonl", [
                {"type": "session_meta", "model": "m", "working_directory": "/p",
                 "timestamp": T(days=-30)},
                *task_start("a", T(days=-1)),
                *task_complete("a"),
            ], mtime=NOW - timedelta(days=30))
            sessions, _ = cakelib.load_sessions(
                pathlib.Path(directory), NOW - timedelta(days=7), None, None,
            )
            self.assertEqual(len(sessions), 1)

    def test_untimestamped_legacy_result_with_fresh_mtime_is_included(self):
        # The legacy result has no timestamp; its activity is the file mtime,
        # so the session stays in the window even though every timestamped
        # record is old.
        with tempfile.TemporaryDirectory() as directory:
            self._write(directory, "legacy.jsonl", [
                {"type": "message", "role": "user", "timestamp": T(days=-30)},
                {"type": "result", "subtype": "success"},
            ], mtime=NOW)
            sessions, _ = cakelib.load_sessions(
                pathlib.Path(directory), NOW - timedelta(days=7), None, None,
            )
            self.assertEqual(len(sessions), 1)

    def test_task_completing_in_window_keeps_session_included(self):
        # The task started before the cutoff but ran into the window; the
        # session is active in the window even though the task outcome still
        # attributes to its start.
        with tempfile.TemporaryDirectory() as directory:
            self._write(directory, "completing.jsonl", [
                {"type": "session_meta", "model": "m", "working_directory": "/p",
                 "timestamp": T(days=-8)},
                *task_start("a", T(days=-8)),
                *task_complete("a", duration_ms=2 * 24 * 60 * 60 * 1000),
            ], mtime=NOW)
            sessions, _ = cakelib.load_sessions(
                pathlib.Path(directory), NOW - timedelta(days=7), None, None,
            )
            self.assertEqual(len(sessions), 1)
            self.assertEqual(sessions[0].tasks_in_window(NOW - timedelta(days=7)), [])

    def test_fresh_records_are_included(self):
        with tempfile.TemporaryDirectory() as directory:
            self._write(directory, "fresh.jsonl", [
                {"type": "session_meta", "model": "m", "working_directory": "/p"},
                *task_start("a", T(days=-1)),
                *task_complete("a"),
            ], mtime=NOW)
            sessions, _ = cakelib.load_sessions(
                pathlib.Path(directory), NOW - timedelta(days=7), None, None,
            )
            self.assertEqual(len(sessions), 1)


class TelemetryWindowTest(unittest.TestCase):
    def _write(self, directory, name, records, mtime):
        path = os.path.join(directory, name)
        with open(path, "w", encoding="utf-8") as fh:
            for rec in records:
                fh.write(json.dumps(rec) + "\n")
        os.utime(path, (mtime.timestamp(), mtime.timestamp()))
        return path

    def test_old_invocation_in_resumed_sidecar_is_excluded(self):
        # Resuming a session appends a new invocation and refreshes the file
        # mtime; the invocation whose records all predate the cutoff must not
        # leak into the window.
        cutoff = NOW - timedelta(days=7)
        with tempfile.TemporaryDirectory() as directory:
            self._write(directory, "s1.ndjson", [
                {"type": "telemetry_init", "session_id": "s1", "invocation_id": "i1",
                 "model": "m", "working_directory": "/p", "timestamp": T(days=-30)},
                {"type": "api_attempt", "session_id": "s1", "invocation_id": "i1",
                 "attempt": 1, "timestamp": T(days=-30)},
                {"type": "telemetry_init", "session_id": "s1", "invocation_id": "i2",
                 "model": "m", "working_directory": "/p", "timestamp": T(days=-1)},
                {"type": "api_attempt", "session_id": "s1", "invocation_id": "i2",
                 "attempt": 1, "timestamp": T(days=-1)},
            ], mtime=NOW)
            invocations, _ = cakelib.load_telemetry(
                pathlib.Path(directory), cutoff, None, None,
            )
            self.assertEqual([inv.invocation_id for inv in invocations], ["i2"])

    def test_untimestamped_sidecar_falls_back_to_mtime(self):
        # Legacy/untimestamped sidecars keep the old mtime membership.
        with tempfile.TemporaryDirectory() as directory:
            self._write(directory, "old.ndjson", [
                {"type": "telemetry_init", "session_id": "s1", "invocation_id": "i1",
                 "model": "m", "working_directory": "/p"},
            ], mtime=NOW - timedelta(days=2))
            invocations, _ = cakelib.load_telemetry(
                pathlib.Path(directory), NOW - timedelta(days=7), None, None,
            )
            self.assertEqual(len(invocations), 1)

    def test_unknown_record_timestamp_keeps_invocation_in_window(self):
        # A sidecar from a newer cake may carry added record types; their
        # timestamps still count toward window membership.
        with tempfile.TemporaryDirectory() as directory:
            self._write(directory, "new.ndjson", [
                {"type": "telemetry_init", "session_id": "s1", "invocation_id": "i1",
                 "model": "m", "working_directory": "/p", "timestamp": T(days=-30)},
                {"type": "future_record", "session_id": "s1", "invocation_id": "i1",
                 "timestamp": T(days=-1)},
            ], mtime=NOW)
            invocations, _ = cakelib.load_telemetry(
                pathlib.Path(directory), NOW - timedelta(days=7), None, None,
            )
            self.assertEqual(len(invocations), 1)


if __name__ == "__main__":
    unittest.main()
