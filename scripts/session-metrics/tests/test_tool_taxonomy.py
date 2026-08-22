#!/usr/bin/env python3
"""Unit tests for the tool failure taxonomy in cakelib.classify_tool_error.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from cakelib import classify_tool_error


class ClassifyToolErrorTest(unittest.TestCase):
    def test_judge_block(self):
        """Judge block: `BLOCKED` + `Reason:` (src/clients/tools/bash.rs).

        Raw form. Transcripts store this output with an `Error: ` prefix;
        see test_judge_block_transcript_prefix.
        """
        self.assertEqual(
            classify_tool_error("Bash", "BLOCKED\n\nReason: destructive rm -rf /"),
            "judge-blocked",
        )

    def test_judge_block_transcript_prefix(self):
        self.assertEqual(
            classify_tool_error("Bash", "Error: BLOCKED\n\nReason: destructive rm -rf /"),
            "judge-blocked",
        )

    def test_judge_fail_closed(self):
        """Judge unavailability blocks fail-closed (src/clients/tools/bash.rs)."""
        self.assertEqual(
            classify_tool_error(
                "Bash",
                "BLOCKED\n\nThe command-safety judge was unavailable, so this command "
                "was not executed (fail-closed).\n\nsafety judge timed out after 14s",
            ),
            "judge-fail-closed",
        )

    def test_judge_fail_closed_transcript_prefix(self):
        self.assertEqual(
            classify_tool_error(
                "Bash",
                "Error: BLOCKED\n\nThe command-safety judge was unavailable, so this "
                "command was not executed (fail-closed).\n\nconnection refused",
            ),
            "judge-fail-closed",
        )

    def test_hook_blocked(self):
        """Real hook denials name the hook runner, not a bare BLOCKED.

        This unprefixed form is what session JSONL stores: agent_loop.rs
        writes `Hook blocked tool execution: {reason}` verbatim, with no
        `Error: ` prefix. The prefixed variant below is robustness only.
        """
        self.assertEqual(
            classify_tool_error("Bash", "Hook blocked tool execution: policy denies curl"),
            "hook-blocked",
        )

    def test_hook_blocked_transcript_prefix(self):
        """Prefixed hook denial matches too. No current code path emits it."""
        self.assertEqual(
            classify_tool_error("Edit", "Error: Hook blocked tool execution: policy denies git push"),
            "hook-blocked",
        )

    def test_duplicate_mutation_guard(self):
        """Historical shape: the guard was replaced by per-path serialization,
        but all-time reports still scan transcripts carrying it."""
        output = (
            "Error: Rejected this Edit because another Edit or Write for the same "
            "file was already issued in this assistant turn."
        )
        self.assertEqual(classify_tool_error("Edit", output), "duplicate-mutation guard")

    def test_read_only_path(self):
        self.assertEqual(
            classify_tool_error("Write", "Path '/etc/hosts' is read-only (added via --add-dir)."),
            "read-only path",
        )


if __name__ == "__main__":
    unittest.main()
