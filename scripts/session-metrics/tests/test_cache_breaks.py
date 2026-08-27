#!/usr/bin/env python3
"""Unit tests for telemetry prompt-cache break detection."""

import contextlib
import io
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cache_breaks
import cakelib


def invocation(
    session_id: str = "session-1",
    invocation_id: str = "inv-1",
    model: str = "model-a",
) -> cakelib.Invocation:
    inv = cakelib.Invocation(session_id, invocation_id)
    inv.init = {"model": model, "working_directory": "/project"}
    return inv


def add_attempt(
    inv: cakelib.Invocation,
    turn: int,
    timestamp: str,
    prompt: int,
    *,
    cached: int = 0,
    cache_write: int = 0,
    attempt: int = 1,
) -> None:
    inv.attempts.append({
        "type": "api_attempt",
        "session_id": inv.session_id,
        "invocation_id": inv.invocation_id,
        "turn_index": turn,
        "attempt": attempt,
        "timestamp": timestamp,
        "usage": {
            "input_tokens": prompt,
            "input_tokens_details": {
                "cached_tokens": cached,
                "cache_write_tokens": cache_write,
            },
        },
    })


class CacheBreakScanTest(unittest.TestCase):
    def test_detects_full_miss_after_cache_activity(self):
        inv = invocation()
        add_attempt(inv, 1, "2026-08-26T10:00:00Z", 100_000, cache_write=100_000)
        add_attempt(inv, 2, "2026-08-26T10:00:10Z", 100_000, cached=99_500, cache_write=500)
        add_attempt(inv, 3, "2026-08-26T10:00:20Z", 100_000)

        summary = cache_breaks.scan([inv])

        self.assertEqual(summary.miss_count, 1)
        self.assertEqual(summary.missed_tokens, 100_000)
        self.assertEqual(summary.misses[0].cause, cache_breaks.CAUSE_GENERIC)
        self.assertEqual(summary.misses[0].idle_ms, 10_000)
        self.assertIsNone(summary.missed_cost)

    def test_does_not_count_provider_that_never_reports_cache(self):
        inv = invocation()
        add_attempt(inv, 1, "2026-08-26T10:00:00Z", 100_000)
        add_attempt(inv, 2, "2026-08-26T10:00:10Z", 100_000)

        summary = cache_breaks.scan([inv])

        self.assertEqual(summary.miss_count, 0)

    def test_retries_for_one_turn_count_once(self):
        inv = invocation()
        add_attempt(inv, 1, "2026-08-26T10:00:00Z", 100_000, cache_write=100_000, attempt=1)
        add_attempt(inv, 1, "2026-08-26T10:00:01Z", 100_000, cache_write=100_000, attempt=2)
        add_attempt(inv, 2, "2026-08-26T10:00:02Z", 100_000)

        summary = cache_breaks.scan([inv])

        self.assertEqual(summary.miss_count, 1)
        self.assertEqual(summary.misses[0].turn_index, 2)

    def test_labels_model_switch_across_invocations(self):
        first = invocation(invocation_id="inv-1", model="model-a")
        second = invocation(invocation_id="inv-2", model="model-b")
        add_attempt(first, 1, "2026-08-26T10:00:00Z", 50_000, cache_write=50_000)
        add_attempt(second, 1, "2026-08-26T10:00:10Z", 50_000)

        summary = cache_breaks.scan([first, second])

        self.assertEqual(summary.miss_count, 1)
        self.assertEqual(summary.misses[0].cause, cache_breaks.CAUSE_MODEL_SWITCH)

    def test_labels_idle_ttl_at_five_minutes(self):
        inv = invocation()
        add_attempt(inv, 1, "2026-08-26T10:00:00Z", 50_000, cache_write=50_000)
        add_attempt(inv, 2, "2026-08-26T10:05:00Z", 50_000)

        summary = cache_breaks.scan([inv])

        self.assertEqual(summary.miss_count, 1)
        self.assertEqual(summary.misses[0].cause, cache_breaks.CAUSE_IDLE_TTL)
        self.assertEqual(summary.misses[0].idle_ms, cache_breaks.CACHE_TTL_MS)

    def test_labels_idle_ttl_across_invocations(self):
        first = invocation(invocation_id="inv-1")
        second = invocation(invocation_id="inv-2")
        add_attempt(first, 1, "2026-08-26T10:00:00Z", 50_000, cache_write=50_000)
        add_attempt(second, 1, "2026-08-26T10:05:00Z", 50_000)

        summary = cache_breaks.scan([first, second])

        self.assertEqual(summary.miss_count, 1)
        self.assertEqual(summary.misses[0].cause, cache_breaks.CAUSE_IDLE_TTL)

    def test_ignores_miss_at_noise_floor(self):
        inv = invocation()
        add_attempt(inv, 1, "2026-08-26T10:00:00Z", 2_000, cache_write=2_000)
        add_attempt(inv, 2, "2026-08-26T10:00:01Z", 2_000, cached=976)

        summary = cache_breaks.scan([inv])

        self.assertEqual(summary.miss_count, 0)

    def test_zero_prompt_turn_does_not_replace_previous_request(self):
        inv = invocation()
        add_attempt(inv, 1, "2026-08-26T10:00:00Z", 100_000, cache_write=100_000)
        add_attempt(inv, 2, "2026-08-26T10:00:01Z", 0)
        add_attempt(inv, 3, "2026-08-26T10:00:02Z", 100_000)

        summary = cache_breaks.scan([inv])

        self.assertEqual(summary.miss_count, 1)
        self.assertEqual(summary.misses[0].turn_index, 3)
        self.assertEqual(summary.misses[0].missed_tokens, 100_000)

    def test_cache_write_is_in_usage_totals(self):
        totals = cakelib.usage_totals([{
            "input_tokens": 100,
            "input_tokens_details": {"cached_tokens": 60, "cache_write_tokens": 40},
        }])

        self.assertEqual(totals["cached"], 60)
        self.assertEqual(totals["cache_write"], 40)


class CacheBreakReportTest(unittest.TestCase):
    def test_report_surfaces_totals_and_unknown_cost(self):
        inv = invocation()
        add_attempt(inv, 1, "2026-08-26T10:00:00Z", 100_000, cache_write=100_000)
        add_attempt(inv, 2, "2026-08-26T10:00:01Z", 100_000)
        data = cakelib.Dataset(
            sessions=[],
            invocations=[inv],
            sessions_dir=None,
            telemetry_dir=None,
            cutoff=None,
        )
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            cache_breaks.run(data)

        text = output.getvalue()
        self.assertIn("PROMPT CACHE BREAKS", text)
        self.assertIn("Detected misses: 1", text)
        self.assertIn("Missed prompt tokens: 100,000", text)
        self.assertIn("Wasted cost: unavailable", text)
        self.assertIn("session-1", text)


if __name__ == "__main__":
    unittest.main()
