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

    def test_sandbox_blocked(self):
        self.assertEqual(
            classify_tool_error(
                "Bash",
                "Operation not permitted\n\n[Sandbox restriction]: This command was blocked by the filesystem sandbox.",
            ),
            "sandbox-blocked",
        )


class SandboxNoticeScopingTest(unittest.TestCase):
    """The notice is a line shape, not a tool name (#561).

    `bash.rs` appends it as its own line; the bare marker also reaches the
    transcript as file content, diff text, and search output, so neither the
    marker anywhere in an output nor a truncated copy of the notice is a denial.
    """

    NOTICE = (
        "ls: /outside: Operation not permitted\n\n"
        "[Sandbox restriction]: This command was blocked by the filesystem sandbox."
    )

    def test_bash_notice_is_sandbox_blocked(self):
        self.assertEqual(classify_tool_error("Bash", self.NOTICE), "sandbox-blocked")

    def test_relayed_notice_is_sandbox_blocked(self):
        """A relay can carry a subagent's denied call verbatim, under any name."""
        for name in ("Bash", "tb__delegate"):
            with self.subTest(tool=name):
                self.assertEqual(
                    classify_tool_error(name, self.NOTICE), "sandbox-blocked"
                )

    def test_quoting_the_notice_keeps_the_tools_own_category(self):
        """Read and Edit prefix every line, so neither can emit a notice line."""
        cases = [
            # A Read failure whose body quotes the marker. Read numbers lines.
            ("Read",
             "Error: Failed to read file '/repo/src/clients/tools/bash.rs': invalid "
             "UTF-8\n\n  1209: [Sandbox restriction]: This command was blocked by "
             "the filesystem sandbox.",
             "path/file access"),
            # Edit's success output is a diff; a failing Edit can quote the same
            # diff text. A diff line is prefixed `+`, `-`, or a space.
            ("Edit",
             "Error: Edit 1 of 1 failed in /repo/src/clients/tools/bash.rs: could not "
             "find the exact text to replace.\n+            [Sandbox restriction]: This "
             "command was blocked by the filesystem sandbox.",
             "no-match (old_text not found)"),
        ]
        for name, output, expected in cases:
            with self.subTest(tool=name):
                self.assertEqual(classify_tool_error(name, output), expected)

    def test_bash_grep_quoting_the_notice_is_not_sandbox_blocked(self):
        """A Bash search that prints the marker mid-line is not a denial."""
        self.assertEqual(
            classify_tool_error(
                "Bash",
                "Error: rg exited 1\n\n"
                "src/clients/tools/bash.rs:1209:            [Sandbox restriction]: This "
                "command was blocked by the filesystem sandbox.",
            ),
            "other",
        )

    def test_truncated_copy_of_the_notice_is_not_sandbox_blocked(self):
        """Corpus shape: a probe that prints the notice cut at 72 characters.

        The line starts with the marker but not with the notice's sentence, and
        the call dumped transcript text rather than hitting a denial.
        """
        self.assertEqual(
            classify_tool_error(
                "Bash",
                "=== 20ea1003-5a45-4267-a303-b8f50497ed32 ===\n0\n"
                "--- tool errors/blocked in outputs ---\n"
                "call_00_x :: bash: /outside/pr199.diff: Operation not permitted\n"
                "[Sandbox restriction]: This command was blocked by the filesystem "
                "sandbo\n",
            ),
            "other",
        )


class ReadOnlyPathScopingTest(unittest.TestCase):
    """`read-only path` is a write-validation denial, not any `read-only` text (#561)."""

    def test_edit_existing_path_denial(self):
        """`validate_path_for_write` (`mod.rs`), reached by Edit and Write."""
        self.assertEqual(
            classify_tool_error(
                "Edit",
                "Error: Path '/etc/hosts' is read-only (added via --add-dir). "
                "Write operations are not allowed.",
            ),
            "read-only path",
        )

    def test_write_new_file_denial(self):
        """`resolve_path_for_write_scheduling` (`mod.rs`), the new-file branch."""
        self.assertEqual(
            classify_tool_error(
                "Write",
                "Error: Path '/Users/u/Library/LaunchAgents/x.plist' is in a "
                "read-only directory (added via --add-dir). Write operations are "
                "not allowed.",
            ),
            "read-only path",
        )

    def test_quoting_the_message_is_not_read_only_path(self):
        """Text that quotes the message keeps its own tool's bucket.

        Read and Bash print it mid-line; an Edit no-match failure embeds it in
        the nearest-match hint, which `edit.rs` builds from raw file lines —
        `mod.rs` itself now holds the message, so the hint can quote it.
        """
        cases = [
            ("Bash",
             "Error: rg exited 1\n\nsrc/clients/tools/mod.rs:1053: Path '{}' is "
             "read-only (added via --add-dir). Write operations are not allowed.",
             "other"),
            ("Read",
             "Error: Failed to read file '/repo/mod.rs': invalid UTF-8\n\n  1053: "
             "Path '/x' is read-only (added via --add-dir). Write operations are "
             "not allowed.",
             "path/file access"),
            ("Edit",
             "Error: Edit 1 of 1 failed in /repo/src/clients/tools/mod.rs: could "
             "not find the exact text to replace. The old_text must match exactly, "
             "including all whitespace and newlines.\nNearest matching context in "
             "file:\n>  1053 |             \"Path '{}' is read-only (added via "
             "--add-dir). Write operations are not allowed.\",\n    1054 | "
             "            validated.canonical.display()\n",
             "no-match (old_text not found)"),
        ]
        for name, output, expected in cases:
            with self.subTest(tool=name):
                self.assertEqual(classify_tool_error(name, output), expected)

    def test_bare_read_only_word_is_not_read_only_path(self):
        """Corpus shape: an Edit failure whose nearest-match context quotes `read-only`."""
        self.assertEqual(
            classify_tool_error(
                "Edit",
                "Error: Edit 1 of 1 failed in /repo/DESIRES.md: could not find the "
                "exact text to replace. The old_text must match exactly, including "
                "all whitespace and newlines.\nNearest matching context in "
                "/repo/DESIRES.md: - Read-only mode skips toolbox discovery",
            ),
            "no-match (old_text not found)",
        )


if __name__ == "__main__":
    unittest.main()
