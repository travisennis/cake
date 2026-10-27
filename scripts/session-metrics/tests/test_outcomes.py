#!/usr/bin/env python3
"""Unit tests for outcomes.py: per-session turn totals and the permission-denial
source breakdown.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import contextlib
import io
import os
import re
import sys
import unittest
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import outcomes

MTIME = datetime(2026, 10, 1, tzinfo=timezone.utc)


def make_dataset(invocations=(), sessions=()) -> cakelib.Dataset:
    return cakelib.Dataset(
        sessions=list(sessions),
        invocations=list(invocations),
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )


def invocation(session_id: str, invocation_id: str, turn_count: int) -> cakelib.Invocation:
    inv = cakelib.Invocation(session_id, invocation_id)
    inv.init = {"model": "alpha", "working_directory": "/proj"}
    inv.summary = {"turn_count": turn_count, "duration_ms": 1000, "success": True}
    return inv


def make_session(session_id: str, tasks: list[dict]) -> cakelib.Session:
    """A transcript session whose completed tasks are the supplied records.

    The records are task_complete only and carry no timestamps, so window
    attribution falls back to the file mtime, which the all-time cutoff admits.
    """
    records = [
        dict(task, type="task_complete", task_id=f"task-{i}")
        for i, task in enumerate(tasks)
    ]
    return cakelib.Session(
        id=session_id,
        path=Path("/sessions") / f"{session_id}.jsonl",
        size=0,
        mtime=MTIME,
        records=records,
    )


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


class DenialSourceAttributionTest(unittest.TestCase):
    """The denial breakdown is metadata-only and reconciles with its total."""

    def run_outcomes(self, tasks: list[dict]) -> str:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            outcomes.run(make_dataset(sessions=[make_session("session-denials", tasks)]))
        return out.getvalue()

    def denial_table(self, text: str) -> list[tuple[str, int, int]]:
        """The printed per-source table as (source, events, tasks) rows."""
        lines = text.splitlines()
        start = next(i for i, line in enumerate(lines) if line.strip().startswith("source"))
        rows = []
        for line in lines[start + 2:]:
            if not line.strip():
                break
            source, events, tasks = line.split()
            rows.append((source, int(events.replace(",", "")), int(tasks.replace(",", ""))))
        return rows

    def given_totals(self, text: str) -> tuple[int, int]:
        match = re.search(r"Permission denials recorded: ([\d,]+) across ([\d,]+) tasks", text)
        self.assertIsNotNone(match, text)
        return int(match.group(1).replace(",", "")), int(match.group(2).replace(",", ""))

    def test_attributes_each_source_with_event_and_task_counts(self):
        text = self.run_outcomes([
            {
                "subtype": "success",
                "permission_denials": [
                    "Bash(call-1): judge block: destructive-rm",
                    "Bash(call-2): judge block: credential-disclosure",
                    "Bash(call-3): judge fail-closed: timeout",
                    "Bash(call-4): sandbox: read /outside/grant.txt",
                    "Bash(call-5): path: /outside/grant.txt",
                    "Bash(call-6): /repo/.cake/hooks.json: Blocked: cargo test --lib",
                ],
            },
            {
                "subtype": "success",
                "permission_denials": ["Bash(call-7): judge block: destructive-rm"],
            },
        ])
        self.assertEqual(self.given_totals(text), (7, 2))
        self.assertEqual(self.denial_table(text), [
            ("judge-block", 3, 2),
            ("judge-fail-closed", 1, 1),
            ("sandbox", 1, 1),
            ("path", 1, 1),
            ("hook", 1, 1),
            ("TOTAL", 7, 2),
        ])

    def test_source_counts_sum_to_the_flat_total(self):
        text = self.run_outcomes([
            {
                "permission_denials": [
                    "Bash(call-1): judge block: destructive-rm",
                    "Bash(call-2): sandbox: write /outside/one.txt",
                ],
            },
            {
                "permission_denials": [
                    "Bash(call-3): sandbox: execute /outside/bin/tool",
                    "Bash(call-4): /repo/.cake/hooks.json: Blocked: policy",
                ],
            },
            {"subtype": "success"},
        ])
        total, task_total = self.given_totals(text)
        rows = self.denial_table(text)
        self.assertEqual(rows[-1][0], "TOTAL")
        self.assertEqual(rows[-1][1:], (total, task_total))
        self.assertEqual(sum(events for _source, events, _tasks in rows[:-1]), total)

    def test_unrecognized_label_shapes_are_counted_as_other(self):
        text = self.run_outcomes([
            {
                "permission_denials": [
                    "bash: rm -rf /",                    # legacy label, no tool/call separator
                    "Bash(call-1): blocked by hook",     # separator, but no recognizable shape
                    "Bash(call-2)",                      # no separator text at all
                ],
            },
        ])
        self.assertEqual(self.given_totals(text), (3, 1))
        self.assertEqual(self.denial_table(text), [("other", 3, 1), ("TOTAL", 3, 1)])

    def test_hook_reason_token_is_attributed_to_that_token(self):
        """The recorded ambiguity: a hook reason's leading token wins."""
        text = self.run_outcomes([
            {
                "permission_denials": [
                    "Bash(call-1): /repo/.cake/hooks.json: sandbox: write /outside/file.txt",
                    "Bash(call-2): /repo/.cake/hooks.json: judge block: destructive-rm",
                    "Bash(call-3): /repo/a/hooks.json: Blocked: first; /repo/b/hooks.json: Blocked: second",
                ],
            },
        ])
        self.assertEqual(self.denial_table(text), [
            ("judge-block", 1, 1),
            ("sandbox", 1, 1),
            ("hook", 1, 1),
            ("TOTAL", 3, 1),
        ])

    def test_denial_label_text_never_reaches_the_report(self):
        secret = "/tmp/secret-cake-denial-path/leak.txt"
        text = self.run_outcomes([
            {
                "permission_denials": [
                    f"Bash(call-1): sandbox: write {secret}",
                    f"Bash(call-2): path: {secret}",
                    "Bash(call-3): /repo/.cake/hooks.json: Blocked: MARKER-REASON-TEXT",
                    "Bash(call-4): judge block: MARKER-VERDICT-CODE",
                ],
            },
        ])
        self.assertNotIn(secret, text)
        self.assertNotIn("secret-cake-denial-path", text)
        self.assertNotIn("MARKER-REASON-TEXT", text)
        self.assertNotIn("MARKER-VERDICT-CODE", text)
        self.assertNotIn("Blocked", text)
        self.assertEqual(self.denial_table(text)[-1], ("TOTAL", 4, 1))


class DenialSourceTest(unittest.TestCase):
    """The precedence table, pinned without going through the report."""

    def test_precedence(self):
        cases = {
            "Bash(call-1): judge block: destructive-rm": "judge-block",
            "Bash(call-1): judge fail-closed: transport": "judge-fail-closed",
            "Bash(call-1): sandbox: read /outside/grant.txt": "sandbox",
            "Bash(call-1): path: /outside/grant.txt": "path",
            "Bash(call-1): /repo/.cake/hooks.json: Blocked: policy": "hook",
            "Bash(call-1): /repo/.cake/hooks.json: judge block: x": "judge-block",
            "Bash(call-1): legacy free text": "other",
            "bash: rm -rf /": "other",
            "Bash(call-1)": "other",
        }
        for label, source in cases.items():
            self.assertEqual(outcomes.denial_source(label), source, label)

    def test_non_string_label_is_other(self):
        self.assertEqual(outcomes.denial_source(None), "other")
        self.assertEqual(outcomes.denial_source(["Bash(call-1)"]), "other")


if __name__ == "__main__":
    unittest.main()
