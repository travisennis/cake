#!/usr/bin/env python3
"""Fixture tests for scripts/binary-size-baseline.py."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "binary-size-baseline.py"


class BinarySizeBaselineTests(unittest.TestCase):
    def write_fake_toolchain(self, root: Path) -> None:
        bin_dir = root / "bin"
        bin_dir.mkdir()
        (bin_dir / "rustc").write_text(
            "#!/bin/sh\n"
            'if [ "$1" = "-vV" ]; then printf \'host: aarch64-apple-darwin\\n\'; '
            "else printf 'rustc fixture\\n'; fi\n",
            encoding="utf-8",
        )
        (bin_dir / "cargo").write_text("#!/bin/sh\nprintf 'cargo fixture\\n'\n", encoding="utf-8")
        for tool in (bin_dir / "rustc", bin_dir / "cargo"):
            tool.chmod(0o755)

    def run_script(self, root: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
        self.write_fake_toolchain(root)
        environment = os.environ.copy()
        environment["PATH"] = f"{root / 'bin'}{os.pathsep}{environment['PATH']}"
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(root), *arguments],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    def test_writes_host_record_without_a_release_build(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "cake"
            output = root / "baseline.json"
            artifact.write_bytes(b"native fixture")

            result = self.run_script(
                root,
                "--artifact",
                str(artifact),
                "--output",
                str(output),
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            document = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(len(document["baselines"]), 1)
            record = next(iter(document["baselines"].values()))
            self.assertEqual(record["size_bytes"], len(b"native fixture"))
            self.assertEqual(record["artifact"], "cake")

    def test_preserves_other_targets_and_uses_explicit_cross_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "cake-linux"
            output = root / "baseline.json"
            artifact.write_bytes(b"cross-target fixture")
            output.write_text(
                json.dumps(
                    {
                        "format_version": 1,
                        "package": "cake",
                        "profile": "release",
                        "baselines": {
                            "aarch64-apple-darwin": {
                                "toolchain": {"rustc": "test", "cargo": "test"},
                                "artifact": "target/release/cake",
                                "size_bytes": 456,
                            }
                        },
                    }
                ),
                encoding="utf-8",
            )

            result = self.run_script(
                root,
                "--target",
                "x86_64-unknown-linux-gnu",
                "--artifact",
                str(artifact),
                "--output",
                str(output),
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            document = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(document["baselines"]["aarch64-apple-darwin"]["size_bytes"], 456)
            self.assertEqual(
                document["baselines"]["x86_64-unknown-linux-gnu"]["size_bytes"],
                len(b"cross-target fixture"),
            )
            self.assertEqual(
                document["baselines"]["x86_64-unknown-linux-gnu"]["artifact"],
                "cake-linux",
            )

    def test_rejects_malformed_baseline_without_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "cake"
            output = root / "baseline.json"
            artifact.write_bytes(b"fixture")
            output.write_text("[]", encoding="utf-8")

            result = self.run_script(
                root,
                "--artifact",
                str(artifact),
                "--output",
                str(output),
            )

            self.assertEqual(result.returncode, 1)
            self.assertIn("binary-size baseline must be a JSON object", result.stderr)
            self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
