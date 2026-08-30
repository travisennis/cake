#!/usr/bin/env python3
"""Unit tests for the API attempt/retry aggregations in api.py.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import contextlib
import io
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import api
import cakelib


def make_dataset(invocations: list[cakelib.Invocation]) -> cakelib.Dataset:
    return cakelib.Dataset(
        sessions=[],
        invocations=invocations,
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )


def invocation(session_id: str, invocation_id: str, model: str = "alpha") -> cakelib.Invocation:
    inv = cakelib.Invocation(session_id, invocation_id)
    inv.init = {"model": model, "working_directory": "/proj"}
    return inv


class TerminationDistributionTest(unittest.TestCase):
    def test_counts_termination_classifications(self):
        inv = invocation("s1", "i1")
        inv.attempts = [
            {"status_code": 200, "termination": {"classification": "completed"}},
            {"status_code": 200, "termination": {"classification": "token_limit"}},
            {"status_code": 200, "termination": {"classification": "content_filter"}},
            {"status_code": 400, "termination": {"classification": "failed"}},
        ]
        agg = api.aggregate(make_dataset([inv]))

        self.assertEqual(agg["terminations"]["completed"], 1)
        self.assertEqual(agg["terminations"]["token_limit"], 1)
        self.assertEqual(agg["terminations"]["content_filter"], 1)
        self.assertEqual(agg["terminations"]["failed"], 1)
        self.assertEqual(agg["terminations"]["unknown"], 0)

    def test_missing_termination_defaults_to_unknown(self):
        # Legacy records predate the termination field; classification is the
        # sanitized vocabulary, so a missing field reports as `unknown`.
        inv = invocation("s1", "i1")
        inv.attempts = [{"status_code": 200}]
        agg = api.aggregate(make_dataset([inv]))

        self.assertEqual(agg["terminations"]["unknown"], 1)

    def test_report_contains_termination_section(self):
        inv = invocation("s1", "i1")
        inv.attempts = [{"status_code": 200, "request_ms": 100,
                         "termination": {"classification": "token_limit"}}]

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            api.run(make_dataset([inv]))
        output = buffer.getvalue()

        self.assertIn("Termination classification", output)
        self.assertIn("token_limit", output)


class RetryAggregateTest(unittest.TestCase):
    def test_max_retries_and_overrides_changed(self):
        inv = invocation("s1", "i1")
        inv.retries = [
            {"reason": "context_overflow", "attempt": 1, "max_retries": 3,
             "delay_ms": 500, "changed_request_overrides": True},
            {"reason": "rate_limit", "attempt": 1, "max_retries": 3,
             "delay_ms": 1000, "changed_request_overrides": False},
        ]
        inv.attempts = [{"status_code": 429, "error": "rate"}]

        agg = api.aggregate(make_dataset([inv]))
        self.assertEqual(len(agg["retry_reasons"]), 2)
        for row in agg["retry_reasons"]:
            self.assertEqual(row["max_retries"], 3)
        changed = [row for row in agg["retry_reasons"] if row["changed_overrides"]]
        self.assertEqual(len(changed), 1)
        self.assertEqual(changed[0]["reason"], "context_overflow")

    def test_report_retry_columns(self):
        inv = invocation("s1", "i1")
        inv.retries = [
            {"reason": "context_overflow", "attempt": 2, "max_retries": 3,
             "delay_ms": 500, "changed_request_overrides": True},
        ]
        inv.attempts = [{"status_code": 200, "request_ms": 100}]

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            api.run(make_dataset([inv]))
        output = buffer.getvalue()

        self.assertIn("max retries", output)
        self.assertIn("changed overrides", output)
        self.assertIn("context_overflow", output)


if __name__ == "__main__":
    unittest.main()