"""Dashboard regression tests: temporal attribution, retention and safe embedding."""
import argparse
import contextlib
import io
import json
from datetime import datetime, timezone
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import cakelib
import dashboard
import install_dashboard


def ts(day):
    return datetime.fromisoformat(day + 'T00:00:00+00:00')


def dataset(sessions=(), invocations=()):
    return cakelib.Dataset(list(sessions), list(invocations), Path('/sessions'), Path('/telemetry'), None)


class DashboardTests(unittest.TestCase):
    def test_task_start_wins_over_completion_timestamp_and_mtime(self):
        session = cakelib.Session('s', Path('s'), 0, ts('2026-10-18'), [
            dict(type='task_start', task_id='a', timestamp='2026-10-10T23:59:00Z'),
            dict(type='task_complete', task_id='a', timestamp='2026-10-11T00:01:00Z',
                 subtype='success', duration_ms=120000),
            dict(type='task_start', task_id='b', timestamp='2026-10-11T01:00:00Z')])
        rows = dashboard.aggregate(dataset([session]), ts('2026-10-10'), ts('2026-10-11'))
        self.assertEqual([(r['day'], r['tasks'], r['success'], r['incomplete']) for r in rows],
                         [('2026-10-10', 1, 1, 0)])
        rows = dashboard.aggregate(dataset([session]), ts('2026-10-11'), ts('2026-10-12'))
        self.assertEqual((rows[0]['tasks'], rows[0]['incomplete']), (0, 1))

    def test_resumed_invocation_events_keep_their_day_and_model(self):
        inv = cakelib.Invocation('s', 'i', init=dict(model='new', timestamp='2026-10-10T00:00:00Z'))
        inv.attempts = [dict(timestamp='2026-10-11T00:00:00Z', status_code=503, request_ms=30,
                             usage=dict(input_tokens=100, output_tokens=5,
                                        input_tokens_details=dict(cached_tokens=80))),
                        dict(timestamp='2026-10-11T01:00:00Z', status_code=200,
                             usage=dict(input_tokens=200))]
        inv.judge_attempts = [dict(timestamp='2026-10-11T00:00:00Z', total_ms=42)]
        inv.tool_calls = [dict(timestamp='2026-10-11T00:00:00Z', was_error=True)]
        row, = dashboard.aggregate(dataset(invocations=[inv]), ts('2026-10-11'), ts('2026-10-12'))
        self.assertEqual((row['invocations'], row['attempts'], row['api_errors'], row['model']), (0, 2, 1, 'new'))
        self.assertEqual((row['input_tokens'], row['cache_reported_input'], row['cached_tokens']), (300, 100, 80))
        self.assertEqual((row['judge_ms'], row['tool_errors']), (42, 1))

    def test_edit_write_recovery_is_attributed_to_failed_call_day(self):
        session = cakelib.Session('s', Path('s'), 0, ts('2026-10-18'), [])
        session._tool_calls = [
            cakelib.ToolCall(1, 'Edit', 'a', '{"path":"/a"}', 'Error', False, '2026-10-10T23:59:00Z'),
            cakelib.ToolCall(2, 'Write', 'b', '{"path":"/a"}', 'ok', True, '2026-10-11T00:01:00Z')]
        rows = dashboard.aggregate(dataset([session]), ts('2026-10-10'), ts('2026-10-12'))
        self.assertEqual((rows[0]['day'], rows[0]['edit_write_retried'], rows[0]['edit_write_recovered']),
                         ('2026-10-10', 1, 1))
        rows = dashboard.aggregate(dataset([session]), ts('2026-10-10'), ts('2026-10-11'))
        self.assertEqual(rows[0]['edit_write_retried'], 0)

    def test_refresh_replaces_recent_history_preserves_old_and_escapes_script(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'sessions').mkdir()
            output = root / 'output'
            (output / 'history').mkdir(parents=True)
            (output / 'history/2026-01-01.json').write_text('[]')
            inv = cakelib.Invocation('s', 'i', init=dict(model='</script><script>evil', timestamp='2026-10-17T00:00:00Z'))
            ns = argparse.Namespace(output=output, sessions_dir=root / 'sessions', telemetry_dir=root / 'telemetry', days=0)
            with patch.object(cakelib, 'load', return_value=dataset(invocations=[inv])), contextlib.redirect_stdout(io.StringIO()):
                dashboard.generate(ns, ts('2026-10-18'))
                dashboard.generate(ns, ts('2026-10-18'))
            self.assertTrue((output / 'history/2026-01-01.json').exists())
            self.assertEqual(len(json.loads((output / 'metrics.json').read_text())['rows']), 1)
            self.assertNotIn('</script><script>evil', (output / 'index.html').read_text())
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)

    def test_launch_jobs_use_shell_wrapper_and_python_fallbacks(self):
        jobs = install_dashboard.jobs(
            Path('/tmp/with spaces'), Path('/opt/homebrew/opt/python@3.15/bin/python3.15'),
            Path('/data/sessions'), Path('/cache'))
        for job in jobs:
            self.assertEqual(plistlib.loads(plistlib.dumps(job)), job)
        script = '/tmp/with spaces/runtime/metrics_job.sh'
        self.assertEqual(jobs[0]['ProgramArguments'][:6],
                         ['/bin/sh', script, 'refresh', '/tmp/with spaces', '/data/sessions', '/cache'])
        self.assertEqual(jobs[0]['ProgramArguments'][6:],
                         ['/opt/homebrew/opt/python@3.15/bin/python3.15',
                          '/opt/homebrew/bin/python3', '/usr/local/bin/python3', '/usr/bin/python3'])
        self.assertEqual(jobs[1]['ProgramArguments'],
                         ['/bin/sh', script, 'remind', '/tmp/with spaces'])
        self.assertEqual(jobs[1]['StartCalendarInterval'], dict(Weekday=1, Hour=9, Minute=0))

    def test_python_candidates_dedupe_the_installer_interpreter(self):
        self.assertEqual(install_dashboard.python_candidates(Path('/opt/homebrew/bin/python3')),
                         ['/opt/homebrew/bin/python3', '/usr/local/bin/python3', '/usr/bin/python3'])

    def job(self, *args, env=None):
        script = Path(install_dashboard.__file__).parent / 'metrics_job.sh'
        return subprocess.run(['/bin/sh', str(script), *map(str, args)],
                              capture_output=True, text=True, env=env)

    def test_job_script_skips_unusable_python_and_logs_success(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            root = temp / 'root'
            root.mkdir()
            marker = temp / 'ran'
            broken = temp / 'broken'
            broken.write_text('#!/bin/sh\nexit 1\n')
            broken.chmod(0o755)
            good = temp / 'good'
            good.write_text('#!/bin/sh\nif [ "$1" = "-c" ]; then exit 0; fi\n'
                            'printf "%s\\n" "$@" > "$MARKER"\n')
            good.chmod(0o755)
            env = dict(os.environ, MARKER=str(marker), CAKE_METRICS_NO_NOTIFY='1')
            result = self.job('refresh', root, '/sessions', '/telemetry', broken, good, env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(marker.exists())
            self.assertIn('refresh succeeded via', (root / 'refresh.log').read_text())

    def test_job_script_reports_missing_python(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            broken = root / 'broken'
            broken.write_text('#!/bin/sh\nexit 1\n')
            broken.chmod(0o755)
            env = dict(os.environ, CAKE_METRICS_NO_NOTIFY='1')
            result = self.job('refresh', root, '/sessions', '/telemetry', broken, env=env)
            self.assertEqual(result.returncode, 78)
            self.assertIn('no Python 3.10+ interpreter found',
                          (root / 'refresh.log').read_text())

    def test_reminder_flags_stale_and_missing_reports(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            env = dict(os.environ, CAKE_METRICS_NO_NOTIFY='1')
            self.assertEqual(self.job('remind', root, env=env).returncode, 0)
            self.assertIn('missing', (root / 'reminder.log').read_text())
            (root / 'metrics.json').write_text('{}')
            old = time.time() - 37 * 3600
            os.utime(root / 'metrics.json', (old, old))
            self.assertEqual(self.job('remind', root, env=env).returncode, 0)
            self.assertIn('stale', (root / 'reminder.log').read_text())

    def test_missing_source_does_not_replace_dashboard(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'index.html').write_text('previous')
            ns = argparse.Namespace(output=root, sessions_dir=root / 'missing')
            with self.assertRaises(SystemExit):
                dashboard.generate(ns)
            self.assertEqual((root / 'index.html').read_text(), 'previous')
