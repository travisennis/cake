#!/usr/bin/env python3
"""Fixture tests for scripts/coverage-guard.py.

The guard decides whether a coverage run's data can be trusted, so the fixtures
pin both directions: each condition that has produced a false total is rejected,
and the artifacts and reports a healthy run leaves behind are accepted. The
symlinked-root case uses a real symlink, because the guard's duplicate detection
is exactly the difference between a path as written and its canonical form.

Run locally with `just coverage-guard-check` and in CI via the `changes` job in
.github/workflows/ci.yml.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "coverage-guard.py"


class CoverageGuardTests(unittest.TestCase):
    def run_guard(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *arguments],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )

    # --- profiles mode -----------------------------------------------------

    def test_missing_profiles_directory_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_guard("--profiles-dir", str(Path(directory) / "llvm-cov-target"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no residual profile data", result.stdout)

    def test_unrelated_build_artifacts_are_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55\n", encoding="utf-8")
            (root / ".rustc_info.json").write_text("{}", encoding="utf-8")
            (root / "debug" / "deps").mkdir(parents=True)
            (root / "debug" / "deps" / "cake-abc").write_bytes(b"binary")
            result = self.run_guard("--profiles-dir", str(root))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no residual profile data", result.stdout)

    def test_stale_merged_profile_data_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "cake-2.profdata").write_bytes(b"stale merged profile")
            result = self.run_guard("--profiles-dir", str(root))
        self.assertEqual(result.returncode, 1)
        self.assertIn("cake-2.profdata", result.stderr)
        self.assertIn("survived the coverage clean", result.stderr)
        self.assertIn("cargo llvm-cov clean --workspace", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_nested_profraw_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "debug" / "deps").mkdir(parents=True)
            (root / "debug" / "deps" / "default-1234.profraw").write_bytes(b"raw")
            result = self.run_guard("--profiles-dir", str(root))
        self.assertEqual(result.returncode, 1)
        self.assertIn("default-1234.profraw", result.stderr)

    def test_residual_profraw_list_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "cake-2-profraw-list").write_text("/tmp/default-1234.profraw\n", encoding="utf-8")
            result = self.run_guard("--profiles-dir", str(root))
        self.assertEqual(result.returncode, 1)
        self.assertIn("cake-2-profraw-list", result.stderr)

    # --- lcov mode ---------------------------------------------------------

    def write_lcov(self, path: Path, sources: list[str]) -> None:
        body = "".join(
            f"SF:{source}\nDA:1,1\nDA:2,0\nend_of_record\n" for source in sources
        )
        path.write_text(body, encoding="utf-8")

    def test_single_root_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lcov = Path(directory) / "lcov.info"
            self.write_lcov(lcov, ["/work/cake/src/main.rs", "/work/cake/src/cli/mod.rs"])
            result = self.run_guard("--lcov", str(lcov))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("2 source file(s) under one root", result.stdout)

    def test_duplicate_source_roots_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real = root / "cake"
            (real / "src").mkdir(parents=True)
            (real / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
            link = root / "cake-link"
            link.symlink_to(real, target_is_directory=True)

            lcov = root / "lcov.info"
            self.write_lcov(
                lcov,
                [str(real / "src" / "main.rs"), str(link / "src" / "main.rs")],
            )
            result = self.run_guard("--lcov", str(lcov))
        self.assertEqual(result.returncode, 1)
        self.assertIn("duplicate source roots", result.stderr)
        self.assertIn(str(real / "src" / "main.rs"), result.stderr)
        self.assertIn(str(link / "src" / "main.rs"), result.stderr)
        self.assertIn("cargo llvm-cov clean --workspace", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_low_coverage_under_one_root_is_left_to_the_threshold(self) -> None:
        # The guard judges provenance, not quality: a genuinely low number must
        # still reach the threshold gate, which is what fails the run.
        with tempfile.TemporaryDirectory() as directory:
            lcov = Path(directory) / "lcov.info"
            self.write_lcov(lcov, ["/work/cake/src/only_partly_covered.rs"])
            result = self.run_guard("--lcov", str(lcov))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_lcov_file_is_an_invocation_error(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_guard("--lcov", str(Path(directory) / "absent.info"))
        self.assertEqual(result.returncode, 2)
        self.assertIn("lcov file not found", result.stderr)

    # --- removal mode ------------------------------------------------------

    def test_removes_only_profile_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            keep = root / ".rustc_info.json"
            keep.write_text("{}", encoding="utf-8")
            stale = root / "cake-2.profdata"
            stale.write_bytes(b"stale merged profile")
            listed = root / "cake-2-profraw-list"
            listed.write_text("/tmp/default-1234.profraw\n", encoding="utf-8")
            nested = root / "debug" / "deps"
            nested.mkdir(parents=True)
            raw = nested / "default-1234.profraw"
            raw.write_bytes(b"raw")

            result = self.run_guard("--profiles-dir", str(root), "--remove-residual")

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stderr, "")
            self.assertEqual(result.stdout.count("Removed residual profile artifact:"), 3)
            self.assertFalse(stale.exists())
            self.assertFalse(listed.exists())
            self.assertFalse(raw.exists())
            self.assertTrue(keep.exists())

    def test_clean_directory_is_accepted_after_removal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "cake-2.profdata").write_bytes(b"stale merged profile")
            first = self.run_guard("--profiles-dir", str(root), "--remove-residual")
            second = self.run_guard("--profiles-dir", str(root), "--remove-residual")
            verified = self.run_guard("--profiles-dir", str(root))
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertIn("no residual profile data", second.stdout)
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_removal_requires_profiles_dir(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lcov = Path(directory) / "lcov.info"
            self.write_lcov(lcov, ["/work/cake/src/main.rs"])
            result = self.run_guard("--lcov", str(lcov), "--remove-residual")
        self.assertEqual(result.returncode, 2)
        self.assertIn("--remove-residual applies to --profiles-dir", result.stderr)

    # --- invocation --------------------------------------------------------

    def test_requires_exactly_one_mode(self) -> None:
        for arguments in ((), ("--profiles-dir", "target", "--lcov", "lcov.info")):
            with self.subTest(arguments=arguments):
                result = self.run_guard(*arguments)
                self.assertEqual(result.returncode, 2)
                self.assertIn("exactly one of --profiles-dir or --lcov is required", result.stderr)


if __name__ == "__main__":
    unittest.main()
