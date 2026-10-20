#!/usr/bin/env python3
"""Unit tests for the machine-readable summary, its diff, and report CLI wiring.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import dashboard
import report
import summary

NOW = datetime(2026, 10, 19, 12, 0, 0, tzinfo=timezone.utc)
START = NOW - timedelta(days=7)


def T(**kw):
    return (NOW + timedelta(**kw)).isoformat()


def dataset(sessions=(), invocations=()):
    return cakelib.Dataset(list(sessions), list(invocations),
                           Path("/sessions"), Path("/telemetry"), None)


def task(task_id, ts, subtype):
    return [{"type": "task_start", "task_id": task_id, "timestamp": ts},
            {"type": "task_complete", "task_id": task_id, "subtype": subtype}]


def edit(call_id, ts, output):
    return [
        {"type": "function_call", "name": "Edit", "call_id": call_id,
         "arguments": '{"path": "/a"}', "timestamp": ts},
        {"type": "function_call_output", "call_id": call_id, "output": output},
    ]


def fixture():
    session = cakelib.Session("s1", Path("/sessions/s1.jsonl"), 0, NOW, [
        {"type": "session_meta", "model": "alpha", "working_directory": "/proj"},
        *task("a", T(days=-1), "success"), *task("b", T(days=-2), "interrupted"),
        *edit("e1", T(days=-3), "Error: could not find the exact text to replace"),
        *edit("e2", T(days=-3), "ok"),
    ])
    invocation = cakelib.Invocation("s1", "i1", init={"model": "alpha", "timestamp": T(days=-3)})
    invocation.mtime = NOW
    invocation.attempts = [
        {"timestamp": T(days=-3), "status_code": 200, "request_ms": 100,
         "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 80}}},
        {"timestamp": T(days=-2), "status_code": 503, "error": "boom", "request_ms": 300},
    ]
    invocation.judge_attempts = [
        {"timestamp": T(days=-2), "terminal_class": cls, "total_ms": 10}
        for cls in ("verdict", "timeout", "transport")]
    invocation.compensations = [
        {"kind": "judge_verdict", "detail": detail, "timestamp": T(days=-2)}
        for detail in ("allow", "block:rm-rf", "warn:code")]
    invocation.summary = {"success": True}
    unterminated = cakelib.Invocation(
        "s2", "i2", init={"model": "alpha", "timestamp": T(days=-4)})
    unterminated.mtime = NOW
    return dataset([session], [invocation, unterminated])


DATA = fixture()


def document(data=None, start=START, end=NOW, **kwargs):
    return summary.summarize(data or DATA, start, end, measured=end, **kwargs)


class SummaryShapeTest(unittest.TestCase):
    def test_schema_provenance_and_shape(self):
        doc = document(filters={"model": "alpha", "project": None,
                                "session": None, "invocation": None})
        self.assertEqual(doc["schema_version"], summary.SCHEMA_VERSION)
        self.assertEqual(doc["measured"], "2026-10-19T12:00:00Z")
        self.assertEqual(doc["window"],
                         {"start": "2026-10-12T12:00:00Z", "end": "2026-10-19T12:00:00Z"})
        self.assertEqual(doc["filters"]["model"], "alpha")
        self.assertEqual(list(doc["metrics"]), list(summary.METRIC_KEYS))
        self.assertEqual(list(doc["metric_metadata"]), list(summary.METRIC_KEYS))
        self.assertTrue(doc["metric_metadata"]["outcomes.task_success_rate"]["higher_is_better"])
        self.assertEqual(doc["metric_metadata"]["api.latency.p95"],
                         {"unit": "ms", "higher_is_better": False})

    def test_metric_values_and_counts(self):
        doc = document()
        expected = {
            "outcomes.task_success_rate": 0.5,
            "outcomes.abnormal_termination_rate": 0.5,
            "tools.edit.failure_rate": 0.5,
            "tools.edit.no_match_rate": 0.5,
            "tokens.cache_hit_rate": 0.8,
            "api.latency.p95": 300,
            "api.failure_rate": 0.5,
            "judge.timeout_rate": 1 / 3,
            "judge.failure_rate": 2 / 3,
            "judge.block_rate": 1 / 3,
        }
        for key, value in expected.items():
            self.assertAlmostEqual(doc["metrics"][key], value, msg=key)
        self.assertEqual(doc["counts"], {
            "sessions": 1, "invocations": 2, "invocations_started": 2,
            "tasks": 2, "tasks_succeeded": 1, "tool_calls": 2, "edit_calls": 2,
            "api_attempts": 2, "judge_attempts": 3, "judge_verdicts": 3,
            "session_parse_errors": 0, "telemetry_parse_errors": 0,
        })

    def test_empty_window_keeps_shape_and_nulls_every_metric(self):
        doc = document(dataset())
        self.assertEqual(set(doc["metrics"]), set(summary.METRIC_KEYS))
        self.assertTrue(all(value is None for value in doc["metrics"].values()))
        self.assertEqual(set(doc["counts"].values()), {0})

    def test_half_open_bounds_and_all_time(self):
        session = cakelib.Session("s1", Path("/sessions/s1.jsonl"), 0, NOW, [
            *task("at-start", START.isoformat(), "success"),
            *task("at-end", NOW.isoformat(), "success")])
        invocation = cakelib.Invocation("s1", "i1")
        invocation.init = {"model": "alpha", "timestamp": START.isoformat()}
        invocation.attempts = [{"timestamp": START.isoformat(), "status_code": 200},
                               {"timestamp": NOW.isoformat(), "status_code": 500}]
        doc = document(dataset([session], [invocation]))
        self.assertEqual((doc["counts"]["tasks"], doc["counts"]["api_attempts"]), (1, 1))
        self.assertEqual(doc["metrics"]["api.failure_rate"], 0)
        self.assertIsNone(document(start=None)["window"]["start"])


class CompareTest(unittest.TestCase):
    def rows(self, metrics):
        baseline = {"schema_version": summary.SCHEMA_VERSION, "metrics": metrics}
        return {row["key"]: row for row in summary.compare(document(), baseline)}

    def test_missing_keys_are_null_not_zero(self):
        missing = self.rows({"outcomes.task_success_rate": 0.4})["api.failure_rate"]
        self.assertEqual((missing["baseline"], missing["delta"], missing["direction"]),
                         (None, None, "n/a"))
        self.assertEqual(len(summary.missing_keys({"metrics": {"outcomes.task_success_rate": 0.4}})),
                         len(summary.METRIC_KEYS) - 1)

    def test_direction_follows_higher_is_better(self):
        rows = self.rows({"outcomes.task_success_rate": 0.4, "api.latency.p95": 100})
        self.assertEqual(rows["outcomes.task_success_rate"]["direction"], "better")
        self.assertAlmostEqual(rows["outcomes.task_success_rate"]["delta"], 0.1)
        self.assertEqual(rows["api.latency.p95"]["direction"], "worse")
        self.assertEqual(rows["api.latency.p95"]["delta"], 200)
        rows = self.rows({"outcomes.task_success_rate": 0.5, "api.latency.p95": None})
        self.assertEqual(rows["outcomes.task_success_rate"]["direction"], "unchanged")
        self.assertEqual(rows["api.latency.p95"]["direction"], "n/a")
        self.assertEqual(len(summary.missing_keys({})), len(summary.METRIC_KEYS))
        self.assertTrue(all(row["baseline"] is None for row in summary.compare(document(), {})))


class DashboardAgreementTest(unittest.TestCase):
    def test_overlapping_metrics_match_dashboard_aggregate(self):
        rows = dashboard.aggregate(DATA, START, NOW)
        metrics = document()["metrics"]
        tasks = sum(row["tasks"] for row in rows)
        attempts = sum(row["attempts"] for row in rows)
        self.assertAlmostEqual(metrics["outcomes.task_success_rate"],
                               sum(row["success"] for row in rows) / tasks)
        self.assertAlmostEqual(metrics["api.failure_rate"],
                               sum(row["api_errors"] for row in rows) / attempts)
        self.assertAlmostEqual(metrics["tokens.cache_hit_rate"],
                               sum(row["cached_tokens"] for row in rows)
                               / sum(row["cache_reported_input"] for row in rows))


class CliTest(unittest.TestCase):
    def parse(self, *args):
        with patch.object(sys, "argv", ["report.py", *args]), \
                contextlib.redirect_stderr(io.StringIO()):
            return report.parse_args()

    def test_defaults_and_flags(self):
        ns = self.parse()
        self.assertEqual((ns.days, ns.json, ns.baseline, ns.vs), (30, False, None, None))
        self.assertTrue(self.parse("--json", "--days", "7").json)
        self.assertEqual(self.parse("--baseline", "/tmp/b.json").baseline, Path("/tmp/b.json"))
        self.assertEqual(self.parse("--vs", "7").vs, 7)

    def test_invalid_combinations_exit(self):
        for args in (("--vs", "0"), ("--json", "--vs", "7"),
                     ("--json", "--baseline", "/tmp/b.json"),
                     ("--baseline", "/tmp/b.json", "--vs", "7")):
            with self.subTest(args=args), self.assertRaises(SystemExit):
                self.parse(*args)

    def test_json_and_baseline_cli(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name in ("sessions", "telemetry"):
                (root / name).mkdir()
            source = ["--sessions-dir", str(root / "sessions"),
                      "--telemetry-dir", str(root / "telemetry")]
            output = io.StringIO()
            with patch.object(sys, "argv", ["report.py", "--json", "--days", "7", *source]), \
                    contextlib.redirect_stdout(output):
                report.main()
            doc = json.loads(output.getvalue())
            self.assertEqual(doc["schema_version"], summary.SCHEMA_VERSION)
            self.assertIsNotNone(doc["window"]["start"])
            self.assertEqual(set(doc["metrics"]), set(summary.METRIC_KEYS))
            self.assertTrue(all(value is None for value in doc["metrics"].values()))
            self.assertEqual(doc["counts"]["sessions"], 0)

            del doc["metrics"]["judge.block_rate"]
            path = root / "baseline.json"
            path.write_text(json.dumps(doc))
            output = io.StringIO()
            with patch.object(sys, "argv", [
                "report.py", "--baseline", str(path), "--days", "0", *source,
            ]), contextlib.redirect_stdout(output):
                report.main()
        text = output.getvalue()
        self.assertIn("Baseline comparison", text)
        self.assertTrue(all(key in text for key in summary.METRIC_KEYS))
        self.assertIn("lacks 1 documented key(s): judge.block_rate", text)

    def test_invocation_json_scopes_transcript_metrics_through_loader(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sessions, telemetry = root / "sessions", root / "telemetry"
            sessions.mkdir()
            telemetry.mkdir()
            records = [
                {"type": "session_meta", "model": "alpha"},
                *task("good-task", T(days=-1), "success"),
                *task("bad-task", T(days=-1), "error"),
                *task("unattributed-task", T(days=-1), "error"),
                *edit("good-call", T(days=-1), "ok"),
                *edit("bad-call", T(days=-1), "Error: could not find the exact text to replace"),
                *edit("unattributed-call", T(days=-1), "Error: unknown"),
            ]
            (sessions / "s1.jsonl").write_text("\n".join(map(json.dumps, records)))
            events = []
            for invocation, task_id, call_id in (
                ("good", "good-task", "good-call"), ("bad", "bad-task", "bad-call"),
            ):
                common = {"session_id": "s1", "invocation_id": invocation,
                          "timestamp": T(days=-1)}
                events.extend([
                    {**common, "type": "telemetry_init", "model": "alpha"},
                    {**common, "type": "api_attempt_in_flight", "task_id": task_id},
                    {**common, "type": "tool_call", "name": "Edit",
                     "call_id": cakelib.call_id_digest(call_id)},
                    {**common, "type": "session_summary"},
                ])
            path = telemetry / "s1.ndjson"
            path.write_text("\n".join(map(json.dumps, events)))

            def run(*filters):
                output = io.StringIO()
                with patch.object(sys, "argv", [
                    "report.py", "--json", "--days", "0", "--sessions-dir", str(sessions),
                    "--telemetry-dir", str(telemetry), *filters,
                ]), patch.object(report, "_now", return_value=NOW), \
                        contextlib.redirect_stdout(output):
                    report.main()
                return json.loads(output.getvalue())

            for invocation, success, failure in (("good", 1, 0), ("bad", 0, 1)):
                with self.subTest(invocation=invocation):
                    doc = run("--invocation", invocation)
                    self.assertEqual(doc["counts"]["invocations"], 1)
                    self.assertEqual(doc["counts"]["tasks"], 1)
                    self.assertEqual(doc["counts"]["tool_calls"], 1)
                    self.assertEqual(doc["counts"]["edit_calls"], 1)
                    self.assertEqual(doc["metrics"]["outcomes.task_success_rate"], success)
                    self.assertEqual(doc["metrics"]["tools.edit.failure_rate"], failure)
                    self.assertEqual(doc["metrics"]["tools.edit.no_match_rate"], failure)
            self.assertEqual(run()["counts"]["tasks"], 3)
            self.assertEqual(run("--session", "s1")["counts"]["tool_calls"], 3)
            self.assertEqual(run("--invocation", "missing")["counts"]["tasks"], 0)

            # Historical/incomplete telemetry must not claim unrelated records.
            events = [event for event in events if event["type"] != "api_attempt_in_flight"]
            for event in events:
                event.pop("call_id", None)
            path.write_text("\n".join(map(json.dumps, events)))
            doc = run("--invocation", "good")
            self.assertEqual(doc["counts"]["tasks"], 0)
            self.assertEqual(doc["counts"]["tool_calls"], 0)
            self.assertIsNone(doc["metrics"]["outcomes.task_success_rate"])
            self.assertIsNone(doc["metrics"]["tools.edit.failure_rate"])

    def test_vs_uses_one_uncapped_load(self):
        output = io.StringIO()
        with patch.object(cakelib, "load", return_value=dataset()) as load, \
                contextlib.redirect_stdout(output):
            report.vs_report(self.parse("--vs", "7"))
        self.assertEqual((load.call_count, load.call_args.args[0].days), (1, 0))
        self.assertIn("Window comparison: last 7 days vs preceding 7 days", output.getvalue())
        self.assertIn("previous", output.getvalue())


if __name__ == "__main__":
    unittest.main()
