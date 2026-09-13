#!/usr/bin/env python3
"""Unit tests for ToolCall.ok, including hook denials (issue #338).

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


def records_for(outputs: list[str]) -> list[dict]:
    records: list[dict] = []
    for i, output in enumerate(outputs):
        call_id = f"call-{i}"
        records.append({
            "type": "function_call",
            "call_id": call_id,
            "name": "Bash",
            "arguments": json.dumps({"command": "fd ."}),
        })
        records.append({
            "type": "function_call_output",
            "call_id": call_id,
            "output": output,
        })
    return records


def session(outputs: list[str]) -> cakelib.Session:
    return cakelib.Session(
        id="s",
        path=None,
        size=0,
        mtime=datetime.now(timezone.utc),
        records=records_for(outputs),
    )


class ToolCallOkTest(unittest.TestCase):
    def test_hook_denial_is_failure(self):
        """Hook denials are stored unprefixed, so the `Error:` gate misses them."""
        calls = cakelib.pair_tool_calls(
            records_for(["Hook blocked tool execution: use fd instead of find"])
        )
        first = calls[0].output.splitlines()[0]
        self.assertTrue(first.startswith("Hook blocked tool execution"))
        self.assertFalse(calls[0].ok)

    def test_prefixed_hook_denial_is_failure(self):
        """The prefixed variant also fails; no code path emits it today."""
        calls = cakelib.pair_tool_calls(
            records_for(["Error: Hook blocked tool execution: policy denies curl"])
        )
        self.assertFalse(calls[0].ok)

    def test_error_prefix_is_failure(self):
        calls = cakelib.pair_tool_calls(records_for(["Error: no such file"]))
        self.assertFalse(calls[0].ok)

    def test_success_is_not_failure(self):
        calls = cakelib.pair_tool_calls(records_for(["fd .\n./src\n"]))
        self.assertTrue(calls[0].ok)


class HookDenialTaxonomyTest(unittest.TestCase):
    def test_taxonomy_reports_hook_blocked(self):
        data = cakelib.Dataset(
            sessions=[session(["Hook blocked tool execution: use rg instead of grep"])],
            invocations=[],
            sessions_dir=None,
            telemetry_dir=None,
            cutoff=None,
        )
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            tools.run(data)
        output = buf.getvalue()
        taxonomy = output.split("Failure taxonomy:", 1)[1]
        self.assertIn("hook-blocked", taxonomy)


if __name__ == "__main__":
    unittest.main()
