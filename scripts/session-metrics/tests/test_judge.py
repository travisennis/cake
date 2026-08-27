#!/usr/bin/env python3
"""Unit tests for the LLM-judge reliability report (`judge.py`).

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import contextlib
import io
import json
import os
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import judge


def make_dataset(invocations: list[cakelib.Invocation]) -> cakelib.Dataset:
    return cakelib.Dataset(
        sessions=[],
        invocations=invocations,
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )


def invocation(session_id: str, invocation_id: str, model: str) -> cakelib.Invocation:
    inv = cakelib.Invocation(session_id, invocation_id)
    inv.init = {"model": model, "working_directory": "/proj"}
    return inv


class AggregateTest(unittest.TestCase):
    def test_counts_terminal_classes_and_latency_per_model(self):
        alpha = invocation("s1", "i1", "alpha")
        alpha.judge_attempts = [
            {"terminal_class": "verdict", "total_ms": 1000,
             "request_build_ms": 1, "request_ms": 950,
             "response_parse_ms": 30, "verdict_parse_ms": 19,
             "configured_timeout_ms": 30000, "status_code": 200,
             "usage": {"input_tokens": 100, "output_tokens": 20, "total_tokens": 120}},
            {"terminal_class": "timeout", "total_ms": 30000,
             "configured_timeout_ms": 30000, "status_code": None},
            {"terminal_class": "malformed_verdict", "total_ms": 500,
             "configured_timeout_ms": 30000, "status_code": 200},
        ]
        beta = invocation("s2", "i2", "beta")
        beta.judge_attempts = [
            {"terminal_class": "verdict", "total_ms": 200,
             "configured_timeout_ms": 30000, "status_code": 200},
        ]

        by_model = judge.aggregate(make_dataset([alpha, beta]))

        self.assertEqual(set(by_model), {"alpha", "beta"})
        alpha_stats = by_model["alpha"]
        self.assertEqual(alpha_stats["terminal"]["verdict"], 1)
        self.assertEqual(alpha_stats["terminal"]["timeout"], 1)
        self.assertEqual(alpha_stats["terminal"]["malformed_verdict"], 1)
        self.assertEqual(sorted(alpha_stats["latency"]), [500, 1000, 30000])
        # Phase lists split request vs verdict phases.
        self.assertEqual(alpha_stats["phases"]["request_build_ms"], [1])
        self.assertEqual(alpha_stats["phases"]["request_ms"], [950])
        self.assertEqual(alpha_stats["phases"]["response_parse_ms"], [30])
        self.assertEqual(alpha_stats["phases"]["verdict_parse_ms"], [19])
        # Only the timeout attempt reached its configured timeout.
        self.assertEqual(alpha_stats["near_timeout"], 1)
        self.assertEqual(alpha_stats["status"][200], 2)
        self.assertEqual(alpha_stats["status"][None], 1)
        self.assertEqual(len(alpha_stats["usage"]), 1)
        self.assertEqual(by_model["beta"]["terminal"]["verdict"], 1)


class RetryAggregationTest(unittest.TestCase):
    def test_retry_era_two_attempt_evaluation(self):
        alpha = invocation("s1", "i1", "alpha")
        alpha.judge_attempts = [
            {
                "attempt": 1, "retry_ordinal": 0, "retry_delay_ms": 0,
                "effective_deadline_ms": 45000, "terminal_class": "timeout",
                "total_ms": 30000, "configured_timeout_ms": 30000,
            },
            {
                "attempt": 2, "retry_ordinal": 1, "retry_reason": "request_timeout",
                "retry_delay_ms": 512, "effective_deadline_ms": 45000,
                "terminal_class": "verdict", "total_ms": 1420,
                "configured_timeout_ms": 30000,
            },
        ]

        by_model = judge.aggregate(make_dataset([alpha]))
        stats = by_model["alpha"]
        self.assertEqual(len(stats["retry_attempts"]), 1)
        self.assertEqual(stats["retry_reasons"]["request_timeout"], 1)
        self.assertEqual(stats["retry_delays"], [512])
        self.assertAlmostEqual(stats["deadline_ratio"][0], 30000 / 45000)
        self.assertAlmostEqual(stats["deadline_ratio"][1], 1420 / 45000)
        # Retry ordinal 0 attempt is not counted as a retry.
        self.assertEqual(stats["terminal"]["timeout"], 1)
        self.assertEqual(stats["terminal"]["verdict"], 1)


class RunTest(unittest.TestCase):
    def test_reports_per_model_numbers(self):
        alpha = invocation("s1", "i1", "alpha")
        alpha.judge_attempts = [
            {"terminal_class": "verdict", "total_ms": 1000},
            {"terminal_class": "timeout", "total_ms": 30000},
        ]
        beta = invocation("s2", "i2", "beta")
        beta.judge_attempts = [
            {"terminal_class": "verdict", "total_ms": 200},
            {"terminal_class": "malformed_verdict", "total_ms": 500},
        ]

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            judge.run(make_dataset([alpha, beta]))
        output = buf.getvalue()

        self.assertIn("JUDGE RELIABILITY", output)
        self.assertIn("Attempts: 4", output)
        self.assertIn("timeouts: 1", output)
        self.assertIn("alpha", output)
        self.assertIn("beta", output)
        self.assertIn("verdict", output)
        self.assertIn("malformed_verdict", output)

    def test_metadata_only_no_raw_content(self):
        alpha = invocation("s1", "i1", "alpha")
        alpha.judge_attempts = [
            {"terminal_class": "verdict", "total_ms": 1000},
            {"terminal_class": "timeout", "total_ms": 30000},
        ]

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            judge.run(make_dataset([alpha]))
        output = buf.getvalue()

        # Model identifiers are metadata; the working directory, commands,
        # reason text, prompts, and response text must never appear.
        self.assertIn("alpha", output)
        self.assertNotIn("/proj", output)
        self.assertNotIn("rm -rf", output)

    def test_empty_dataset_prints_no_data(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            judge.run(make_dataset([]))
        output = buf.getvalue()
        self.assertIn("No telemetry judge_attempt records in window.", output)


class LoadAndRunIntegrationTest(unittest.TestCase):
    def test_retry_era_sidecar_loads_and_reports(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "retry.ndjson")
            records = [
                {
                    "type": "telemetry_init", "session_id": "s1",
                    "invocation_id": "i1", "model": "deepseek",
                    "working_directory": "/proj",
                },
                {
                    "type": "judge_attempt", "session_id": "s1",
                    "invocation_id": "i1", "attempt": 1, "retry_ordinal": 0,
                    "retry_delay_ms": 0, "effective_deadline_ms": 45000,
                    "terminal_class": "timeout", "total_ms": 30000,
                    "configured_timeout_ms": 30000,
                },
                {
                    "type": "judge_attempt", "session_id": "s1",
                    "invocation_id": "i1", "attempt": 2, "retry_ordinal": 1,
                    "retry_reason": "request_timeout", "retry_delay_ms": 512,
                    "effective_deadline_ms": 45000, "terminal_class": "verdict",
                    "total_ms": 1420, "configured_timeout_ms": 30000,
                },
            ]
            with open(path, "w", encoding="utf-8") as fh:
                for rec in records:
                    fh.write(json.dumps(rec) + "\n")

            invocations, errors = cakelib.load_telemetry(pathlib.Path(directory), None)
            self.assertEqual(errors, 0)
            self.assertEqual(len(invocations), 1)
            self.assertEqual(len(invocations[0].judge_attempts), 2)

            data = make_dataset(invocations)
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                judge.run(data)
            output = buf.getvalue()

            self.assertIn("timeouts: 1", output)
            self.assertIn("request_timeout", output)
            # The retry-era attempt reached its configured timeout.
            self.assertIn("reached their configured timeout: 1", output)


if __name__ == "__main__":
    unittest.main()