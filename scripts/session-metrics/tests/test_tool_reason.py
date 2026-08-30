#!/usr/bin/env python3
"""Unit tests for Bash `reason` coverage in tools.bash_reason_coverage.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import contextlib
import io
import json
import os
import sys
import unittest
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import tools


def make_dataset(sessions: list[cakelib.Session]) -> cakelib.Dataset:
    return cakelib.Dataset(
        sessions=sessions,
        invocations=[],
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )


def session(model: str, calls: list[tuple[str, dict]]) -> cakelib.Session:
    """A Session whose paired tool calls come from the given (name, args) list."""
    records: list[dict] = []
    for i, (name, arguments) in enumerate(calls):
        call_id = f"call-{i}"
        records.append({
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": json.dumps(arguments),
        })
        records.append({
            "type": "function_call_output",
            "call_id": call_id,
            "output": "ok",
        })
    s = cakelib.Session(
        id="s",
        path=None,
        size=0,
        mtime=datetime.now(timezone.utc),
        records=records,
    )
    s.model = model
    return s


class BashReasonCoverageTest(unittest.TestCase):
    def test_counts_presence_per_model(self):
        alpha = session("alpha", [
            ("Bash", {"command": "rm -rf /tmp/x", "reason": "clean build dir"}),
            ("Bash", {"command": "ls"}),
            ("Edit", {"path": "f.txt"}),
        ])
        beta = session("beta", [
            ("Bash", {"command": "git push", "reason": "publish the release"}),
            ("Bash", {"command": "cargo fmt", "reason": "format"}),
            ("Bash", {"command": "cargo test", "reason": "verify the change"}),
        ])
        # alpha: 2 Bash calls, 1 with reason -> 50.0%
        # beta: 3 Bash calls, 3 with reason -> 100.0%
        rows = tools.bash_reason_coverage(make_dataset([alpha, beta]))
        self.assertEqual(
            rows,
            [
                ["beta", "3", "3", "100.0%"],
                ["alpha", "2", "1", "50.0%"],
                ["TOTAL", "5", "4", "80.0%"],
            ],
        )

    def test_unparseable_arguments_count_as_no_reason(self):
        records = [
            {"type": "function_call", "call_id": "c1", "name": "Bash",
             "arguments": "not-json"},
            {"type": "function_call_output", "call_id": "c1", "output": "ok"},
        ]
        s = cakelib.Session(id="s", path=None, size=0,
                            mtime=datetime.now(timezone.utc), records=records)
        s.model = "alpha"
        rows = tools.bash_reason_coverage(make_dataset([s]))
        self.assertEqual(rows, [["alpha", "1", "0", "0.0%"], ["TOTAL", "1", "0", "0.0%"]])

    def test_json_null_arguments_count_as_no_reason(self):
        records = [
            {"type": "function_call", "call_id": "c1", "name": "Bash",
             "arguments": "null"},
            {"type": "function_call_output", "call_id": "c1", "output": "ok"},
        ]
        s = cakelib.Session(id="s", path=None, size=0,
                            mtime=datetime.now(timezone.utc), records=records)
        s.model = "alpha"
        rows = tools.bash_reason_coverage(make_dataset([s]))
        self.assertEqual(rows, [["alpha", "1", "0", "0.0%"], ["TOTAL", "1", "0", "0.0%"]])

    def test_non_string_reason_values_count_as_absent(self):
        values = [None, 123, True, ["reason"], {"reason": "value"}]
        s = session(
            "alpha",
            [("Bash", {"command": "ls", "reason": value}) for value in values],
        )
        rows = tools.bash_reason_coverage(make_dataset([s]))
        self.assertEqual(rows, [["alpha", "5", "0", "0.0%"], ["TOTAL", "5", "0", "0.0%"]])

    def test_empty_reason_string_counts_absent(self):
        s = session("alpha", [("Bash", {"command": "ls", "reason": ""})])
        rows = tools.bash_reason_coverage(make_dataset([s]))
        self.assertEqual(rows, [["alpha", "1", "0", "0.0%"], ["TOTAL", "1", "0", "0.0%"]])


class BashReasonReportTest(unittest.TestCase):
    def test_report_lists_section_and_never_leaks_reason_or_command(self):
        secret = "destructive rm -rf /tmp/topsecret"
        reason = "clean up before final packaging"
        s = session("alpha", [("Bash", {"command": secret, "reason": reason})])
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            tools.run(make_dataset([s]))
        text = out.getvalue()
        self.assertIn("Bash `reason` coverage", text)
        # Metadata-only: reason text and the command never enter the report.
        self.assertNotIn(secret, text)
        self.assertNotIn(reason, text)
        self.assertIn("alpha", text)


if __name__ == "__main__":
    unittest.main()