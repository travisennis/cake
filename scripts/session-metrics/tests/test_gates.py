#!/usr/bin/env python3
"""Unit tests for the generic policy-gate report (`gates.py`).

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
import gates


def make_dataset(invocations: list[cakelib.Invocation]) -> cakelib.Dataset:
    return cakelib.Dataset(
        sessions=[],
        invocations=invocations,
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )


def invocation(session_id: str = "s1", invocation_id: str = "i1") -> cakelib.Invocation:
    inv = cakelib.Invocation(session_id, invocation_id)
    inv.init = {"model": "alpha", "working_directory": "/proj"}
    return inv


class AggregateTest(unittest.TestCase):
    def test_counts_startups_decisions_failures_and_evidence(self):
        inv = invocation()
        inv.gate_startups = [
            {"gate": "safety", "mode": "enforce", "outcome": "ready"},
            {"gate": "observer", "mode": "shadow", "outcome": "failed",
             "failure_class": "config"},
        ]
        inv.gate_decisions = [
            {"gate": "safety", "mode": "enforce", "decision": "allow",
             "latency_ms": 10},
            {"gate": "safety", "mode": "enforce", "decision": "block",
             "latency_ms": 20},
            {"gate": "observer", "mode": "shadow", "decision": "warn",
             "latency_ms": 5},
            {"gate": None, "mode": "enforce", "bypassed": True, "latency_ms": 0},
            {"gate": "safety", "mode": "enforce", "failure_class": "timeout",
             "latency_ms": 30},
        ]
        inv.gate_evidence = [
            {"gate": "safety", "kind": "attempt"},
            {"gate": "safety", "kind": "attempt"},
        ]

        stats = gates.aggregate(make_dataset([inv]))

        safety = stats["safety"]
        self.assertEqual(safety["startups"], 1)
        self.assertEqual(safety["startup_failures"], 0)
        self.assertEqual(safety["decisions"], 3)
        self.assertEqual(safety["outcomes"]["allow"], 1)
        self.assertEqual(safety["outcomes"]["block"], 1)
        self.assertEqual(safety["outcomes"]["failure"], 1)
        self.assertEqual(safety["evidence"], 2)
        observer = stats["observer"]
        self.assertEqual(observer["startup_failures"], 1)
        self.assertEqual(observer["outcomes"]["warn"], 1)
        self.assertEqual(stats["(bypass)"]["outcomes"]["bypass"], 1)
        self.assertEqual(safety["failure_classes"]["timeout"], 1)

    def test_gate_decision_outcome_classifies_non_decisions(self):
        cases = [
            ({"bypassed": True}, "bypass"),
            ({"disabled": True, "failure_class": "crash"}, "disabled"),
            ({"failure_class": "timeout"}, "failure"),
            ({"decision": "allow"}, "allow"),
            ({"decision": "future"}, "other"),
            ({}, "other"),
        ]
        for record, expected in cases:
            self.assertEqual(cakelib.gate_decision_outcome(record), expected)


class RunTest(unittest.TestCase):
    def test_report_names_gate_and_keeps_raw_values_out(self):
        inv = invocation()
        inv.gate_startups = [
            {"gate": "safety", "mode": "enforce", "outcome": "ready"},
        ]
        inv.gate_decisions = [
            {"gate": "safety", "mode": "enforce", "decision": "block",
             "code": "rm-rf", "latency_ms": 12, "call_id": "digest",
             "message": "secret message"},
        ]
        inv.gate_evidence = [
            {"gate": "safety", "kind": "attempt", "data": {"command": "rm -rf /"}},
        ]
        inv.init["working_directory"] = "/proj"

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            gates.run(make_dataset([inv]))
        output = buf.getvalue()

        self.assertIn("POLICY GATES", output)
        self.assertIn("safety", output)
        self.assertIn("block", output)
        self.assertNotIn("secret message", output)
        self.assertNotIn("rm -rf /", output)
        self.assertNotIn("/proj", output)

    def test_empty_dataset_says_no_records(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            gates.run(make_dataset([]))
        self.assertIn("No generic gate_startup or gate_decision records", buf.getvalue())


if __name__ == "__main__":
    unittest.main()
