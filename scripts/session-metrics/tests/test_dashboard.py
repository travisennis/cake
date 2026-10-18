"""Dashboard regression tests: temporal attribution, retention and safe embedding."""
import argparse
import contextlib
import io
import json
from datetime import datetime, timezone
from pathlib import Path
import plistlib
import sys
import tempfile
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

    def test_launch_jobs_preserve_spaces_and_pin_explicit_roots(self):
        jobs = install_dashboard.jobs(Path('/tmp/with spaces'), Path('/usr/bin/python3'), Path('/data/sessions'), Path('/cache'))
        for job in jobs:
            self.assertEqual(plistlib.loads(plistlib.dumps(job)), job)
        self.assertEqual(jobs[0]['ProgramArguments'][1], '/tmp/with spaces/runtime/dashboard.py')
        self.assertEqual(jobs[1]['StartCalendarInterval'], dict(Weekday=1, Hour=9, Minute=0))

    def test_missing_source_does_not_replace_dashboard(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'index.html').write_text('previous')
            ns = argparse.Namespace(output=root, sessions_dir=root / 'missing')
            with self.assertRaises(SystemExit):
                dashboard.generate(ns)
            self.assertEqual((root / 'index.html').read_text(), 'previous')
