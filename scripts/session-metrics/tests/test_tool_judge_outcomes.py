#!/usr/bin/env python3
"""Unit tests for Bash judge outcomes by `reason` presence (tools.py).

The report pairs a transcript Bash call with its judge outcome by the SHA-256
digest of the transcript's raw `call_id` (issue #404). These tests cover every
outcome, retries, out-of-order and concurrent records, and the unlinked,
ambiguous, and legacy shapes the join must report rather than guess at.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import sys
import unittest
from collections import Counter
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import tools

HEADERS = ["model", "reason", "calls"] + list(tools.JUDGE_OUTCOME_HEADERS) + ["block %", "warn %"]


def cell(row: list, header: str) -> str:
    """One cell of an outcome row, addressed by its header."""
    return row[HEADERS.index(header)]


def count(row: list, header: str) -> int:
    return int(cell(row, header).replace(",", ""))


def dataset(sessions: list[cakelib.Session], invocations: list[cakelib.Invocation]) -> cakelib.Dataset:
    return cakelib.Dataset(
        sessions=sessions,
        invocations=invocations,
        sessions_dir=None,
        telemetry_dir=None,
        cutoff=None,
    )


def session(session_id: str, model: str, calls: list[tuple[str, str | None]]) -> cakelib.Session:
    """A Session whose Bash calls are the given `(call_id, reason)` pairs.

    `None` omits the `reason` argument; an empty string supplies an empty one,
    which the metrics count as absent.
    """
    records: list[dict] = []
    for call_id, reason in calls:
        arguments: dict = {"command": f"echo {call_id or 'unidentified'}"}
        if reason is not None:
            arguments["reason"] = reason
        records.append(
            {
                "type": "function_call",
                "call_id": call_id,
                "name": "Bash",
                "arguments": json.dumps(arguments),
            }
        )
        records.append({"type": "function_call_output", "call_id": call_id, "output": "ok"})
    s = cakelib.Session(
        id=session_id,
        path=None,
        size=0,
        mtime=datetime.now(timezone.utc),
        records=records,
    )
    s.model = model
    return s


def invocation(session_id: str, model: str = "alpha") -> cakelib.Invocation:
    inv = cakelib.Invocation(session_id, "i1")
    inv.init = {"model": model, "working_directory": "/proj"}
    return inv


def event(raw_call_id: str | None, kind: str = "judge_verdict", detail: str | None = "allow") -> dict:
    """A judge compensation event as telemetry records it: a digest, no raw id.

    `raw_call_id` None leaves `call_id` unset, which is the legacy shape written
    before the linkage field existed.
    """
    e: dict = {"kind": kind}
    if detail is not None:
        e["detail"] = detail
    if raw_call_id is not None:
        e["call_id"] = cakelib.call_id_digest(raw_call_id)
    return e


class PairingTest(unittest.TestCase):
    def assert_outcome_columns_sum_to_calls(self, row: list) -> None:
        counted = sum(count(row, header) for header in tools.JUDGE_OUTCOME_HEADERS)
        self.assertEqual(
            counted,
            count(row, "calls"),
            f"outcome columns must account for every call: {row}",
        )

    def test_verdicts_split_by_reason_presence(self):
        s = session(
            "s1",
            "alpha",
            [
                ("call-1", "clean the build dir"),
                ("call-2", None),
                ("call-3", "publish the release"),
                ("call-4", None),
            ],
        )
        inv = invocation("s1")
        inv.compensations = [
            event("call-1", "judge_verdict", "block:destructive-rm"),
            event("call-2", "judge_verdict", "allow"),
            event("call-3", "judge_verdict", "warn:rg-replace-footgun"),
            event("call-4", "judge_verdict", "allow"),
        ]

        rows, linkage = tools.bash_judge_outcomes(dataset([s], [inv]))

        self.assertEqual(
            [row[:2] for row in rows],
            [
                ["alpha", "with reason"],
                ["alpha", "without reason"],
                ["TOTAL", "with reason"],
                ["TOTAL", "without reason"],
            ],
        )
        for row in rows:
            self.assert_outcome_columns_sum_to_calls(row)

        with_reason = rows[0]
        self.assertEqual(count(with_reason, "calls"), 2)
        self.assertEqual(count(with_reason, "allow"), 0)
        self.assertEqual(count(with_reason, "warn"), 1)
        self.assertEqual(count(with_reason, "block"), 1)
        self.assertEqual(cell(with_reason, "block %"), "50.0%")
        self.assertEqual(cell(with_reason, "warn %"), "50.0%")

        without_reason = rows[1]
        self.assertEqual(count(without_reason, "calls"), 2)
        self.assertEqual(count(without_reason, "allow"), 2)
        self.assertEqual(cell(without_reason, "block %"), "0.0%")
        self.assertEqual(cell(without_reason, "warn %"), "0.0%")

        # The TOTAL rows add both models' groups up.
        self.assertEqual(count(rows[2], "block"), 1)
        self.assertEqual(count(rows[3], "allow"), 2)
        self.assertEqual(linkage["calls"], 4)
        self.assertEqual(linkage["paired"], 4)
        self.assertEqual(linkage["events"], 4)
        self.assertEqual(linkage["events_paired"], 4)
        self.assertEqual(linkage["events_ambiguous"], 0)
        self.assertEqual(linkage["events_keyless"], Counter())
        self.assertEqual(linkage["events_windowless"], Counter())

    def test_calls_without_a_judge_outcome_are_split_by_cause(self):
        # Three causes, one row each: a call with no provider-assigned id has
        # nothing to join on, a call whose session has no telemetry records
        # cannot be joined, and a call whose session has telemetry but no event
        # for its digest is unjudged.
        identified = session("s1", "alpha", [("call-1", None), ("", None)])
        sidecar = invocation("s1", "alpha")
        without_sidecar = session("s2", "beta", [("call-2", None)])

        rows, linkage = tools.bash_judge_outcomes(
            dataset([identified, without_sidecar], [sidecar])
        )

        self.assertEqual([row[:2] for row in rows[:2]], [
            ["alpha", "without reason"],
            ["beta", "without reason"],
        ])
        self.assertEqual(count(rows[0], "no call id"), 1)
        self.assertEqual(count(rows[0], "no event"), 1)
        self.assertEqual(count(rows[0], "no sidecar"), 0)
        self.assertEqual(count(rows[1], "no sidecar"), 1)
        self.assertEqual(count(rows[1], "no call id"), 0)
        self.assertEqual(count(rows[1], "no event"), 0)
        for row in rows:
            self.assert_outcome_columns_sum_to_calls(row)

        self.assertEqual(linkage["no_call_id"], 1)
        self.assertEqual(linkage["no_event"], 1)
        self.assertEqual(linkage["no_sidecar"], 1)
        self.assertEqual(linkage["paired"], 0)

    def test_judge_event_outcome_only_returns_columns_the_report_has(self):
        # A verdict value or event kind outside the vocabulary must land in a
        # column rather than being dropped or riding an existing one, and the
        # join's own bookkeeping keys are never judge outcomes.
        self.assertTrue(set(tools.JUDGE_VERDICTS) <= set(tools.JUDGE_OUTCOME_LABELS))
        produced = {
            tools.judge_event_outcome({"kind": "judge_verdict", "detail": detail})
            for detail in ("allow", "warn:code", "block:code", "verdict-from-the-future", "")
        } | {
            tools.judge_event_outcome({"kind": kind})
            for kind in ("judge_fail_closed", "judge_bypass", "judge_future_kind")
        }
        self.assertEqual(
            produced, {"allow", "warn", "block", "fail_closed", "bypass", "other"}
        )
        self.assertTrue(produced <= set(tools.JUDGE_OUTCOME_LABELS))

    def test_outcomes_without_a_verdict_are_reported_separately(self):
        s = session(
            "s1",
            "alpha",
            [("call-1", None), ("call-2", None), ("call-3", None), ("call-4", None)],
        )
        inv = invocation("s1")
        inv.compensations = [
            event("call-1", "judge_fail_closed", "timeout"),
            event("call-2", "judge_bypass", None),
            event("call-3", "judge_verdict", "verdict-from-the-future"),
            event("call-4", "judge_verdict", "allow"),
        ]

        rows, _ = tools.bash_judge_outcomes(dataset([s], [inv]))
        row = rows[0]

        self.assertEqual(count(row, "allow"), 1)
        self.assertEqual(count(row, "fail-closed"), 1)
        self.assertEqual(count(row, "bypass"), 1)
        # An unrecognized verdict value stays visible instead of riding a column.
        self.assertEqual(count(row, "other"), 1)
        self.assertEqual(count(row, "block"), 0)
        # Fail-closed, bypass, and other are judge outcomes, not verdicts: the
        # rate denominator is the one allow verdict.
        self.assertEqual(cell(row, "warn %"), "0.0%")
        self.assert_outcome_columns_sum_to_calls(row)

    def test_retried_call_contributes_one_outcome(self):
        s = session("s1", "alpha", [("call-1", None)])
        inv = invocation("s1")
        inv.compensations = [event("call-1", "judge_verdict", "block:rm-rf")]
        inv.judge_attempts = [
            {
                "call_id": cakelib.call_id_digest("call-1"),
                "retry_ordinal": 0,
                "terminal_class": "timeout",
            },
            {
                "call_id": cakelib.call_id_digest("call-1"),
                "retry_ordinal": 1,
                "terminal_class": "verdict",
            },
        ]

        rows, linkage = tools.bash_judge_outcomes(dataset([s], [inv]))

        self.assertEqual(count(rows[0], "calls"), 1)
        self.assertEqual(count(rows[0], "block"), 1)
        self.assertEqual(cell(rows[0], "block %"), "100.0%")
        # Two provider calls, one logical judge evaluation.
        self.assertEqual(linkage["attempts"], 2)
        self.assertEqual(linkage["attempted_calls"], 1)
        self.assertEqual(linkage["retried_calls"], 1)
        self.assert_outcome_columns_sum_to_calls(rows[0])

    def test_pairing_ignores_record_order(self):
        # Concurrent calls and an out-of-order sidecar (judge events written
        # before or after unrelated transcript records) must pair identically:
        # the join never consults timestamps or record positions.
        calls = [("call-1", None), ("call-2", "why"), ("call-3", None)]
        events = [
            event("call-1", "judge_verdict", "allow"),
            event("call-2", "judge_verdict", "warn:footgun"),
            event("call-3", "judge_verdict", "block:rm-rf"),
        ]
        in_order = invocation("s1")
        in_order.compensations = events
        reversed_events = invocation("s1")
        reversed_events.compensations = list(reversed(events))

        ordered_rows, _ = tools.bash_judge_outcomes(
            dataset([session("s1", "alpha", calls)], [in_order])
        )
        reversed_rows, _ = tools.bash_judge_outcomes(
            dataset([session("s1", "alpha", calls)], [reversed_events])
        )
        shuffled_rows, _ = tools.bash_judge_outcomes(
            dataset([session("s1", "alpha", list(reversed(calls)))], [reversed_events])
        )

        self.assertEqual(ordered_rows, reversed_rows)
        self.assertEqual(ordered_rows, shuffled_rows)
        # One verdict per call, whichever order the records arrived in: the
        # `without reason` group holds the `allow` and `block` calls.
        self.assertEqual(count(ordered_rows[1], "allow"), 1)
        self.assertEqual(count(ordered_rows[1], "block"), 1)
        self.assertEqual(count(ordered_rows[0], "warn"), 1)

    def test_ambiguous_pairs_are_reported_not_guessed(self):
        # Two transcript calls sharing one call id: the pair is indeterminate.
        duplicated = session("s1", "alpha", [("call-1", None), ("call-1", None)])
        inv = invocation("s1")
        inv.compensations = [event("call-1", "judge_verdict", "allow")]

        rows, linkage = tools.bash_judge_outcomes(dataset([duplicated], [inv]))

        self.assertEqual(count(rows[0], "calls"), 2)
        self.assertEqual(count(rows[0], "ambiguous"), 2)
        self.assertEqual(count(rows[0], "allow"), 0)
        self.assertEqual(linkage["ambiguous"], 2)
        self.assertEqual(linkage["events_ambiguous"], 1)
        self.assert_outcome_columns_sum_to_calls(rows[0])

        # The mirror image: one call with two judge events for its digest (for
        # instance a replayed evaluation) is equally indeterminate, so it is
        # excluded rather than paired by record order.
        single = session("s1", "alpha", [("call-1", None)])
        replayed = invocation("s1")
        replayed.compensations = [
            event("call-1", "judge_verdict", "allow"),
            event("call-1", "judge_verdict", "block:rm-rf"),
        ]

        rows, linkage = tools.bash_judge_outcomes(dataset([single], [replayed]))

        self.assertEqual(count(rows[0], "ambiguous"), 1)
        self.assertEqual(count(rows[0], "allow"), 0)
        self.assertEqual(count(rows[0], "block"), 0)
        self.assertEqual(linkage["events_ambiguous"], 2)

    def test_unmatched_and_legacy_records_are_accounted_for(self):
        # A judge event with no digest (written before the linkage field
        # existed), an event whose transcript call is outside the window, and
        # an event from another session whose digest collides with a call in
        # this one: none of them is dropped, and the report says which is which.
        s = session("s1", "alpha", [("call-1", None), ("", None)])
        inv = invocation("s1")
        inv.compensations = [
            event("call-1", "judge_verdict", "allow"),
            event(None, "judge_bypass", None),
            event("call-outside-window", "judge_verdict", "block:rm-rf"),
        ]
        other_session = invocation("s2")
        other_session.compensations = [event("call-1", "judge_verdict", "warn:footgun")]

        rows, linkage = tools.bash_judge_outcomes(dataset([s], [inv, other_session]))

        self.assertEqual(count(rows[0], "calls"), 2)
        self.assertEqual(count(rows[0], "allow"), 1)
        # The call with no provider-assigned id is a cause the report names.
        self.assertEqual(count(rows[0], "no call id"), 1)
        self.assert_outcome_columns_sum_to_calls(rows[0])

        self.assertEqual(linkage["events"], 4)
        self.assertEqual(linkage["events_paired"], 1)
        self.assertEqual(linkage["events_ambiguous"], 0)
        # Nothing is dropped: every event is paired, ambiguous, or unpaired.
        self.assertEqual(
            linkage["events_paired"]
            + linkage["events_ambiguous"]
            + sum(linkage["events_keyless"].values())
            + sum(linkage["events_windowless"].values()),
            linkage["events"],
        )
        self.assertEqual(linkage["events_keyless"], Counter({"bypass": 1}))
        # The out-of-window event and the other session's event are both events
        # whose transcript call this window cannot pair.
        self.assertEqual(linkage["events_windowless"], Counter({"block": 1, "warn": 1}))

    def test_no_bash_calls_yields_no_rows(self):
        rows, linkage = tools.bash_judge_outcomes(dataset([], []))
        self.assertEqual(rows, [])
        self.assertEqual(linkage["calls"], 0)


class ReportTest(unittest.TestCase):
    def test_report_prints_the_table_and_linkage_notes(self):
        s = session("s1", "alpha", [("call-1", "clean up"), ("call-2", None)])
        inv = invocation("s1")
        inv.compensations = [
            event("call-1", "judge_verdict", "block:destructive-rm"),
            event("call-2", "judge_verdict", "allow"),
        ]

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            tools.run(dataset([s], [inv]))
        text = out.getvalue()

        self.assertIn("Bash judge outcomes by `reason` presence:", text)
        self.assertIn(
            "Judge linkage (SHA-256 digest of the transcript call id, same session):", text
        )
        self.assertIn("the coverage total above", text)
        self.assertIn("retried", text)
        # The report names each cause of a missing judge outcome rather than
        # leaving one unpaired total to interpret.
        self.assertIn("no judge outcome", text)
        for cause in ("no call id", "no sidecar", "no event", "no linkage key"):
            self.assertIn(cause, text)

    def test_report_never_leaks_commands_reasons_or_call_ids(self):
        command = "rm -rf /tmp/topsecret"
        reason = "clean up before final packaging"
        raw_call_id = "call-secret-identifier"
        records = [
            {
                "type": "function_call",
                "call_id": raw_call_id,
                "name": "Bash",
                "arguments": json.dumps({"command": command, "reason": reason}),
            },
            {"type": "function_call_output", "call_id": raw_call_id, "output": "ok"},
        ]
        s = cakelib.Session(
            id="s1",
            path=None,
            size=0,
            mtime=datetime.now(timezone.utc),
            records=records,
        )
        s.model = "alpha"
        inv = invocation("s1")
        inv.compensations = [event(raw_call_id, "judge_verdict", "block:rm-rf")]

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            tools.run(dataset([s], [inv]))
        text = out.getvalue()

        # Metadata-only: presence, bounded outcome, model, and counts. Neither
        # the inputs nor the join key itself are reported.
        self.assertIn("Bash judge outcomes by `reason` presence:", text)
        for secret in (command, reason, raw_call_id, cakelib.call_id_digest(raw_call_id)):
            self.assertNotIn(secret, text)


if __name__ == "__main__":
    unittest.main()
