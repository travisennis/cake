#!/usr/bin/env python3
"""Unit tests for the telemetry surface sections in overview.py.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import contextlib
import io
import os
import sys
import unittest
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import overview


def session(session_id: str) -> cakelib.Session:
    return cakelib.Session(
        id=session_id,
        path=Path(f"/sessions/{session_id}.jsonl"),
        size=100,
        mtime=datetime.now(timezone.utc),
        records=[],
    )


def invocation(session_id: str, invocation_id: str) -> cakelib.Invocation:
    inv = cakelib.Invocation(session_id, invocation_id)
    inv.init = {"model": "alpha", "working_directory": "/proj"}
    return inv


def run_overview(sessions: list[cakelib.Session], invocations: list[cakelib.Invocation]) -> str:
    data = cakelib.Dataset(
        sessions=sessions,
        invocations=invocations,
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        overview.run(data)
    return buf.getvalue()


class TelemetrySurfaceTest(unittest.TestCase):
    def test_reasoning_effort_distribution_reported(self):
        alpha = invocation("s1", "i1")
        alpha.init["settings"] = {"reasoning_effort": "high"}
        beta = invocation("s1", "i2")
        beta.init["settings"] = {"reasoning_effort": "low"}
        output = run_overview([session("s1")], [alpha, beta])

        self.assertIn("Reasoning effort (telemetry)", output)
        self.assertIn("high", output)
        self.assertIn("low", output)

    def test_missing_settings_reports_unknown(self):
        inv = invocation("s1", "i1")
        output = run_overview([session("s1")], [inv])

        self.assertIn("Reasoning effort (telemetry)", output)
        self.assertIn("unknown", output)

    def test_tools_present_table_reported(self):
        inv = invocation("s1", "i1")
        inv.init["tools"] = ["Bash", "Read", "Bash"]
        output = run_overview([session("s1")], [inv])

        self.assertIn("Tools present (telemetry)", output)
        self.assertIn("Bash", output)
        self.assertIn("Read", output)


if __name__ == "__main__":
    unittest.main()