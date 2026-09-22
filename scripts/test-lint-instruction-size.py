#!/usr/bin/env python3
"""Regression tests for the instruction-size report and its optional skill catalog."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import ModuleType
import unittest


SCRIPT = Path(__file__).resolve().with_name("lint-instruction-size.py")


def load_module() -> ModuleType:
    """Import the hyphenated report script as a module."""
    spec = importlib.util.spec_from_file_location("lint_instruction_size", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SkillCatalogLaunchTests(unittest.TestCase):
    def run_report(self, binary: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--skill-catalog", str(binary)],
            capture_output=True, text=True, check=False,
        )

    def assert_launch_error(self, binary: Path) -> None:
        result = self.run_report(binary)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"ERROR: could not run skill catalog binary '{binary}'", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_missing_binary_reports_a_concise_error(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            self.assert_launch_error(Path(directory) / "missing-cake")

    @unittest.skipUnless(os.name == "posix", "requires POSIX executable permissions")
    def test_non_executable_binary_reports_a_concise_error(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "cake"
            binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            binary.chmod(0o600)
            self.assert_launch_error(binary)

    @unittest.skipUnless(os.name == "posix", "requires a POSIX shell fixture")
    def test_child_exit_status_is_preserved(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "cake"
            binary.write_text("#!/bin/sh\nexit 7\n", encoding="utf-8")
            binary.chmod(0o700)
            result = self.run_report(binary)
            self.assertEqual(result.returncode, 7, result.stderr)
            self.assertEqual(result.stderr, "")


class PromptAssetTests(unittest.TestCase):
    def test_reports_prompt_text_and_excludes_snapshots(self) -> None:
        module = load_module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src/clients/tools").mkdir(parents=True)
            (root / "src/prompts/snapshots").mkdir(parents=True)
            (root / "src/clients/tools/read-description.txt").write_text(
                "Read a file.\n", encoding="utf-8"
            )
            (root / "src/clients/tools/bash.rs").write_text("", encoding="utf-8")
            (root / "src/prompts/system.md").write_text(
                "You are cake.\n", encoding="utf-8"
            )
            (root / "src/prompts/snapshots/prompt.snap").write_text(
                "generated\n", encoding="utf-8"
            )

            self.assertEqual(
                module.find_prompt_assets(str(root)),
                ["src/clients/tools/read-description.txt", "src/prompts/system.md"],
            )


if __name__ == "__main__":
    unittest.main()
