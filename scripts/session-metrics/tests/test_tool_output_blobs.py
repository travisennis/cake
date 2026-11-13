#!/usr/bin/env python3
"""Unit tests for resolving externalized tool outputs in session metrics.

Run with `just session-metrics-check` or:
  python3 -m unittest discover -s scripts/session-metrics/tests -v
"""

import hashlib
import json
import os
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cakelib


NOW = datetime(2026, 11, 13, 12, 0, 0, tzinfo=timezone.utc)


def content_id(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def records_for(content_id: str, output: str = "", name: str = "Bash") -> list[dict]:
    return [
        {
            "type": "function_call",
            "call_id": "call-1",
            "name": name,
            "arguments": json.dumps({"command": "fd ."}),
        },
        {
            "type": "function_call_output",
            "call_id": "call-1",
            "output": output,
            "content_id": content_id,
        },
    ]


def session(root: Path, records: list[dict]) -> cakelib.Session:
    return cakelib.Session(
        id="s",
        path=root / "s.jsonl",
        size=0,
        mtime=NOW,
        records=records,
    )


def write_blob(root: Path, digest: str, data: bytes) -> None:
    blob_dir = root / "s.blobs"
    blob_dir.mkdir(exist_ok=True)
    (blob_dir / digest).write_bytes(data)


class ExternalizedOutputTest(unittest.TestCase):
    def test_failure_is_classified_from_blob_text(self):
        text = "Error: timed out after 30s\n" + "output detail\n" * 2_000
        data = text.encode("utf-8")
        digest = content_id(data)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_blob(root, digest, data)
            calls = session(root, records_for(digest)).tool_calls

        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0].output, text)
        self.assertFalse(calls[0].ok)
        self.assertEqual(
            cakelib.classify_tool_error(calls[0].name, calls[0].output),
            "timeout",
        )

    def test_externalized_bytes_count_toward_output_size(self):
        text = "é" * 10_000
        data = text.encode("utf-8")
        digest = content_id(data)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_blob(root, digest, data)
            calls = session(root, records_for(digest)).tool_calls

        self.assertEqual(len(calls[0].output), 10_000)
        self.assertEqual(len(calls[0].output.encode("utf-8")), len(data))

    def test_missing_blob_is_a_visible_failure(self):
        digest = content_id(b"missing")
        with tempfile.TemporaryDirectory() as temp:
            calls = session(Path(temp), records_for(digest)).tool_calls

        self.assertEqual(len(calls), 1)
        self.assertIn(cakelib.EXTERNALIZED_OUTPUT_UNAVAILABLE, calls[0].output)
        self.assertIn("missing blob", calls[0].output)
        self.assertFalse(calls[0].ok)
        self.assertEqual(
            cakelib.classify_tool_error(calls[0].name, calls[0].output),
            "externalized-output-unavailable",
        )

    def test_corrupt_blob_is_a_visible_failure(self):
        digest = content_id(b"expected payload")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_blob(root, digest, b"different payload")
            calls = session(root, records_for(digest)).tool_calls

        self.assertIn("digest mismatch", calls[0].output)
        self.assertFalse(calls[0].ok)

    def test_non_utf8_blob_is_a_visible_failure(self):
        data = b"\xff\xfe"
        digest = content_id(data)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_blob(root, digest, data)
            calls = session(root, records_for(digest)).tool_calls

        self.assertIn("not UTF-8", calls[0].output)
        self.assertFalse(calls[0].ok)

    def test_malformed_content_id_cannot_escape_the_blob_directory(self):
        digest = "../" + "a" * 61
        with tempfile.TemporaryDirectory() as temp:
            calls = session(Path(temp), records_for(digest)).tool_calls

        self.assertIn("invalid content id", calls[0].output)
        self.assertFalse(calls[0].ok)

    def test_inline_output_keeps_its_current_path(self):
        digest = content_id(b"external bytes")
        with tempfile.TemporaryDirectory() as temp:
            calls = session(
                Path(temp), records_for(digest, output="inline text")
            ).tool_calls

        self.assertEqual(calls[0].output, "inline text")
        self.assertTrue(calls[0].ok)


if __name__ == "__main__":
    unittest.main()
