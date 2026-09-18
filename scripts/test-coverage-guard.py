#!/usr/bin/env python3
"""Fixture tests for the coverage artifact policy: scripts/coverage-guard.py and
scripts/coverage-clean.sh.

The guard decides whether a coverage run's data can be trusted, so the fixtures
pin both directions: each condition that has produced a false total is rejected,
and the artifacts and reports a healthy run leaves behind are accepted. The
symlinked-root case uses a real symlink, because the guard's duplicate detection
is exactly the difference between a path as written and its canonical form. The
clean-helper fixtures stub `cargo`, because what that helper owns is the policy
around the clean: which directory holds the artifacts, and that nothing survives
the clean and removal. The guard accepts a missing directory, so a helper that
derived the wrong path would pass without checking anything.

Run locally with `just coverage-guard-check` and in CI via the `changes` job in
.github/workflows/ci.yml.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "coverage-guard.py"
CLEAN_SCRIPT = ROOT / "scripts" / "coverage-clean.sh"


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


class CoverageCleanTests(unittest.TestCase):
    """scripts/coverage-clean.sh: clean, remove what the clean left, prove it."""

    def stub_environment(
        self, directory: Path, **overrides: str
    ) -> tuple[dict[str, str], Path]:
        """An environment whose `cargo` is a stub that records the argv it was given."""
        stub_dir = directory / "bin"
        stub_dir.mkdir()
        log = directory / "cargo.log"
        stub = stub_dir / "cargo"
        stub.write_text(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> \"$CARGO_STUB_LOG\"\n",
            encoding="utf-8",
        )
        stub.chmod(0o755)

        environment = dict(os.environ)
        # The directory derivation is exactly what these fixtures pin, so neither
        # variable may leak in from the caller's environment.
        environment.pop("CARGO_LLVM_COV_TARGET_DIR", None)
        environment.pop("CARGO_TARGET_DIR", None)
        environment.update(
            PATH=f"{stub_dir}{os.pathsep}{environment['PATH']}",
            CARGO_STUB_LOG=str(log),
            **overrides,
        )
        return environment, log

    def run_clean(
        self, environment: dict[str, str], cwd: Path = ROOT
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(CLEAN_SCRIPT)],
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    def test_cleans_the_directory_named_by_cargo_llvm_cov_target_dir(self) -> None:
        # cargo-llvm-cov puts the artifacts directly in this directory when it is
        # set, with no nested `llvm-cov-target`, so deriving the default path here
        # would leave the residue in place and pass the guard vacuously.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            coverage_dir = root / "cov"
            coverage_dir.mkdir()
            stale = coverage_dir / "cake-2.profdata"
            stale.write_bytes(b"stale merged profile")
            listed = coverage_dir / "cake-2-profraw-list"
            listed.write_text("/tmp/default-1234.profraw\n", encoding="utf-8")

            environment, log = self.stub_environment(
                root, CARGO_LLVM_COV_TARGET_DIR=str(coverage_dir)
            )
            result = self.run_clean(environment)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(stale.exists())
            self.assertFalse(listed.exists())
            self.assertIn("Removed residual profile artifact", result.stdout)
            self.assertIn("llvm-cov clean --workspace", log.read_text(encoding="utf-8"))

    def test_cleans_the_nested_directory_under_cargo_target_dir(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            nested = target / "llvm-cov-target"
            nested.mkdir(parents=True)
            residue = nested / "cake-2-profraw-list"
            residue.write_text("/tmp/default-1234.profraw\n", encoding="utf-8")
            outside = target / "cake-2.profdata"
            outside.write_bytes(b"not the coverage directory")

            environment, _ = self.stub_environment(root, CARGO_TARGET_DIR=str(target))
            result = self.run_clean(environment)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(residue.exists())
            self.assertTrue(outside.exists())

    def test_cleans_the_default_target_directory_relative_to_the_working_directory(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            work = root / "work"
            nested = work / "target" / "llvm-cov-target"
            nested.mkdir(parents=True)
            stale = nested / "cake-2.profdata"
            stale.write_bytes(b"stale merged profile")
            (work / "scripts").symlink_to(ROOT / "scripts", target_is_directory=True)

            environment, _ = self.stub_environment(root)
            result = self.run_clean(environment, cwd=work)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(stale.exists())


if __name__ == "__main__":
    unittest.main()
