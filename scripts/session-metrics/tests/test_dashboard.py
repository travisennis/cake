"""Dashboard regression tests: temporal attribution, retention and safe embedding."""
import argparse
import contextlib
import io
import json
from datetime import datetime, timedelta, timezone
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
import dashboard_render
import dashboard_report
import install_dashboard
import summary


def ts(day):
    return datetime.fromisoformat(day + 'T00:00:00+00:00')


def dataset(sessions=(), invocations=()):
    return cakelib.Dataset(list(sessions), list(invocations), Path('/sessions'), Path('/telemetry'), None)


def rich_dataset(now=None):
    """Twelve sessions across models, projects, versions, tools, and builds."""
    now = now or ts('2026-10-19')
    sessions, invocations = [], []
    for i in range(12):
        sid = f'session-{i:02d}'
        moment = (now - timedelta(days=(i % 14) + 1)).isoformat()
        tool = f'Tool{i:02d}'
        model = f'model-{i % 3}'
        project = f'/projects/project-{i % 4}'
        commit = f'{i // 6 + 1:040x}'
        dirty = i % 2 == 0
        edit_output = ('Error: could not find the exact text to replace'
                       if i % 2 == 0 else 'ok')
        records = [
            dict(type='session_meta', timestamp=moment, model=model,
                 working_directory=project, cake_version=f'1.0.{i % 2}',
                 build_commit=commit, build_dirty=dirty),
            dict(type='task_start', task_id=f'task-{i}', timestamp=moment),
            dict(type='task_complete', task_id=f'task-{i}',
                 subtype='success' if i % 2 == 0 else 'error_during_execution',
                 duration_ms=1000 + i, num_turns=2 + i, tool_call_count=3),
            dict(type='function_call', name='Bash', call_id=f'bash-{i}',
                 arguments='{"reason":"inspect"}', timestamp=moment),
            dict(type='function_call_output', call_id=f'bash-{i}', output='ok'),
            dict(type='function_call', name=tool, call_id=f'call-{i}',
                 arguments='{"path":"/tmp/x"}', timestamp=moment),
            dict(type='function_call_output', call_id=f'call-{i}',
                 output=f'Error: sentinel error {i}'),
            dict(type='function_call', name='Edit', call_id=f'edit-{i}',
                 arguments='{"path":"/tmp/e"}', timestamp=moment),
            dict(type='function_call_output', call_id=f'edit-{i}', output=edit_output),
            dict(type='hook_event', event='PreToolUse',
                 decision='deny' if i % 3 == 0 else 'allow',
                 resolved_decision='deny' if i % 3 == 0 else 'allow',
                 tool_name=tool, tool_input_summary=f'input-{i}',
                 command=f'command-{i}', exit_code=0, duration_ms=5 + i,
                 timestamp=moment),
            dict(type='skill_activated', name=f'skill-{i % 4}',
                 path=f'/skills/{i}/SKILL.md', timestamp=moment),
        ]
        sessions.append(cakelib.Session(
            sid, Path('/sessions') / f'{sid}.jsonl', 1000 + i, now, records,
            model=model, cake_version=f'1.0.{i % 2}', build_commit=commit,
            build_dirty=dirty, working_directory=project))
        inv = cakelib.Invocation(sid, f'inv-{i}', init=dict(
            model=model, working_directory=project, timestamp=moment, mode='new',
            api_type='responses', output_format='text', build_commit=commit,
            build_dirty=dirty))
        inv.mtime = now
        inv.attempts = [dict(
            timestamp=moment, status_code=500 + i, error=f'api error {i}',
            request_ms=100 + i, total_ms=200 + i, turn_index=i, attempt=1,
            usage=dict(input_tokens=100 + i, output_tokens=10,
                       input_tokens_details=dict(cached_tokens=80),
                       total_tokens=110 + i))]
        inv.retries = [dict(timestamp=moment, reason=f'reason-{i % 4}',
                            delay_ms=10 + i, attempt=1, max_retries=3,
                            changed_request_overrides=False)]
        inv.gate_startups = [dict(gate=f'gate-{i % 4}', mode='enforce',
                                  outcome='ready', timestamp=moment)]
        inv.gate_decisions = [dict(gate=f'gate-{i % 4}', mode='enforce',
                                   decision='block' if i % 5 == 0 else 'allow',
                                   latency_ms=10 + i, timestamp=moment)]
        inv.gate_evidence = [dict(gate=f'gate-{i % 4}', kind='attempt', timestamp=moment)]
        inv.tool_calls = [dict(name=tool, duration_ms=50 + i, output_bytes=100 + i,
                               was_error=True, timestamp=moment, call_id='0' * 64,
                               turn_index=i, attempt=1)]
        inv.judge_attempts = [dict(
            timestamp=moment, terminal_class='verdict' if i % 2 == 0 else 'timeout',
            total_ms=30 + i, call_id='0' * 64)]
        inv.compensations = [dict(kind='judge_verdict', detail='block:test',
                                  timestamp=moment, latency_ms=30 + i)]
        inv.summary = dict(timestamp=moment, success=(i % 2 == 0),
                           duration_ms=1000 + i, turn_count=2 + i,
                           usage=dict(input_tokens=100 + i, output_tokens=10,
                                      total_tokens=110 + i))
        invocations.append(inv)
    return dataset(sessions, invocations)


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
        inv.gate_decisions = [dict(timestamp='2026-10-11T00:00:00Z', latency_ms=7)]
        inv.tool_calls = [dict(timestamp='2026-10-11T00:00:00Z', was_error=True)]
        row, = dashboard.aggregate(dataset(invocations=[inv]), ts('2026-10-11'), ts('2026-10-12'))
        self.assertEqual((row['invocations'], row['attempts'], row['api_errors'], row['model']), (0, 2, 1, 'new'))
        self.assertEqual((row['input_tokens'], row['cache_reported_input'], row['cached_tokens']), (300, 100, 80))
        self.assertEqual((row['judge_ms'], row['gate_ms'], row['tool_errors']), (42, 7, 1))

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

    def test_runtime_copy_includes_every_dashboard_module(self):
        source = Path(install_dashboard.__file__).resolve().parent
        for name in install_dashboard.RUNTIME_FILES:
            self.assertTrue((source / name).exists(), name)

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

    def test_build_identity_loads_from_transcript_and_telemetry(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sessions_dir = root / 'sessions'
            telemetry_dir = root / 'telemetry'
            sessions_dir.mkdir()
            telemetry_dir.mkdir()
            session_id = '550e8400-e29b-41d4-a716-446655440000'
            transcript = [
                dict(type='session_meta', format_version=4, session_id=session_id,
                     timestamp='2026-10-18T00:00:00Z', working_directory='/work',
                     model='alpha', tools=[], cake_version='1.0.0',
                     build_commit='a' * 40, build_dirty=False),
                dict(type='task_start', task_id='t1',
                     timestamp='2026-10-18T00:00:00Z'),
            ]
            (sessions_dir / f'{session_id}.jsonl').write_text(
                '\n'.join(json.dumps(record) for record in transcript) + '\n')
            sidecar = [
                dict(type='telemetry_init', session_id=session_id, invocation_id='i1',
                     timestamp='2026-10-18T00:00:00Z', mode='new', model='alpha',
                     api_type='responses', output_format='text', tools=[], settings={},
                     build_commit='b' * 40, build_dirty=True),
                dict(type='telemetry_init', session_id=session_id, invocation_id='i2',
                     timestamp='2026-10-18T01:00:00Z', mode='resume', model='alpha',
                     api_type='responses', output_format='text', tools=[], settings={}),
                dict(type='telemetry_init', session_id=session_id, invocation_id='i3',
                     timestamp='2026-10-18T02:00:00Z', mode='resume', model='alpha',
                     api_type='responses', output_format='text', tools=[], settings={},
                     build_commit='c' * 40),
            ]
            (telemetry_dir / f'{session_id}.ndjson').write_text(
                '\n'.join(json.dumps(record) for record in sidecar) + '\n')
            ns = argparse.Namespace(days=0, sessions_dir=sessions_dir,
                                    telemetry_dir=telemetry_dir, model=None,
                                    project=None, session=None, invocation=None)
            data = cakelib.load(ns)
        self.assertEqual(data.sessions[0].build_identity, 'a' * 40)
        self.assertEqual(data.sessions[0].cake_version, '1.0.0')
        by_id = {inv.invocation_id: inv for inv in data.invocations}
        self.assertEqual(by_id['i1'].build_identity, 'b' * 40 + ' + dirty')
        self.assertEqual(by_id['i2'].build_commit, 'a' * 40)
        self.assertEqual(by_id['i2'].build_identity, 'a' * 40)
        self.assertEqual(by_id['i3'].build_identity, 'c' * 40)

    def test_change_attribution_uses_the_invocation_interval(self):
        now = ts('2026-10-19')
        session = cakelib.Session(
            's', Path('s'), 0, now,
            [dict(type='task_start', task_id='old', timestamp='2026-10-18T09:00:00Z'),
             dict(type='task_complete', task_id='old', subtype='success'),
             dict(type='task_start', task_id='new', timestamp='2026-10-18T10:45:00Z'),
             dict(type='task_complete', task_id='new', subtype='success')],
            model='alpha', cake_version='1.0.0', build_commit='a' * 40,
            build_dirty=False, working_directory='/work')
        inv = cakelib.Invocation('s', 'i', init=dict(
            model='alpha', working_directory='/work', timestamp='2026-10-18T10:30:00Z',
            build_commit='b' * 40, build_dirty=False))
        inv.mtime = now
        inv.summary = {'timestamp': '2026-10-18T11:00:00Z', 'success': True,
                       'duration_ms': 1000, 'turn_count': 1}
        rows = dashboard_report.change_attribution(
            dataset([session], [inv]), ts('2026-10-18'), ts('2026-10-19'))
        by_identity = {row['identity']: row for row in rows}
        self.assertEqual(by_identity['a' * 40]['tasks'], 1)
        self.assertEqual(by_identity['b' * 40]['tasks'], 1)

    def test_report_has_every_section_and_keeps_all_rows(self):
        now = ts('2026-10-19')
        data = rich_dataset(now)
        report = dashboard_report.build_report(
            data, now - timedelta(days=31), now + timedelta(days=1), now)
        ids = {section['id'] for section in report['sections']}
        self.assertEqual(ids, {
            'provenance', 'headline', 'attribution', 'outcomes', 'tools', 'api',
            'tokens', 'cache', 'gates', 'judge', 'time', 'hooks',
            'compensations', 'sessions', 'definitions',
        })
        for section in report['sections']:
            for table in section['tables']:
                for row in table['rows']:
                    self.assertEqual(len(row), len(table['headers']), table['title'])
        text = '\n'.join(
            [section['title'] for section in report['sections']]
            + [str(cell) for section in report['sections']
               for table in section['tables'] for row in table['rows'] for cell in row])
        for sentinel in [
            'Tool00', 'Tool11', 'Edit', 'api error 0', 'api error 11', '500', '511',
            'gate-3', 'command-11', 'session-11', 'skill-3', 'other', 'no event',
            'TOTAL', 'Data dictionary',
        ]:
            self.assertIn(sentinel, text)
        tools_section = next(section for section in report['sections']
                             if section['id'] == 'tools')
        per_tool = tools_section['tables'][0]
        tool_rows = {row[0] for row in per_tool['rows']}
        self.assertTrue({f'Tool{i:02d}' for i in range(12)} <= tool_rows)
        api_section = next(section for section in report['sections']
                           if section['id'] == 'api')
        status_table = next(table for table in api_section['tables']
                            if table['title'] == 'Status codes')
        statuses = {row[0] for row in status_table['rows']}
        self.assertTrue({500 + i for i in range(12)} <= statuses)

    def test_headline_facts_match_summary(self):
        now = ts('2026-10-19')
        data = rich_dataset(now)
        end = now.replace(hour=0, minute=0, second=0, microsecond=0)
        start = end - timedelta(days=7)
        facts = dashboard_report._window_facts(data, start, end)
        doc = summary.summarize(data, start, end, measured=end)
        self.assertEqual(facts['tasks'], doc['counts']['tasks'])
        self.assertEqual(facts['api_attempts'], doc['counts']['api_attempts'])
        self.assertEqual(facts['gate_decisions'], doc['counts']['gate_decisions'])
        self.assertEqual(facts['judge_attempts'], doc['counts']['judge_attempts'])
        self.assertAlmostEqual(facts['success'] / facts['tasks'],
                               doc['metrics']['outcomes.task_success_rate'])
        self.assertAlmostEqual(facts['abnormal'] / facts['invocations'],
                               doc['metrics']['outcomes.abnormal_termination_rate'])
        self.assertAlmostEqual(facts['api_failures'] / facts['api_attempts'],
                               doc['metrics']['api.failure_rate'])
        self.assertAlmostEqual(cakelib.percentile(facts['api_ms'], 95),
                               doc['metrics']['api.latency.p95'])
        self.assertAlmostEqual(facts['cache_input'] and facts['cached_input'] / facts['cache_input'],
                               doc['metrics']['tokens.cache_hit_rate'])
        self.assertAlmostEqual(facts['edit_failures'] / facts['edit_calls'],
                               doc['metrics']['tools.edit.failure_rate'])
        self.assertAlmostEqual(facts['edit_no_match'] / facts['edit_calls'],
                               doc['metrics']['tools.edit.no_match_rate'])
        self.assertAlmostEqual(facts['judge_timeouts'] / facts['judge_attempts'],
                               doc['metrics']['judge.timeout_rate'])
        self.assertAlmostEqual(facts['judge_failures'] / facts['judge_attempts'],
                               doc['metrics']['judge.failure_rate'])
        self.assertAlmostEqual(facts['judge_blocks'] / facts['judge_verdicts'],
                               doc['metrics']['judge.block_rate'])
        self.assertAlmostEqual(facts['gate_failures'] / facts['gate_denominator'],
                               doc['metrics']['gates.failure_rate'])
        self.assertAlmostEqual(facts['gate_blocks'] / facts['gate_judged'],
                               doc['metrics']['gates.block_rate'])

    def test_generate_writes_detail_and_filter_pages(self):
        now = ts('2026-10-19')
        data = rich_dataset(now)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'sessions').mkdir()
            output = root / 'output'
            ns = argparse.Namespace(output=output, sessions_dir=root / 'sessions',
                                    telemetry_dir=root / 'telemetry', days=0)
            with patch.object(cakelib, 'load', return_value=data):
                dashboard.generate(ns, now)
            index = (output / 'index.html').read_text()
            self.assertIn('Change attribution', index)
            self.assertIn('Every API attempt', index)
            self.assertIn('Data dictionary', index)
            self.assertIn('Tool11', index)
            self.assertTrue(list(output.glob('model-*.html')))
            self.assertTrue(list(output.glob('project-*.html')))
            self.assertTrue(list(output.glob('version-*.html')))
            self.assertTrue(any('Filtered to model model-0' in path.read_text()
                                for path in output.glob('model-*.html')))
            payload = json.loads((output / 'metrics.json').read_text())
            self.assertEqual(payload['schema_version'], 2)
            self.assertTrue(payload['attribution'])
            self.assertTrue(payload['rows'])

    def test_empty_report_renders_every_section(self):
        now = ts('2026-10-19')
        report = dashboard_report.build_report(
            dataset(), now - timedelta(days=31), now, now)
        self.assertEqual(len(report['sections']), 15)
        html = dashboard_render.render_page(
            title='Cake metrics', subtitle='empty', nav='',
            sections=report['sections'], footer='fixture')
        self.assertIn('(no data)', html)
        self.assertNotIn('<script', html)

    def test_render_escapes_labels_and_does_not_rescan_placeholders(self):
        now = ts('2026-10-19')
        session = cakelib.Session(
            's', Path('s'), 0, now,
            [dict(type='session_meta', timestamp='2026-10-18T00:00:00Z',
                  model='</script><script>evil __BODY__',
                  working_directory='__TITLE__', cake_version='1.0.0')],
            model='</script><script>evil __BODY__',
            working_directory='__TITLE__', cake_version='1.0.0')
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'sessions').mkdir()
            ns = argparse.Namespace(output=root / 'output',
                                    sessions_dir=root / 'sessions',
                                    telemetry_dir=root / 'telemetry', days=0)
            with patch.object(cakelib, 'load', return_value=dataset([session])):
                dashboard.generate(ns, now)
            html = (root / 'output' / 'index.html').read_text()
        self.assertNotIn('<script>evil', html)
        self.assertIn('&lt;/script&gt;', html)
        self.assertIn('__TITLE__', html)
