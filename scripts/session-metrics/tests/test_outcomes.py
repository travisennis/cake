#!/usr/bin/env python3
"""Unit tests for the per-session turn totals in outcomes.py.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import contextlib
import io
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import outcomes


def make_dataset(invocations: list[cakelib.Invocation]) -> cakelib.Dataset:
    return cakelib.Dataset(
        sessions=[],
        invocations=invocations,
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )


def invocation(session_id: str, invocation_id: str, turn_count: int) -> cakelib.Invocation:
    inv = cakelib.Invocation(session_id, invocation_id)
    inv.init = {"model": "alpha", "working_directory": "/proj"}
    inv.summary = {"turn_count": turn_count, "duration_ms": 1000, "success": True}
    return inv


class TurnsPerSessionTest(unittest.TestCase):
    def run_outcomes(self, invocations) -> str:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            outcomes.run(make_dataset(invocations))
        return out.getvalue()

    def test_sums_turn_counts_across_invocations(self):
        text = self.run_outcomes([
            invocation("session-aaa", "i1", 30),
            invocation("session-aaa", "i2", 24),
            invocation("session-bbb", "i1", 10),
        ])
        self.assertIn("Turns per session", text)
        # The table lists summed totals per session, highest first.
        aaa = text.index("session-aaa")
        bbb = text.index("session-bbb")
        self.assertLess(aaa, bbb)
        # session-aaa: 2 invocations, 30 + 24 = 54 turns.
        self.assertGreater(text[aaa:bbb].count("2"), 0)
        self.assertIn("54", text[aaa:bbb])
        self.assertNotIn("30", text[aaa:bbb])
        # session-bbb: 1 invocation, 10 turns.
        self.assertIn("10", text[bbb:])

    def test_single_invocation_session_reports_its_own_turn_count(self):
        text = self.run_outcomes([
            invocation("session-ccc", "i1", 42),
        ])
        self.assertIn("session-ccc", text)
        self.assertIn("42", text)
        self.assertIn("turns/session", text)


if __name__ == "__main__":
    unittest.main()