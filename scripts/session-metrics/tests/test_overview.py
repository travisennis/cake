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
from collections import Counter
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


def report_section(output: str, label: str) -> str:
    return output.split(f"{label} (telemetry):", 1)[1].split("\n\n", 1)[0]


class TelemetrySurfaceTest(unittest.TestCase):
    def test_reasoning_effort_distribution_reported(self):
        alpha = invocation("s1", "i1")
        alpha.init["settings"] = {"reasoning_effort": "high"}
        beta = invocation("s1", "i2")
        beta.init["settings"] = {"reasoning_effort": "low"}
        output = run_overview([session("s1")], [alpha, beta])

        self.assertIn("Reasoning effort (telemetry)", output)
        self.assertEqual(
            overview._setting_distribution([alpha, beta], "reasoning_effort"),
            Counter({"high": 1, "low": 1}),
        )

    def test_token_budget_distributions_report_exact_values_and_counts(self):
        first = invocation("s1", "i1")
        first.init["settings"] = {
            "max_output_tokens": 4096,
            "reasoning_max_tokens": 2048,
        }
        second = invocation("s1", "i2")
        second.init["settings"] = {
            "max_output_tokens": 4096,
            "reasoning_max_tokens": 1024,
        }
        third = invocation("s1", "i3")
        third.init["settings"] = {"max_output_tokens": 2048}
        fourth = invocation("s1", "i4")
        invocations = [first, second, third, fourth]
        output = run_overview([session("s1")], invocations)

        self.assertIn("Max output tokens (telemetry)", output)
        self.assertIn("Reasoning max tokens (telemetry)", output)
        max_output_section = report_section(output, "Max output tokens")
        reasoning_max_section = report_section(output, "Reasoning max tokens")
        for value, count in ((4096, 2), (2048, 1), ("unset", 1)):
            self.assertRegex(max_output_section, rf"(?m)^\s+{value}\s+{count}\s*$")
        for value, count in ((2048, 1), (1024, 1), ("unset", 2)):
            self.assertRegex(reasoning_max_section, rf"(?m)^\s+{value}\s+{count}\s*$")
        self.assertEqual(
            overview._setting_distribution(invocations, "max_output_tokens"),
            Counter({4096: 2, 2048: 1, "unset": 1}),
        )
        self.assertEqual(
            overview._setting_distribution(invocations, "reasoning_max_tokens"),
            Counter({2048: 1, 1024: 1, "unset": 2}),
        )

    def test_unset_settings_report_safe_value(self):
        inv = invocation("s1", "i1")
        output = run_overview([session("s1")], [inv])

        for field, label in overview.TELEMETRY_SETTINGS:
            self.assertIn(f"{label} (telemetry)", output)
            self.assertRegex(
                report_section(output, label),
                r"(?m)^\s+unset\s+1\s*$",
            )
            self.assertEqual(
                overview._setting_distribution([inv], field),
                Counter({"unset": 1}),
            )

    def test_tools_present_counts_each_tool_once_per_invocation(self):
        first = invocation("s1", "i1")
        first.init["tools"] = ["Bash", "Read", "Bash"]
        second = invocation("s1", "i2")
        second.init["tools"] = ["Bash", "Write", "Read"]
        invocations = [first, second]
        output = run_overview([session("s1")], invocations)

        self.assertIn("Tools present (telemetry)", output)
        tools_section = report_section(output, "Tools present")
        for tool, count in (("Bash", 2), ("Read", 2), ("Write", 1)):
            self.assertRegex(tools_section, rf"(?m)^\s+{tool}\s+{count}\s*$")
        self.assertEqual(
            overview._tools_present_distribution(invocations),
            Counter({"Bash": 2, "Read": 2, "Write": 1}),
        )


if __name__ == "__main__":
    unittest.main()