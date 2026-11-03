#!/usr/bin/env python3
"""Generate a private, offline, full-detail dashboard from local cake sessions."""
import argparse
import hashlib
import json
import os
import re
import tempfile
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path

import cakelib
import dashboard_render
import dashboard_report

DEFAULT_OUTPUT = Path.home() / 'Library/Application Support/cake-metrics'
SCHEMA_VERSION = 2


def bucket(rows, day, model, project):
    key = (day, model, project)
    if key not in rows:
        rows[key] = dict(day=day, model=model, project=project, tasks=0, success=0,
                         interrupted=0, incomplete=0, invocations=0, summaries=0,
                         tools=0, tool_errors=0, attempts=0, api_errors=0,
                         retries=0, input_tokens=0, output_tokens=0, cached_tokens=0,
                         cache_reported_input=0, judge_ms=0, gate_ms=0, wall_ms=0,
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
                  ('retries', inv.retries), ('judge', inv.judge_attempts),
                  ('gates', inv.gate_decisions)]
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
                elif kind == 'gates':
                    row['gate_ms'] += record.get('latency_ms') or 0
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


def slug(value):
    """A filesystem-safe page name that keeps colliding labels distinct."""
    cleaned = re.sub(r'[^A-Za-z0-9._-]+', '-', value).strip('-').lstrip('.')[:60] or 'value'
    digest = hashlib.sha256(value.encode('utf-8')).hexdigest()[:8]
    return f"{cleaned}-{digest}"


def _session_version(session):
    return session.cake_version or 'unknown'


def _invocation_version(inv):
    return inv.cake_version or 'unknown'


def filter_values(data):
    models = sorted({s.model for s in data.sessions if s.model and s.model != 'unknown'}
                    | {inv.model for inv in data.invocations if inv.model and inv.model != 'unknown'})
    projects = sorted({s.working_directory for s in data.sessions
                       if s.working_directory and s.working_directory != 'unknown'}
                      | {inv.working_directory for inv in data.invocations
                         if inv.working_directory and inv.working_directory != 'unknown'})
    versions = sorted({_session_version(s) for s in data.sessions} - {'unknown'}
                      | {_invocation_version(inv) for inv in data.invocations} - {'unknown'})
    return {'model': models, 'project': projects, 'version': versions}


def filter_dataset(data, *, model=None, project=None, version=None):
    sessions = data.sessions
    invocations = data.invocations
    if model:
        sessions = [s for s in sessions if s.model == model]
        invocations = [inv for inv in invocations if inv.model == model]
    if project:
        sessions = [s for s in sessions if s.working_directory == project]
        invocations = [inv for inv in invocations if inv.working_directory == project]
    if version:
        session_ids = {inv.session_id for inv in data.invocations
                       if _invocation_version(inv) == version}
        sessions = [s for s in sessions
                    if _session_version(s) == version or s.id in session_ids]
        invocations = [inv for inv in invocations if _invocation_version(inv) == version]
    return cakelib.Dataset(
        sessions=list(sessions), invocations=list(invocations),
        sessions_dir=data.sessions_dir, telemetry_dir=data.telemetry_dir,
        cutoff=data.cutoff, session_parse_errors=data.session_parse_errors,
        telemetry_parse_errors=data.telemetry_parse_errors,
        selected_invocation=data.selected_invocation,
        invocation_filter=data.invocation_filter,
    )


def nav_groups(values, current):
    groups = [{'label': 'View', 'links': [
        {'label': 'All data', 'href': 'index.html'},
    ]}]
    for key, label in (('model', 'Models'), ('project', 'Projects'), ('version', 'Versions')):
        links = [{'label': value, 'href': f'{key}-{slug(value)}.html'}
                 for value in values.get(key) or []]
        if links:
            groups.append({'label': label, 'links': links})
    for group in groups:
        for link in group['links']:
            link['active'] = link['href'] == current
    return groups


def _page(data, href, title, subtitle, values, start, end, now):
    report = dashboard_report.build_report(data, start, end, now)
    return dashboard_render.render_page(
        title=title,
        subtitle=subtitle,
        nav=dashboard_render.render_filter_nav(nav_groups(values, href), href),
        sections=report['sections'],
        footer=(
            'Generated by scripts/session-metrics/dashboard.py. '
            'Raw prompts, commands, and tool output stay in the session files; '
            'keep this directory private.'
        ),
    )


def _write_views(data, values, start, end, now, output):
    detail = (f'Detail window {cakelib.fmt_utc(start)} to {cakelib.fmt_utc(end)}. '
              'Owner-only local report; no network requests.')
    atomic_write(output / 'index.html', _page(
        data, 'index.html', 'Cake metrics',
        'All models, projects, and builds. ' + detail, values, start, end, now))
    for dimension, folder in (('model', 'model'), ('project', 'project'), ('version', 'version')):
        for value in values.get(dimension) or []:
            filtered = filter_dataset(data, **{dimension: value})
            href = f'{folder}-{slug(value)}.html'
            atomic_write(output / href, _page(
                filtered, href, f'Cake metrics — {dimension}: {value}',
                f'Filtered to {dimension} {value}. ' + detail, values, start, end, now))


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
    values = filter_values(data)
    attribution = dashboard_report.change_attribution(data, start, end)
    payload = dict(schema_version=SCHEMA_VERSION, measured=cakelib.fmt_utc(now),
                   today=now.date().isoformat(),
                   window=dict(start=cakelib.fmt_utc(start), end=cakelib.fmt_utc(end)),
                   session_parse_errors=data.session_parse_errors,
                   telemetry_parse_errors=data.telemetry_parse_errors,
                   telemetry_available=data.telemetry_dir.is_dir(),
                   newest_activity=max((s.last_activity.isoformat() for s in data.sessions), default=None),
                   filters=values, attribution=attribution, rows=retained)
    atomic_write(output / 'metrics.json', json.dumps(payload))
    _write_views(data, values, start, end, now, output)
    print(output / 'index.html')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sessions-dir', type=Path)
    parser.add_argument('--telemetry-dir', type=Path)
    parser.add_argument('--output', type=Path, default=DEFAULT_OUTPUT)
    parser.set_defaults(days=0, model=None, project=None, session=None, invocation=None)
    generate(parser.parse_args())
