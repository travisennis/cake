#!/usr/bin/env python3
"""Regression tests for honest wall-time reporting in time_breakdown.py.

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
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import time_breakdown


BASE = "2026-09-22T00:00:"


def stamp(milliseconds: int) -> str:
    seconds, remainder = divmod(milliseconds, 1000)
    return f"{BASE}{seconds:02d}.{remainder:03d}Z"


def invocation(
    *,
    wall_ms: int | None,
    tool_durations: list[int] | None = None,
    retry_delays: list[int] | None = None,
) -> cakelib.Invocation:
    inv = cakelib.Invocation("session-1", "invocation-1")
    inv.init = {"model": "test-model", "working_directory": "/project"}
    if wall_ms is not None:
        inv.summary = {"duration_ms": wall_ms, "turn_count": 1}
    inv.tool_calls = [
        {
            "name": f"Tool{index}",
            "duration_ms": duration,
            "output_bytes": 0,
            "session_id": inv.session_id,
        }
        for index, duration in enumerate(tool_durations or [], start=1)
    ]
    inv.retries = [
        {"reason": "rate_limit", "delay_ms": delay}
        for delay in retry_delays or []
    ]
    return inv


def report(invocations: list[cakelib.Invocation]) -> str:
    data = cakelib.Dataset(
        sessions=[],
        invocations=invocations,
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        time_breakdown.run(data)
    return output.getvalue()


class TimeBreakdownTest(unittest.TestCase):
    def test_loader_selects_one_invocation_and_reads_retry_wait(self):
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            telemetry = Path(directory) / "telemetry"
            sessions.mkdir()
            telemetry.mkdir()
            path = telemetry / "session-1.ndjson"
            records = [
                {"type": "telemetry_init", "session_id": "session-1",
                 "invocation_id": "invocation-1", "timestamp": stamp(0),
                 "model": "test-model", "working_directory": "/project"},
                {"type": "retry_wait", "session_id": "session-1",
                 "invocation_id": "invocation-1", "timestamp": stamp(200),
                 "started_at": stamp(100), "completed_at": stamp(200),
                 "duration_ms": 100},
                {"type": "session_summary", "session_id": "session-1",
                 "invocation_id": "invocation-1", "timestamp": stamp(1000),
                 "duration_ms": 1000, "turn_count": 1},
                {"type": "telemetry_init", "session_id": "session-2",
                 "invocation_id": "invocation-2", "timestamp": stamp(0),
                 "model": "test-model", "working_directory": "/project"},
            ]
            path.write_text("\n".join(json.dumps(record) for record in records) + "\n")
            args = cakelib.build_arg_parser("test").parse_args([
                "--days", "0", "--sessions-dir", str(sessions),
                "--telemetry-dir", str(telemetry), "--session", "session-1",
                "--invocation", "invocation-1",
            ])

            data = cakelib.load(args)

            self.assertTrue(data.selected_invocation)
            self.assertEqual(len(data.invocations), 1)
            totals, missing = time_breakdown.invocation_timeline(data.invocations[0])
            self.assertEqual(missing, {})
            self.assertEqual(round(totals["retry wait"]), 100)

    def test_selected_timeline_assigns_parallel_work_once(self):
        inv = invocation(wall_ms=1000)
        inv.summary["timestamp"] = stamp(1000)
        inv.other = [{
            "type": "api_attempt_in_flight", "turn_index": 1, "attempt": 1,
            "started_at": stamp(0),
        }]
        inv.attempts = [{
            "turn_index": 1, "attempt": 1, "timestamp": stamp(300),
            "total_ms": 300,
        }]
        inv.retry_waits = [{"started_at": stamp(300), "completed_at": stamp(400)}]
        inv.tool_calls = [
            {"name": "Bash", "call_id": "call-1", "started_at": stamp(400),
             "completed_at": stamp(700)},
            {"name": "Read", "started_at": stamp(700), "completed_at": stamp(950)},
            {"name": "Edit", "started_at": stamp(750), "completed_at": stamp(900)},
        ]
        inv.judge_attempts = [{
            "call_id": cakelib.call_id_digest("call-1"), "total_ms": 80,
        }]

        totals, missing = time_breakdown.invocation_timeline(inv)

        self.assertEqual(missing, {})
        self.assertEqual({name: round(value) for name, value in totals.items()}, {
            "main provider": 300, "retry wait": 100,
            "delegated agent": 0, "Bash safety models": 80,
            "Bash remaining": 220, "other tools": 100,
            "parallel activity": 150, "unobserved": 50,
        })

    def test_old_tool_record_is_unknown_instead_of_counted_as_zero(self):
        inv = invocation(wall_ms=1000, tool_durations=[800])
        inv.summary["timestamp"] = stamp(1000)
        totals, missing = time_breakdown.invocation_timeline(inv)

        self.assertEqual(round(totals["unobserved"]), 1000)
        self.assertEqual(missing, {"tool calls": 1})

    def test_subagent_wait_has_its_own_category(self):
        inv = invocation(wall_ms=1000)
        inv.summary["timestamp"] = stamp(1000)
        inv.tool_calls = [{
            "name": "tb__subagent", "started_at": stamp(100),
            "completed_at": stamp(700),
        }]
        totals, _ = time_breakdown.invocation_timeline(inv)

        self.assertEqual(round(totals["delegated agent"]), 600)
        self.assertEqual(round(totals["unobserved"]), 400)

    def test_concurrent_tool_work_is_not_an_exclusive_wall_share(self):
        output = report([invocation(wall_ms=1000, tool_durations=[800, 800])])

        self.assertIn("tool execution (cumulative work)", output)
        self.assertRegex(output, r"tool execution \(cumulative work\)\s+1\.6s\s+may overlap")
        self.assertNotIn("160.0%", output)
        self.assertNotIn("other (streaming", output)

    def test_sequential_tool_work_keeps_useful_cumulative_context(self):
        output = report([invocation(wall_ms=1000, tool_durations=[300, 400])])

        self.assertRegex(output, r"tool execution \(cumulative work\)\s+700ms\s+may overlap")
        self.assertIn("share of cumulative tool work", output)
        self.assertIn("42.9%", output)
        self.assertIn("57.1%", output)

    def test_scheduled_retry_delay_is_not_reported_as_elapsed_time(self):
        output = report([invocation(wall_ms=1000, retry_delays=[500])])

        self.assertRegex(output, r"scheduled retry delay\s+500ms\s+actual wait not measured")
        self.assertNotIn("50.0%", output)
        self.assertIn("actual wait not measured", output)

    def test_interrupted_invocation_has_no_precise_wall_attribution(self):
        inv = invocation(wall_ms=None, tool_durations=[800])
        inv.attempts = [{"total_ms": 900, "parse_ms": 25}]
        output = report([inv])

        self.assertIn("No complete invocation wall time is available", output)
        self.assertIn("1 incomplete invocation has no session_summary", output)
        self.assertIn("only their wall time and turn pacing are unavailable", output)
        self.assertRegex(output, r"tool execution \(cumulative work\)\s+800ms\s+may overlap")
        self.assertRegex(output, r"Slowest API attempts:[\s\S]+test-model\s+900ms")


if __name__ == "__main__":
    unittest.main()
