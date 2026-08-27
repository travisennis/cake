#!/usr/bin/env python3
"""Fixture tests for scripts/dependency-sweep.py."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "dependency-sweep.py"


class DependencySweepTests(unittest.TestCase):
    def run_report(self, *arguments: str, root: Path | None = None) -> tuple[subprocess.CompletedProcess[str], dict]:
        command = [sys.executable, str(SCRIPT), *arguments]
        if root is not None:
            command.extend(["--root", str(root)])
        result = subprocess.run(
            command,
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.stderr, "")
        return result, json.loads(result.stdout)

    def channel(self, version: str, published: str) -> tempfile.NamedTemporaryFile[str]:
        channel = tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False)
        channel.write(f'date = "{published}"\n\n[pkg.rust]\nversion = "{version} (fixture)"\n')
        channel.close()
        self.addCleanup(Path(channel.name).unlink, missing_ok=True)
        return channel

    def rust_fixture(self) -> Path:
        temporary = tempfile.TemporaryDirectory(prefix="dependency-sweep-repo-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / ".github" / "workflows").mkdir(parents=True)
        (root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.97.1"\n', encoding="utf-8")
        (root / ".mise.toml").write_text(
            '[tools]\nrust = { version = "1.97.1" }\njust = "1.58.0"\nsccache = "0.17.0"\n',
            encoding="utf-8",
        )
        (root / "Cargo.toml").write_text(
            '[package]\nname = "cake"\nversion = "0.1.0"\n\n[dependencies]\nanyhow = "1.0.103"\n',
            encoding="utf-8",
        )
        (root / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "cake"\nversion = "0.1.0"\n',
            encoding="utf-8",
        )
        (root / "justfile").write_text("setup:\n    cargo install cargo-deny --version 0.20.2 --locked\n", encoding="utf-8")
        (root / ".github" / "workflows" / "ci.yml").write_text(
            "jobs:\n  test:\n    steps:\n      - uses: actions/checkout@v7\n      - uses: dtolnay/rust-toolchain@stable\n        with:\n          toolchain: 1.97.1\n      - uses: taiki-e/install-action@v2\n        with:\n          tool: cargo-deny@0.20.2\n",
            encoding="utf-8",
        )
        return root

    def test_inventory_assigns_owners_and_authorities(self) -> None:
        result, report = self.run_report()

        self.assertEqual(result.returncode, 20)
        self.assertEqual(report["status"], "review-required")
        self.assertTrue(any(record["code"] == "rust-release-unavailable" for record in report["findings"]))
        ids = {record["id"] for record in report["inventory"]}
        self.assertIn("cargo:lockfile", ids)
        self.assertIn("cargo:dependencies:anyhow", ids)
        self.assertIn("cargo:target.cfg(target_os = \"linux\").dependencies:landlock", ids)
        self.assertIn("mise:rust", ids)
        self.assertTrue(any(record["name"] == "cargo-deny" and record["path"] == "justfile" for record in report["inventory"]))
        self.assertTrue(any(record["name"] == "cargo-udeps" and record["path"] == ".github/workflows/scheduled.yml" for record in report["inventory"]))
        self.assertTrue(any(record["name"] == "actions/checkout" and record["path"] == ".github/workflows/ci.yml" for record in report["inventory"]))
        mapping_actions = {
            (record["name"], record["path"])
            for record in report["inventory"]
            if record["id"].startswith("action:")
        }
        self.assertIn(("codecov/codecov-action", ".github/workflows/ci.yml"), mapping_actions)
        self.assertIn(("actions/upload-artifact", ".github/workflows/release.yml"), mapping_actions)
        self.assertIn(("softprops/action-gh-release", ".github/workflows/release.yml"), mapping_actions)
        nightly = next(record for record in report["inventory"] if record["name"] == "dtolnay/rust-toolchain" and record["current"] == "nightly")
        self.assertEqual(nightly["owner"], "human decision and review")
        self.assertEqual(nightly["exception"], "cargo-udeps requires a compatible nightly")
        self.assertIn("rust-toolchain", ids)
        msrv = next(record for record in report["inventory"] if record["name"] == "msrv toolchain")
        self.assertEqual(msrv["owner"], "human decision and review")
        self.assertEqual(msrv["relation"], "msrv")
        self.assertTrue(all(record["owner"] and record["authority"] and record["current"] for record in report["inventory"]))

    def test_same_rust_release_is_no_work(self) -> None:
        root = self.rust_fixture()
        channel = self.channel("1.97.1", "2026-08-20")
        result, report = self.run_report("--rust-channel", channel.name, "--as-of", "2026-08-27", root=root)

        self.assertEqual(result.returncode, 0)
        self.assertEqual(report["status"], "no-work")
        self.assertEqual(report["findings"], [])
        self.assertEqual(report["rust_release"]["age_days"], 7)

    def test_current_toolchain_update_is_actionable_after_cooldown(self) -> None:
        root = self.rust_fixture()
        channel = self.channel("1.98.0", "2026-08-20")
        result, report = self.run_report("--rust-channel", channel.name, "--as-of", "2026-08-27", root=root)

        self.assertEqual(result.returncode, 10)
        self.assertEqual(report["status"], "actionable")
        update = report["findings"][0]
        self.assertEqual(update["code"], "rust-toolchain-update")
        self.assertEqual(update["current"], "1.97.1")
        self.assertEqual(update["candidate"], "1.98.0")
        self.assertEqual(update["age_days"], 7)

    def test_cooldown_fails_closed(self) -> None:
        root = self.rust_fixture()
        channel = self.channel("1.98.0", "2026-08-25")
        result, report = self.run_report("--rust-channel", channel.name, "--as-of", "2026-08-27", root=root)

        self.assertEqual(result.returncode, 20)
        self.assertEqual(report["status"], "review-required")
        self.assertEqual(report["findings"][0]["code"], "rust-release-cooldown")
        self.assertEqual(report["findings"][0]["classification"], "review-required")

    def test_major_rust_update_requires_human_review(self) -> None:
        root = self.rust_fixture()
        channel = self.channel("2.0.0", "2026-08-20")
        result, report = self.run_report("--rust-channel", channel.name, "--as-of", "2026-08-27", root=root)

        self.assertEqual(result.returncode, 20)
        self.assertEqual(report["status"], "review-required")
        self.assertEqual(report["findings"][0]["code"], "rust-major-update")

    def test_pin_drift_is_review_required(self) -> None:
        root = self.rust_fixture()
        (root / ".mise.toml").write_text(
            '[tools]\nrust = { version = "1.96.0" }\njust = "1.58.0"\nsccache = "0.17.0"\n',
            encoding="utf-8",
        )
        (root / "justfile").write_text("setup:\n    cargo install cargo-deny --version 0.20.1 --locked\n", encoding="utf-8")

        result, report = self.run_report(root=root)

        self.assertEqual(result.returncode, 20)
        self.assertEqual(report["status"], "review-required")
        codes = {record["code"] for record in report["findings"]}
        self.assertIn("rust-pin-drift", codes)
        self.assertIn("tool-version-drift", codes)

    def test_security_reason_exempts_cooldown(self) -> None:
        root = self.rust_fixture()
        channel = self.channel("1.98.0", "2026-08-25")
        result, report = self.run_report(
            "--rust-channel",
            channel.name,
            "--as-of",
            "2026-08-27",
            "--security-reason",
            "RustSec advisory fixes a confirmed vulnerability",
            root=root,
        )

        self.assertEqual(result.returncode, 10)
        self.assertEqual(report["status"], "actionable")
        self.assertEqual(report["findings"][0]["security_reason"], "RustSec advisory fixes a confirmed vulnerability")

    def test_ambiguous_release_is_review_required(self) -> None:
        root = self.rust_fixture()
        channel = tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False)
        channel.write('date = "not-a-date"\n\n[pkg.rust]\nversion = "unknown"\n')
        channel.close()
        self.addCleanup(Path(channel.name).unlink, missing_ok=True)

        result, report = self.run_report("--rust-channel", channel.name, "--as-of", "2026-08-27", root=root)

        self.assertEqual(result.returncode, 20)
        self.assertEqual(report["status"], "review-required")
        self.assertEqual(report["findings"][0]["code"], "rust-release-invalid")


if __name__ == "__main__":
    unittest.main()
