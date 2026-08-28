#!/usr/bin/env python3
"""Focused tests for scripts/profile-agent-loop.py."""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "profile-agent-loop.py"
SPEC = importlib.util.spec_from_file_location("profile_agent_loop", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"could not load {SCRIPT}")
PROFILE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PROFILE
SPEC.loader.exec_module(PROFILE)


class ProfileAgentLoopTests(unittest.TestCase):
    def test_parser_uses_amplified_default_batch(self) -> None:
        arguments = PROFILE.parser().parse_args([])

        self.assertEqual(arguments.tool_calls, 5_000)

    def test_parser_marks_instruments_as_optional(self) -> None:
        self.assertIn("optional macOS", PROFILE.parser().format_help())

    def test_positive_int_rejects_zero(self) -> None:
        with self.assertRaises(argparse.ArgumentTypeError):
            PROFILE.positive_int("0")

    def test_function_calls_are_unique_and_read_the_fixture(self) -> None:
        fixture = Path("/tmp/profile-fixture.txt")

        calls = PROFILE.function_calls(fixture, 3)

        self.assertEqual(
            [call["call_id"] for call in calls],
            ["profile-call-1", "profile-call-2", "profile-call-3"],
        )
        self.assertTrue(all(call["name"] == "Read" for call in calls))
        arguments = json.loads(calls[0]["arguments"])
        self.assertEqual(arguments["path"], str(fixture))
        self.assertEqual(arguments["start_line"], 1)
        self.assertEqual(arguments["end_line"], 32)

    def test_tool_output_validation_accepts_complete_batch(self) -> None:
        output = "\n".join(PROFILE.READ_OUTPUT_MARKERS)
        items = [
            {
                "type": "function_call_output",
                "call_id": f"profile-call-{number}",
                "output": output,
            }
            for number in range(1, 4)
        ]

        error = PROFILE.tool_output_error(
            items, {"profile-call-1", "profile-call-2", "profile-call-3"}
        )

        self.assertIsNone(error)

    def test_tool_output_validation_rejects_missing_call(self) -> None:
        output = "\n".join(PROFILE.READ_OUTPUT_MARKERS)
        items = [
            {
                "type": "function_call_output",
                "call_id": "profile-call-1",
                "output": output,
            }
        ]

        error = PROFILE.tool_output_error(
            items, {"profile-call-1", "profile-call-2"}
        )

        self.assertIn("missing 1", error)

    def test_tool_output_validation_rejects_failed_read(self) -> None:
        items = [
            {
                "type": "function_call_output",
                "call_id": "profile-call-1",
                "output": "Path not found",
            }
        ]

        error = PROFILE.tool_output_error(items, {"profile-call-1"})

        self.assertEqual(
            error, "profile-call-1 did not return the complete profiling fixture"
        )


if __name__ == "__main__":
    unittest.main()
