#!/usr/bin/env python3
"""Unit tests for compensation telemetry loading and metrics.

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
import compensations


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


class LoadTelemetryTest(unittest.TestCase):
    def test_compensation_records_load_per_invocation(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "session.ndjson")
            with open(path, "w", encoding="utf-8") as fh:
                records = [
                    {
                        "type": "telemetry_init",
                        "session_id": "s1",
                        "invocation_id": "i1",
                        "model": "deepseek",
                        "working_directory": "/proj",
                    },
                    {
                        "type": "judge_attempt",
                        "session_id": "s1",
                        "invocation_id": "i1",
                        "attempt": 1,
                        "terminal_class": "verdict",
                        "total_ms": 42,
                    },
                    {
                        "type": "compensation",
                        "session_id": "s1",
                        "invocation_id": "i1",
                        "kind": "json_repair",
                        "detail": "Edit",
                    },
                    {
                        "type": "retry_scheduled",
                        "session_id": "s1",
                        "invocation_id": "i1",
                        "reason": "context_overflow",
                    },
                    {
                        "type": "compensation",
                        "session_id": "s1",
                        "invocation_id": "i1",
                        "kind": "output_truncation",
                        "detail": "Read",
                    },
                ]
                for record in records:
                    fh.write(json.dumps(record) + "\n")

            invocations, errors = cakelib.load_telemetry(pathlib.Path(directory), None)
            self.assertEqual(errors, 0)
            self.assertEqual(len(invocations), 1)
            inv = invocations[0]
            self.assertEqual(inv.model, "deepseek")
            self.assertEqual(
                [c["kind"] for c in inv.compensations],
                ["json_repair", "output_truncation"],
            )
            self.assertEqual(len(inv.judge_attempts), 1)
            self.assertEqual(inv.judge_attempts[0]["terminal_class"], "verdict")

    def test_old_sidecar_without_judge_attempts_remains_compatible(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "old.ndjson")
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(
                    json.dumps(
                        {
                            "type": "api_attempt",
                            "session_id": "s1",
                            "invocation_id": "i1",
                            "attempt": 1,
                        }
                    )
                    + "\n"
                )

            invocations, errors = cakelib.load_telemetry(pathlib.Path(directory), None)
            self.assertEqual(errors, 0)
            self.assertEqual(len(invocations), 1)
            self.assertEqual(len(invocations[0].attempts), 1)
            self.assertEqual(invocations[0].judge_attempts, [])

    def test_retry_era_judge_attempts_load_with_new_fields(self):
        # A bounded-recovery evaluation emits two judge_attempt records; the
        # recovery one carries retry_reason, retry_delay_ms, and
        # effective_deadline_ms. The loader must tolerate the additive fields
        # and preserve them on the invocation.
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "retry.ndjson")
            with open(path, "w", encoding="utf-8") as fh:
                records = [
                    {
                        "type": "telemetry_init",
                        "session_id": "s1",
                        "invocation_id": "i1",
                        "model": "deepseek",
                        "working_directory": "/proj",
                    },
                    {
                        "type": "judge_attempt",
                        "session_id": "s1",
                        "invocation_id": "i1",
                        "attempt": 1,
                        "retry_ordinal": 0,
                        "retry_delay_ms": 0,
                        "effective_deadline_ms": 45000,
                        "terminal_class": "timeout",
                        "total_ms": 30000,
                    },
                    {
                        "type": "judge_attempt",
                        "session_id": "s1",
                        "invocation_id": "i1",
                        "attempt": 2,
                        "retry_ordinal": 1,
                        "retry_reason": "network",
                        "retry_delay_ms": 512,
                        "effective_deadline_ms": 45000,
                        "terminal_class": "verdict",
                        "total_ms": 1420,
                    },
                ]
                for record in records:
                    fh.write(json.dumps(record) + "\n")

            invocations, errors = cakelib.load_telemetry(pathlib.Path(directory), None)
            self.assertEqual(errors, 0)
            self.assertEqual(len(invocations), 1)
            attempts = invocations[0].judge_attempts
            self.assertEqual(len(attempts), 2)
            # The first attempt omits retry_reason (serde skips it when None),
            # exactly as a real sidecar from a single-attempt evaluation does.
            self.assertEqual(attempts[0].get("retry_reason"), None)
            self.assertEqual(attempts[0]["retry_delay_ms"], 0)
            self.assertEqual(attempts[0]["effective_deadline_ms"], 45000)
            self.assertEqual(attempts[1]["attempt"], 2)
            self.assertEqual(attempts[1]["retry_ordinal"], 1)
            self.assertEqual(attempts[1]["retry_reason"], "network")
            self.assertEqual(attempts[1]["retry_delay_ms"], 512)
            self.assertEqual(attempts[1]["effective_deadline_ms"], 45000)
            self.assertEqual(attempts[1]["terminal_class"], "verdict")


class CountEventsTest(unittest.TestCase):
    def test_counts_by_model_and_detail(self):
        alpha_1 = invocation("s1", "i1", "alpha")
        alpha_1.compensations = [
            {"kind": "json_repair", "detail": "Edit"},
            {"kind": "json_repair", "detail": "Write"},
            {"kind": "same_path_serialization", "detail": "/p/f.txt"},
            {"kind": "judge_verdict", "detail": "block:rm-rf", "latency_ms": 120},
            {"kind": "judge_verdict", "detail": "block:curl", "overridden": True, "latency_ms": 60},
        ]
        alpha_1.retries = [{"reason": "context_overflow"}]

        alpha_2 = invocation("s2", "i2", "alpha")
        alpha_2.compensations = [{"kind": "json_repair", "detail": "Edit"}]

        beta = invocation("s3", "i3", "beta")
        beta.compensations = [{"kind": "edit_invalid_arguments"}]

        by_model, by_kind_detail, latencies, overflow, judge_overridden = compensations.count_events(
            make_dataset([alpha_1, alpha_2, beta])
        )

        self.assertEqual(by_model["alpha"]["json_repair"], 3)
        self.assertEqual(by_model["alpha"]["same_path_serialization"], 1)
        self.assertEqual(by_model["alpha"]["judge_verdict"], 2)
        self.assertEqual(by_model["alpha"]["edit_invalid_arguments"], 0)
        self.assertEqual(by_model["beta"]["edit_invalid_arguments"], 1)
        self.assertEqual(by_model.get("beta", {}).get("json_repair", 0), 0)

        self.assertEqual(by_kind_detail[("json_repair", "Edit")], 2)
        self.assertEqual(by_kind_detail[("json_repair", "Write")], 1)
        self.assertEqual(by_kind_detail[("same_path_serialization", "/p/f.txt")], 1)
        self.assertEqual(by_kind_detail[("edit_invalid_arguments", "-")], 1)

        self.assertEqual(sorted(latencies), [60, 120])
        self.assertEqual(overflow["alpha"], 1)
        self.assertEqual(judge_overridden["alpha"], 1)
        self.assertEqual(judge_overridden.get("beta", 0), 0)

    def test_unknown_kinds_do_not_break_aggregation(self):
        inv = invocation("s1", "i1", "alpha")
        inv.compensations = [{"kind": "future_kind"}]
        by_model, by_kind_detail, latencies, overflow, judge_overridden = compensations.count_events(
            make_dataset([inv])
        )
        self.assertEqual(by_model["alpha"]["future_kind"], 1)
        self.assertEqual(by_kind_detail[("future_kind", "-")], 1)
        self.assertEqual(latencies, [])
        self.assertEqual(dict(overflow), {})
        self.assertEqual(dict(judge_overridden), {})

    def test_flatlined_models_appear_in_report(self):
        # A model with telemetry coverage but zero compensation events is the
        # deletion-candidate signal; it must show as a zero row, not vanish.
        flatlined = invocation("s1", "i1", "alpha")
        active = invocation("s2", "i2", "beta")
        active.compensations = [{"kind": "json_repair", "detail": "Edit"}]

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            compensations.run(make_dataset([flatlined, active]))
        output = buffer.getvalue()

        self.assertIn("alpha", output)
        self.assertIn("beta", output)
        self.assertIn("json_repair", output)

    def test_total_includes_unknown_kinds(self):
        # A compensation kind added after the script froze its vocabulary must
        # still count toward the model total (additive enum tolerance).
        inv = invocation("s1", "i1", "alpha")
        inv.compensations = [{"kind": "future_kind", "detail": "-"}]

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            compensations.run(make_dataset([inv]))
        output = buffer.getvalue()

        # Seven known-kind columns show 0; the total column shows 1.
        self.assertRegex(output, r"alpha\s+0\s+0\s+0\s+0\s+0\s+0\s+0\s+0\s+1")

    def test_total_excludes_retry_derived_compensation_kind(self):
        # A newer sidecar records the context-overflow retry both as a
        # compensation event and as a retry_scheduled record; the total must
        # count it once, via the retry-derived column.
        inv = invocation("s1", "i1", "alpha")
        inv.compensations = [{"kind": "context_overflow_retry"}]
        inv.retries = [{"reason": "context_overflow"}]

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            compensations.run(make_dataset([inv]))
        output = buffer.getvalue()

        # Overflow column 1, total column 1, not 2.
        self.assertRegex(output, r"alpha\s+0\s+0\s+0\s+0\s+0\s+0\s+0\s+1\s+1")

    def test_judge_overridden_reports_model_and_count(self):
        inv = invocation("s1", "i1", "alpha")
        inv.compensations = [
            {"kind": "judge_verdict", "detail": "block:rm", "overridden": True, "latency_ms": 5},
            {"kind": "judge_verdict", "detail": "block:rm", "latency_ms": 4},
        ]

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            compensations.run(make_dataset([inv]))
        output = buffer.getvalue()

        self.assertIn("Judge block overridden by allowlist", output)
        self.assertIn("alpha", output)
        # Only the overridden verdict counts: one, not two.
        self.assertIn("1", output)

    def test_total_includes_retry_derived_overflow(self):
        # A legacy sidecar has the retry_scheduled record but no compensation
        # record; the total must still count the overflow retry.
        legacy = invocation("s1", "i1", "alpha")
        legacy.retries = [{"reason": "context_overflow"}]

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            compensations.run(make_dataset([legacy]))
        output = buffer.getvalue()

        self.assertIn("context-overflow retries", output)
        # The alpha row must show overflow 1 and total 1.
        self.assertRegex(output, r"alpha\s+0\s+0\s+0\s+0\s+0\s+0\s+0\s+1\s+1")


if __name__ == "__main__":
    unittest.main()
