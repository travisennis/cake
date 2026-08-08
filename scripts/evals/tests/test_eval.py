#!/usr/bin/env python3
"""Tests for the controlled model evaluation harness.

Uses only the stdlib (unittest) and the fake cake executable; no model
credentials or network access are required. Run with:

  just eval-check
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import eval_lib
import run_eval
from eval_lib import (
    OUTCOME_CAKE_ERROR,
    OUTCOME_CORRECT,
    OUTCOME_HARNESS_ERROR,
    OUTCOME_INCORRECT,
    OUTCOME_PROVIDER_ERROR,
    OUTCOME_TIMEOUT,
)

HERE = Path(__file__).resolve().parent
EVALS_DIR = HERE.parent
FAKE_CAKE = EVALS_DIR / "fake_cake.py"
COMMITTED_CASES_DIR = EVALS_DIR / "cases"
RUN_EVAL = EVALS_DIR / "run_eval.py"

# End state each committed fixture's intended solution produces. Kept here so
# the committed fixtures are exercised end to end: the verifier must fail on
# the initial state (no-op) and pass on this intended solution.
SOLUTIONS = {
    "repository-discovery": {
        "build_command.txt": "make build\n",
    },
    "single-file-edit": {
        "src/config.py": (
            '"""Application configuration."""\n'
            "\n"
            "PORT = 9090\n"
            'HOST = "127.0.0.1"\n'
        ),
    },
    "stale-context-edit": {
        "src/catalog.py": (
            '"""Catalog helpers."""\n'
            "\n"
            "\n"
            "def page_size():\n"
            '    """Number of items shown per page."""\n'
            "    return 50\n"
        ),
    },
    "multi-file-change": {
        "src/lib.py": (
            '"""Shared helpers."""\n'
            "\n"
            "\n"
            "def format_amount(cents):\n"
            '    return f"${cents / 100:.2f}"\n'
        ),
        "src/app.py": (
            '"""Storefront app."""\n'
            "\n"
            "from lib import format_amount\n"
            "\n"
            "\n"
            "def render(price_cents):\n"
            "    return format_amount(price_cents)\n"
        ),
        "src/report.py": (
            '"""Sales report."""\n'
            "\n"
            "from lib import format_amount\n"
            "\n"
            "\n"
            "def total_line(items):\n"
            "    return format_amount(sum(items))\n"
        ),
    },
    "test-driven-correction": {
        "src/validate.py": (
            '"""Input validation helpers."""\n'
            "\n"
            "\n"
            "def is_valid_email(address):\n"
            '    """Return True when address looks like an email address."""\n'
            "    if not isinstance(address, str):\n"
            "        return False\n"
            '    if " " in address:\n'
            "        return False\n"
            '    if address.count("@") != 1:\n'
            "        return False\n"
            '    local, domain = address.split("@")\n'
            '    return bool(local) and "." in domain and bool(domain.split(".", 1)[1])\n'
        ),
    },
}


def run_harness(args: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(RUN_EVAL), *args],
        capture_output=True,
        text=True,
    )


def make_fixture(
    root: Path,
    name: str = "case-a",
    prompt: str = "Make the tests pass",
    verify: str = 'bash "$EVAL_CASE_DIR/verify.sh"',
    verify_script: str = 'grep -q "new" target.txt',
    files: dict[str, str] | None = None,
    timeout_seconds: int = 60,
    tags: tuple[str, ...] = ("edit",),
    expected: str = "correct",
) -> Path:
    """Build a temporary fixture case directory under root."""
    case_dir = root / name
    for rel, content in (files or {"target.txt": "old"}).items():
        path = case_dir / "repo" / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    manifest = {
        "name": name,
        "prompt": prompt,
        "verify": verify,
        "timeout_seconds": timeout_seconds,
        "tags": list(tags),
        "expected": expected,
    }
    (case_dir / "manifest.json").write_text(json.dumps(manifest, indent=2))
    (case_dir / "verify.sh").write_text(verify_script)
    return case_dir


class HarnessEndToEndTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cake-eval-test-")
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def script(self, **overrides) -> Path:
        path = self.root / "fake-script.json"
        path.write_text(json.dumps(overrides))
        return path

    def run_one(self, case_dir: Path, script: Path, results_dir: Path | None = None, *extra):
        os.environ["FAKE_CAKE_SCRIPT"] = str(script)
        return run_harness([
            "--cake", str(FAKE_CAKE),
            "--model", "test-model",
            "--cases-dir", str(case_dir.parent),
            "--cases", case_dir.name,
            "--results-dir", str(results_dir or (self.root / "results")),
            *extra,
        ])

    def load_latest(self, results_dir: Path | None = None) -> dict:
        path = (results_dir or (self.root / "results")) / "latest.json"
        return json.loads(path.read_text())

    def test_correct_outcome(self):
        fixture = make_fixture(self.root / "fixtures")
        script = self.script(
            result="task complete",
            turns=7,
            tool_calls=4,
            tool_failures=1,
            write_files={"target.txt": "new\n"},
            model="test-model",
        )
        proc = self.run_one(fixture, script)
        self.assertEqual(proc.returncode, 0, proc.stderr)

        result = self.load_latest()
        self.assertEqual(result["schema_version"], 1)
        self.assertEqual(result["configuration"]["models"], ["test-model"])
        self.assertEqual(len(result["trials"]), 1)
        trial = result["trials"][0]
        self.assertEqual(trial["outcome"], OUTCOME_CORRECT)
        self.assertEqual(trial["exit_code"], 0)
        self.assertEqual(trial["turns"], 7)
        self.assertEqual(trial["tool_calls"], 4)
        self.assertEqual(trial["tool_failures"], 1)
        self.assertEqual(trial["usage"]["total_tokens"], 150)
        self.assertEqual(trial["model_reported"], "test-model")
        self.assertEqual(trial["cake_elapsed_ms"], 1000)
        self.assertEqual(trial["verifier"]["exit_code"], 0)
        self.assertIn("task complete", trial["result_preview"])
        self.assertIsInstance(trial["session_id"], str)
        self.assertTrue(trial["session_file"].endswith(".jsonl"))
        self.assertIsNotNone(trial["duration_ms"])
        self.assertEqual(
            result["summary"]["by_model"]["test-model"]["correctness_rate"], 1.0
        )
        self.assertIn("test-model", proc.stdout)

    def test_verifier_failure(self):
        fixture = make_fixture(self.root / "fixtures")
        script = self.script(write_files={"target.txt": "old\n"})  # not the required state
        proc = self.run_one(fixture, script)
        self.assertEqual(proc.returncode, 0, proc.stderr)

        trial = self.load_latest()["trials"][0]
        self.assertEqual(trial["outcome"], OUTCOME_INCORRECT)
        self.assertEqual(trial["exit_code"], 0)
        self.assertEqual(trial["verifier"]["exit_code"], 1)

    def test_malformed_completion_json(self):
        fixture = make_fixture(self.root / "fixtures")
        script = self.script(stdout="malformed")
        proc = self.run_one(fixture, script)
        self.assertEqual(proc.returncode, 0, proc.stderr)

        trial = self.load_latest()["trials"][0]
        self.assertEqual(trial["outcome"], OUTCOME_HARNESS_ERROR)
        self.assertIn("malformed completion JSON", trial["error"])

    def test_timeout(self):
        fixture = make_fixture(self.root / "fixtures", timeout_seconds=1)
        script = self.script(sleep_seconds=30)
        proc = self.run_one(fixture, script)
        self.assertEqual(proc.returncode, 0, proc.stderr)

        trial = self.load_latest()["trials"][0]
        self.assertEqual(trial["outcome"], OUTCOME_TIMEOUT)
        self.assertIsNone(trial["exit_code"])
        self.assertIn("timeout", trial["error"])

    def test_exit_code_classification(self):
        for exit_code, expected in (
            (1, OUTCOME_CAKE_ERROR),
            (2, OUTCOME_PROVIDER_ERROR),
            (3, OUTCOME_HARNESS_ERROR),
        ):
            with self.subTest(exit_code=exit_code):
                fixture = make_fixture(self.root / "fixtures")
                script = self.script(exit_code=exit_code)
                proc = self.run_one(fixture, script)
                self.assertEqual(proc.returncode, 0, proc.stderr)
                trial = self.load_latest()["trials"][0]
                self.assertEqual(trial["outcome"], expected)
                self.assertEqual(trial["exit_code"], exit_code)

    def test_repeated_trials(self):
        fixture = make_fixture(self.root / "fixtures")
        record_file = self.root / "invocations.jsonl"
        script = self.script(
            write_files={"target.txt": "new\n"},
            record_file=str(record_file),
        )
        proc = self.run_one(fixture, script, None, "--repetitions", "3")
        self.assertEqual(proc.returncode, 0, proc.stderr)

        trials = self.load_latest()["trials"]
        self.assertEqual(len(trials), 3)
        for trial in trials:
            self.assertEqual(trial["outcome"], OUTCOME_CORRECT)
        self.assertEqual(sorted(t["repetition"] for t in trials), [0, 1, 2])
        self.assertEqual(len({t["session_id"] for t in trials}), 3)

        records = [json.loads(line) for line in record_file.read_text().splitlines()]
        self.assertEqual(len(records), 3)
        prompts = [r["argv"][-1] for r in records]
        self.assertEqual(len(set(prompts)), 1, "prompt must be identical across trials")
        fingerprints = [json.dumps(r["repo_fingerprint"], sort_keys=True) for r in records]
        self.assertEqual(
            len(set(fingerprints)), 1,
            "initial repository state must be identical across trials",
        )
        self.assertEqual(len({r["cwd"] for r in records}), 3, "each trial needs its own repo")

    def test_identical_presentation_across_models(self):
        fixture = make_fixture(self.root / "fixtures")
        record_file = self.root / "invocations.jsonl"
        script = self.script(
            write_files={"target.txt": "new\n"},
            record_file=str(record_file),
        )
        os.environ["FAKE_CAKE_SCRIPT"] = str(script)
        proc = run_harness([
            "--cake", str(FAKE_CAKE),
            "--model", "model-a",
            "--model", "model-b",
            "--cases-dir", str(fixture.parent),
            "--cases", fixture.name,
            "--results-dir", str(self.root / "results"),
        ])
        self.assertEqual(proc.returncode, 0, proc.stderr)

        trials = self.load_latest()["trials"]
        self.assertEqual([t["model"] for t in trials], ["model-a", "model-b"])
        records = [json.loads(line) for line in record_file.read_text().splitlines()]
        prompts = [r["argv"][-1] for r in records]
        self.assertEqual(len(set(prompts)), 1, "prompt must be identical across models")
        fingerprints = [json.dumps(r["repo_fingerprint"], sort_keys=True) for r in records]
        self.assertEqual(
            len(set(fingerprints)), 1,
            "initial repository state must be identical across models",
        )

    def test_cleanup(self):
        fixture = make_fixture(self.root / "fixtures")
        script = self.script(write_files={"target.txt": "new\n"})
        before = set(Path(tempfile.gettempdir()).glob("cake-eval-*"))
        proc = self.run_one(fixture, script)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        after = set(Path(tempfile.gettempdir()).glob("cake-eval-*"))
        self.assertEqual(after, before, "temporary repositories must be cleaned up")

    def test_interrupt_terminates_cake_subprocess(self):
        fixture = make_fixture(self.root / "fixtures", timeout_seconds=300)
        record_file = self.root / "invocations.jsonl"
        script = self.script(sleep_seconds=300, record_file=str(record_file))
        os.environ["FAKE_CAKE_SCRIPT"] = str(script)
        proc = subprocess.Popen(
            [
                sys.executable, str(RUN_EVAL),
                "--cake", str(FAKE_CAKE),
                "--model", "test-model",
                "--cases-dir", str(fixture.parent),
                "--cases", fixture.name,
                "--results-dir", str(self.root / "results"),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            fake_pid = None
            deadline = time.time() + 30
            while time.time() < deadline:
                if record_file.is_file():
                    lines = record_file.read_text().splitlines()
                    if lines:
                        fake_pid = json.loads(lines[-1])["pid"]
                        break
                time.sleep(0.05)
            self.assertIsNotNone(fake_pid, "fake cake never started")
            proc.send_signal(signal.SIGINT)
            stdout, stderr = proc.communicate(timeout=30)
            self.assertEqual(proc.returncode, 130, stderr)
            deadline = time.time() + 10
            alive = True
            while time.time() < deadline:
                try:
                    os.kill(fake_pid, 0)
                except ProcessLookupError:
                    alive = False
                    break
                time.sleep(0.05)
            self.assertFalse(alive, "cake subprocess left alive after interrupt")
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.communicate()

    def test_create_repo_pins_local_git_config(self):
        # The initial commit must not run the developer's global hooks or
        # attempt GPG signing; mirror the repo's fixture-repo convention
        # (src/config/git.rs pins core.hooksPath locally).
        fixture = make_fixture(self.root / "fixtures")
        dst = self.root / "work"
        eval_lib.create_repo(dst, fixture / "repo")
        for key, expected in (
            ("core.hooksPath", "/dev/null"),
            ("commit.gpgSign", "false"),
        ):
            with self.subTest(key=key):
                out = subprocess.run(
                    ["git", "-C", str(dst), "config", "--local", key],
                    capture_output=True,
                    text=True,
                    check=True,
                )
                self.assertEqual(out.stdout.strip(), expected)

    def test_verifier_launch_failure_is_harness_error(self):
        fixture = make_fixture(
            self.root / "fixtures",
            verify='bash "$EVAL_CASE_DIR/missing.sh"',
        )
        script = self.script(write_files={"target.txt": "new\n"})
        proc = self.run_one(fixture, script)
        self.assertEqual(proc.returncode, 0, proc.stderr)

        trial = self.load_latest()["trials"][0]
        self.assertEqual(trial["outcome"], OUTCOME_HARNESS_ERROR)
        self.assertEqual(trial["verifier"]["exit_code"], 127)
        self.assertIn("verifier could not run", trial["error"])


class CommittedFixturesTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cake-eval-test-")
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def cases(self) -> list[eval_lib.Case]:
        return eval_lib.discover_cases([COMMITTED_CASES_DIR])

    def script(self, **overrides) -> Path:
        path = self.root / "fake-script.json"
        path.write_text(json.dumps(overrides))
        return path

    def test_required_fixtures_present_and_valid(self):
        cases = self.cases()
        names = {case.name for case in cases}
        required = {
            "repository-discovery",
            "single-file-edit",
            "stale-context-edit",
            "multi-file-change",
            "test-driven-correction",
        }
        self.assertTrue(required.issubset(names), names)
        self.assertGreaterEqual(len(cases), 5)
        for case in cases:
            with self.subTest(case=case.name):
                self.assertIn(case.expected, eval_lib.OUTCOMES)
                self.assertGreater(case.timeout_seconds, 0)
                self.assertTrue(case.tags)
                self.assertTrue(case.prompt)
                self.assertTrue(case.repo_dir.is_dir())
                self.assertTrue(any(case.repo_dir.iterdir()))
                self.assertTrue((case.directory / "verify.sh").is_file())

    def test_committed_fixtures_noop_is_incorrect(self):
        # A verifier that passes on the untouched initial state is vacuous and
        # would make every run report "correct" regardless of the model.
        for case in self.cases():
            with self.subTest(case=case.name):
                results_dir = self.root / "results" / case.name
                script = self.script()
                os.environ["FAKE_CAKE_SCRIPT"] = str(script)
                proc = run_harness([
                    "--cake", str(FAKE_CAKE),
                    "--model", "test-model",
                    "--cases-dir", str(COMMITTED_CASES_DIR),
                    "--cases", case.name,
                    "--results-dir", str(results_dir),
                ])
                self.assertEqual(proc.returncode, 0, proc.stderr)
                trial = json.loads((results_dir / "latest.json").read_text())["trials"][0]
                self.assertEqual(trial["outcome"], OUTCOME_INCORRECT)

    def test_committed_fixtures_solved_is_correct(self):
        # The intended solution must be achievable and accepted by the
        # verifier; otherwise the fixture cannot discriminate models.
        for case in self.cases():
            with self.subTest(case=case.name):
                self.assertIn(case.name, SOLUTIONS, "add a solution for every committed fixture")
                results_dir = self.root / "results" / case.name
                script = self.script(write_files=SOLUTIONS[case.name])
                os.environ["FAKE_CAKE_SCRIPT"] = str(script)
                proc = run_harness([
                    "--cake", str(FAKE_CAKE),
                    "--model", "test-model",
                    "--cases-dir", str(COMMITTED_CASES_DIR),
                    "--cases", case.name,
                    "--results-dir", str(results_dir),
                ])
                self.assertEqual(proc.returncode, 0, proc.stderr)
                trial = json.loads((results_dir / "latest.json").read_text())["trials"][0]
                self.assertEqual(trial["outcome"], OUTCOME_CORRECT)

    def test_committed_fixtures_tolerate_bytecode_cache(self):
        # A model that runs the tests leaves tests/__pycache__ in the work
        # repo; verifiers must not treat Python bytecode caches as test
        # tampering. The multi-file pyc deliberately contains the old name to
        # prove the grep exclusion.
        test_bearing = {
            "stale-context-edit",
            "multi-file-change",
            "test-driven-correction",
        }
        for case in self.cases():
            if case.name not in test_bearing:
                continue
            with self.subTest(case=case.name):
                writes = dict(SOLUTIONS[case.name])
                writes["tests/__pycache__/stale.cpython-314.pyc"] = (
                    "format_price stale bytecode\x00\x01"
                )
                results_dir = self.root / "results" / case.name
                script = self.script(write_files=writes)
                os.environ["FAKE_CAKE_SCRIPT"] = str(script)
                proc = run_harness([
                    "--cake", str(FAKE_CAKE),
                    "--model", "test-model",
                    "--cases-dir", str(COMMITTED_CASES_DIR),
                    "--cases", case.name,
                    "--results-dir", str(results_dir),
                ])
                self.assertEqual(proc.returncode, 0, proc.stderr)
                trial = json.loads((results_dir / "latest.json").read_text())["trials"][0]
                self.assertEqual(
                    trial["outcome"], OUTCOME_CORRECT,
                    "verifier must ignore __pycache__ left by test runs",
                )


class CaseValidationTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cake-eval-test-")
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def test_load_case_rejects_missing_verifier(self):
        case_dir = self.root / "case-b"
        (case_dir / "repo").mkdir(parents=True)
        (case_dir / "repo" / "f.txt").write_text("x")
        (case_dir / "manifest.json").write_text(json.dumps({
            "name": "case-b",
            "prompt": "do the thing",
            "verify": 'bash "$EVAL_CASE_DIR/verify.sh"',
            "timeout_seconds": 60,
            "tags": ["edit"],
            "expected": "correct",
        }))
        with self.assertRaisesRegex(eval_lib.HarnessError, "verify.sh"):
            eval_lib.load_case(case_dir)


class CliFormattingTest(unittest.TestCase):
    def test_pair_renders_zero_not_dash(self):
        self.assertEqual(run_eval.pair(0.0, 0.0), "0.0/0.0")
        self.assertEqual(run_eval.pair(None, None), "-/-")
        self.assertEqual(run_eval.pair(7.0, 9.0), "7.0/9.0")


if __name__ == "__main__":
    unittest.main()
