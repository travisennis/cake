#!/usr/bin/env python3
"""Unit tests for ToolCall.ok, including hook denials (issue #338).

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import contextlib
import io
import json
import os
import sys
import unittest
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib
import tools


# cake's own notice first line (`bash.rs` `compose_text_output`). Fixtures must
# use the emitted shape: `is_tool_failure` matches this line, not the bare
# `[Sandbox restriction]` marker (issue #561).
NOTICE_LINE = (
    "[Sandbox restriction]: This command was blocked by the filesystem sandbox."
)


def records_for(outputs: list[str], name: str = "Bash") -> list[dict]:
    records: list[dict] = []
    for i, output in enumerate(outputs):
        call_id = f"call-{i}"
        records.append({
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": json.dumps({"command": "fd ."}),
        })
        records.append({
            "type": "function_call_output",
            "call_id": call_id,
            "output": output,
        })
    return records


def session(outputs: list[str]) -> cakelib.Session:
    return cakelib.Session(
        id="s",
        path=None,
        size=0,
        mtime=datetime.now(timezone.utc),
        records=records_for(outputs),
    )


class ToolCallOkTest(unittest.TestCase):
    def test_hook_denial_is_failure(self):
        """Hook denials are stored unprefixed, so the `Error:` gate misses them."""
        calls = cakelib.pair_tool_calls(
            records_for(["Hook blocked tool execution: use fd instead of find"])
        )
        first = calls[0].output.splitlines()[0]
        self.assertTrue(first.startswith("Hook blocked tool execution"))
        self.assertFalse(calls[0].ok)

    def test_prefixed_hook_denial_is_failure(self):
        """The prefixed variant also fails; no code path emits it today."""
        calls = cakelib.pair_tool_calls(
            records_for(["Error: Hook blocked tool execution: policy denies curl"])
        )
        self.assertFalse(calls[0].ok)

    def test_error_prefix_is_failure(self):
        calls = cakelib.pair_tool_calls(records_for(["Error: no such file"]))
        self.assertFalse(calls[0].ok)

    def test_write_validation_denials_are_failures(self):
        """Both write-validation messages reach the model as `Error:` lines.

        No message-specific rule is needed for the gate; the `read-only path`
        category is a taxonomy concern, covered in `test_tool_taxonomy.py`.
        These samples pin that a denial is recorded as an `Error:` first line
        (issue #561).
        """
        calls = cakelib.pair_tool_calls(
            records_for([
                "Error: Path '/etc/hosts' is read-only (added via --add-dir). "
                "Write operations are not allowed.",
            ], name="Edit")
            + records_for([
                "Error: Path '/u/Library/LaunchAgents/x.plist' is in a read-only "
                "directory (added via --add-dir). Write operations are not allowed.",
            ], name="Write")
        )
        self.assertFalse(calls[0].ok)
        self.assertFalse(calls[1].ok)

    def test_success_is_not_failure(self):
        calls = cakelib.pair_tool_calls(records_for(["fd .\n./src\n"]))
        self.assertTrue(calls[0].ok)

    def test_sandbox_denial_is_failure(self):
        calls = cakelib.pair_tool_calls(records_for([
            "Operation not permitted\n\n" + NOTICE_LINE,
        ]))
        self.assertFalse(calls[0].ok)

    def test_synthetic_not_executed_outputs_are_not_failures(self):
        """Boundary: the two synthetic `not executed:` shapes stay successes.

        History repair (`agent_state.rs`) and a correction turn
        (`agent_loop.rs`) append these without `Error:` or a hook prefix, so
        the gate passes them; the README caveat records the exclusion. Flipping
        it is a deliberate change to the taxonomy numbers, not a bug fix.
        """
        calls = cakelib.pair_tool_calls(records_for([
            "not executed: the previous cake process ended before Bash(call-1) "
            "recorded a result",
            "not executed: correction turn offers no tools for Bash(call-2)",
        ]))
        self.assertTrue(all(c.ok for c in calls), [c.output for c in calls])


class SandboxNoticeScopingTest(unittest.TestCase):
    """The notice counts on its shape, not on the tool that carries it (#561).

    Read prefixes line numbers and Edit embeds a unified diff, so a file that
    quotes `[Sandbox restriction]` reaches the transcript as quoted text. A
    failed call is still a failure on its own tool's terms.
    """

    def test_read_quoting_the_marker_is_not_a_failure(self):
        calls = cakelib.pair_tool_calls(records_for([
            "File: /repo/src/clients/tools/bash.rs\nLines 1209-1209/1400\n"
            "  1209:             [Sandbox restriction]: This command was blocked by "
            "the filesystem sandbox.\\\n"
        ], name="Read"))
        self.assertTrue(calls[0].ok)

    def test_edit_diff_quoting_the_marker_is_not_a_failure(self):
        calls = cakelib.pair_tool_calls(records_for([
            "Edited /repo/src/clients/tools/bash.rs (1 replacement)\n"
            "--- a/src/clients/tools/bash.rs\n+++ b/src/clients/tools/bash.rs\n"
            "@@ -1208,3 +1208,3 @@\n"
            "             [Sandbox restriction]: This command was blocked by the "
            "filesystem sandbox.\\\n"
            "+            [Sandbox restriction]: This command was blocked by the "
            "filesystem sandbox.\\\n"
        ], name="Edit"))
        self.assertTrue(calls[0].ok)

    def test_bash_quoting_the_marker_mid_line_is_not_a_failure(self):
        """A search that prints the marker is not a denial: no line starts with it."""
        calls = cakelib.pair_tool_calls(records_for([
            "src/clients/tools/bash.rs:1209:            [Sandbox restriction]: This "
            "command was blocked by the filesystem sandbox.\n"
        ]))
        self.assertTrue(calls[0].ok)

    def test_relayed_denial_is_a_failure(self):
        """A relay can carry a subagent's denied call verbatim, under any name."""
        calls = cakelib.pair_tool_calls(records_for([
            "Found one denial:\n\n"
            "ls: /outside: Operation not permitted\n\n"
            + NOTICE_LINE
            + "\n"
        ], name="tb__delegate"))
        self.assertFalse(calls[0].ok)

    def test_truncated_notice_copy_is_not_a_failure(self):
        """A truncated copy of the notice is not the notice (corpus shape).

        A probe dumped transcript text and cut each match at 72 characters, so
        the marker began a line without the notice's sentence.
        """
        calls = cakelib.pair_tool_calls(records_for([
            "=== 20ea1003-5a45-4267-a303-b8f50497ed32 ===\n0\n"
            "--- tool errors/blocked in outputs ---\n"
            "call_00_x :: bash: /outside/pr199.diff: Operation not permitted\n"
            "[Sandbox restriction]: This command was blocked by the filesystem "
            "sandbo\n"
        ]))
        self.assertTrue(calls[0].ok)

    def test_relayed_report_quoting_the_marker_is_not_a_failure(self):
        """Corpus shape: a subagent report that names the marker in prose."""
        calls = cakelib.pair_tool_calls(records_for([
            "- **Impact**: Denial UX corruption. A user command echoing "
            "`sandbox-exec: sandbox_apply` on stdout suppresses a real "
            "`[Sandbox restriction]: This command was blocked by the filesystem "
            "sandbox.` notice\n"
        ], name="tb__delegate"))
        self.assertTrue(calls[0].ok)


class HookDenialTaxonomyTest(unittest.TestCase):
    def test_taxonomy_reports_hook_blocked(self):
        data = cakelib.Dataset(
            sessions=[session(["Hook blocked tool execution: use rg instead of grep"])],
            invocations=[],
            sessions_dir=None,
            telemetry_dir=None,
            cutoff=None,
        )
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            tools.run(data)
        output = buf.getvalue()
        taxonomy = output.split("Failure taxonomy:", 1)[1]
        self.assertIn("hook-blocked", taxonomy)

    def test_taxonomy_reports_sandbox_blocked(self):
        data = cakelib.Dataset(
            sessions=[session([
                "Operation not permitted\n\n" + NOTICE_LINE,
            ])],
            invocations=[],
            sessions_dir=None,
            telemetry_dir=None,
            cutoff=None,
        )
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            tools.run(data)
        output = buf.getvalue()
        taxonomy = output.split("Failure taxonomy:", 1)[1]
        self.assertIn("sandbox-blocked", taxonomy)


if __name__ == "__main__":
    unittest.main()
