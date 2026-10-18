#!/usr/bin/env python3
"""Generate a private, offline dashboard from local cake sessions."""
import argparse
import json
import os
import tempfile
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path

import cakelib

DEFAULT_OUTPUT = Path.home() / 'Library/Application Support/cake-metrics'


def bucket(rows, day, model, project):
    key = (day, model, project)
    if key not in rows:
        rows[key] = dict(day=day, model=model, project=project, tasks=0, success=0,
                         interrupted=0, incomplete=0, invocations=0, summaries=0,
                         tools=0, tool_errors=0, attempts=0, api_errors=0,
                         retries=0, input_tokens=0, output_tokens=0, cached_tokens=0,
                         cache_reported_input=0, judge_ms=0, wall_ms=0,
                         edit_write_retried=0, edit_write_recovered=0,
                         task_ms=[], api_ms=[], sessions=[], covered_sessions=[])
    return rows[key]


def aggregate(data, start, end):
    """Half-open UTC calendar window; task outcomes belong to task start day."""
    rows = {}

    def get(ts, fallback, model, project):
        stamp = cakelib.parse_ts(ts) or fallback
        if stamp is None or not start <= stamp < end:
            return None
        return bucket(rows, stamp.date().isoformat(), model, project)

    for session in data.sessions:
        starts = session._task_start_times()
        for task in session.tasks_in_window(start):
            stamp = starts.get(task.get('task_id')) if task.get('type') == 'task_complete' else None
            row = get(stamp.isoformat() if stamp else task.get('timestamp'), session.mtime, session.model, session.working_directory)
            if row is None:
                continue
            row['tasks'] += 1
            row['success'] += task.get('subtype') == 'success'
            row['interrupted'] += task.get('subtype') == 'interrupted'
            if task.get('duration_ms') is not None:
                row['task_ms'].append(task['duration_ms'])
            row['sessions'].append(session.id)
        calls = session.tool_calls
        for i, call in enumerate(calls):
            if call.name not in ('Edit', 'Write') or call.ok or not call.path:
                continue
            row = get(call.timestamp, session.mtime, session.model, session.working_directory)
            if row is None:
                continue
            later = [c for c in calls[i + 1:] if c.name in ('Edit', 'Write')
                     and c.path == call.path
                     and (cakelib.parse_ts(c.timestamp) or session.mtime) < end]
            if later:
                row['edit_write_retried'] += 1
                row['edit_write_recovered'] += any(c.ok for c in later)
        if session.inflight:
            task = session.final_task_start()
            row = get(task.get('timestamp'), session.mtime, session.model, session.working_directory)
            if row is not None:
                row['incomplete'] += 1
                row['sessions'].append(session.id)

    for inv in data.invocations:
        # Count invocation starts once, rather than once per active day.
        row = get((inv.init or {}).get('timestamp'), inv.mtime, inv.model, inv.working_directory)
        if row is not None:
            row['invocations'] += 1
        groups = [('attempts', inv.attempts), ('tools', inv.tool_calls),
                  ('retries', inv.retries), ('judge', inv.judge_attempts)]
        if inv.summary:
            groups.append(('summary', [inv.summary]))
        for kind, records in groups:
            for record in records:
                row = get(record.get('timestamp'), inv.mtime, inv.model, inv.working_directory)
                if row is None:
                    continue
                row['covered_sessions'].append(inv.session_id)
                if kind == 'attempts':
                    row['attempts'] += 1
                    row['api_errors'] += bool(record.get('error') or record.get('status_code') not in (200, None))
                    if record.get('request_ms') is not None:
                        row['api_ms'].append(record['request_ms'])
                    usage = record.get('usage') or {}
                    inp = usage.get('input_tokens') or usage.get('prompt_tokens') or 0
                    row['input_tokens'] += inp
                    row['output_tokens'] += usage.get('output_tokens') or usage.get('completion_tokens') or 0
                    cached = (usage.get('input_tokens_details') or {}).get('cached_tokens')
                    if cached is not None:
                        row['cached_tokens'] += cached
                        row['cache_reported_input'] += inp
                elif kind == 'tools':
                    row['tools'] += 1
                    row['tool_errors'] += bool(record.get('was_error'))
                elif kind == 'retries':
                    row['retries'] += 1
                elif kind == 'judge':
                    row['judge_ms'] += record.get('total_ms') or 0
                else:
                    row['summaries'] += 1
                    row['wall_ms'] += record.get('duration_ms') or 0
    for row in rows.values():
        for key in ('sessions', 'covered_sessions'):
            row[key] = sorted(set(row[key]))
    return sorted(rows.values(), key=lambda r: (r['day'], r['model'], r['project']))


def atomic_write(path, content):
    with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=path.parent, delete=False) as stream:
        temp = Path(stream.name)
        try:
            stream.write(content)
            stream.flush()
            temp.replace(path)
        finally:
            temp.unlink(missing_ok=True)


def generate(ns, now=None):
    now = now or datetime.now(timezone.utc)
    # Today stays visible, but week comparisons use complete UTC days.
    end = now.replace(hour=0, minute=0, second=0, microsecond=0) + timedelta(days=1)
    start = end - timedelta(days=31)
    if not cakelib.resolve_sessions_dir(ns).is_dir():
        raise SystemExit('Session directory does not exist; dashboard was not replaced.')
    ns.days = 0
    data = cakelib.load(ns)
    rows = aggregate(data, start, end)
    output = ns.output.expanduser().resolve()
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(output, 0o700)
    history = output / 'history'
    history.mkdir(exist_ok=True, mode=0o700)
    by_day = defaultdict(list)
    for row in rows:
        by_day[row['day']].append(row)
    # Replace recent dates so resumes and late completions do not double count.
    for offset in range(31):
        day = (start + timedelta(days=offset)).date().isoformat()
        atomic_write(history / (day + '.json'), json.dumps(by_day[day]))
    retained = []
    for path in sorted(history.glob('*.json')):
        retained.extend(json.loads(path.read_text()))
    payload = dict(schema_version=1, measured=now.isoformat(), today=now.date().isoformat(),
                   session_parse_errors=data.session_parse_errors,
                   telemetry_parse_errors=data.telemetry_parse_errors,
                   telemetry_available=data.telemetry_dir.is_dir(),
                   newest_activity=max((s.last_activity.isoformat() for s in data.sessions), default=None),
                   rows=retained)
    atomic_write(output / 'metrics.json', json.dumps(payload))
    template = Path(__file__).with_name('dashboard.html').read_text()
    # Prevent local project/model names from terminating the script element.
    encoded = json.dumps(payload).replace('<', '\\u003c').replace('>', '\\u003e').replace('&', '\\u0026')
    atomic_write(output / 'index.html', template.replace('__METRICS__', encoded))
    print(output / 'index.html')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sessions-dir', type=Path)
    parser.add_argument('--telemetry-dir', type=Path)
    parser.add_argument('--output', type=Path, default=DEFAULT_OUTPUT)
    parser.set_defaults(days=0, model=None, project=None, session=None, invocation=None)
    generate(parser.parse_args())
