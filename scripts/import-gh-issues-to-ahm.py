#!/usr/bin/env python3
"""Snapshot Cake's GitHub backlog for ahm. Fetching always previews, never imports.

Artifacts must live outside the checkout. Re-run --fetch to refresh the baseline;
review that baseline before the separate milestone 3 import operation.
"""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import quote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
REPO = 'travisennis/cake'
STATUS = {'Backlog': 'Open', 'Ready': 'Pending', 'In Progress': 'In Progress',
          'Blocked': 'Blocked'}
ACCEPTANCE = {'acceptance', 'acceptance notes', 'acceptance criteria'}


def gh(*args, missing_ok=False):
    result = subprocess.run(['gh', *args], text=True, capture_output=True, check=False)
    if result.returncode:
        if missing_ok and 'HTTP 404' in result.stderr:
            return None
        raise RuntimeError(f"gh {' '.join(args)}: {result.stderr.strip()}")
    return json.loads(result.stdout)


def pages(endpoint):
    return [item for page in gh('api', '--paginate', '--slurp', endpoint)
            for item in page]


def issue_details(issue):
    base = f"repos/{REPO}/issues/{issue['number']}"
    return {**issue, 'comments': pages(base + '/comments'),
            'blocked_by': pages(base + '/dependencies/blocked_by'),
            'parent': gh('api', base + '/parent', missing_ok=True)}


def fetch():
    issues = gh('issue', 'list', '--repo', REPO, '--state', 'open', '--limit', '10000',
                '--json', 'number,title,body,url,createdAt,labels')
    board = gh('project', 'item-list', '1', '--owner', 'travisennis',
               '--limit', '10000', '--format', 'json')
    if len(issues) >= 10000 or len(board['items']) != board['totalCount']:
        raise ValueError('Snapshot truncated; increase fetch limits before continuing')
    with ThreadPoolExecutor(max_workers=8) as pool:
        issues = list(pool.map(issue_details, issues))
    imported = {i['number'] for i in issues}
    refs = set()
    for issue in issues:
        _, deps = sections(issue['body'] or '')
        refs.update(deps)
        refs.update(d['number'] for d in issue['blocked_by'])
    targets = {}
    for number in sorted(refs - imported):
        targets[str(number)] = gh('api', f'repos/{REPO}/issues/{number}')
    closed = gh('api', 'search/issues', '-f', f'q=repo:{REPO} is:issue is:closed',
                '-X', 'GET')['total_count']
    return {'fetched_at': datetime.now(timezone.utc).isoformat(), 'issues': issues,
            'board': board, 'dependency_targets': targets, 'closed_count': closed}


def sections(body):
    """Remove migrated/dependency sections and normalize acceptance checkboxes.

    Headings inside fenced examples are content, not section boundaries.
    """
    output, dependencies = [], []
    removed = acceptance_level = None
    fence = None
    for line in body.splitlines():
        marker = re.match(r'^\s{0,3}(`{3,}|~{3,})', line)
        if marker:
            token = marker[1]
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
            if removed is None:
                output.append(line)
            continue
        heading = re.match(r'^(#{1,6})\s+(.+?)\s*#*$', line) if fence is None else None
        if heading:
            level, name = len(heading[1]), heading[2].strip().lower()
            if removed and level <= removed[0]:
                removed = None
            if acceptance_level and level <= acceptance_level:
                acceptance_level = None
            if removed is None and level == 2 and name in {'depends on', 'migrated from ahm'}:
                removed = (level, name)
            if name in ACCEPTANCE and level in {2, 3}:
                acceptance_level = level
        if removed:
            if removed[1] == 'depends on' and fence is None:
                match = re.match(r'^\s*[-*+]\s+#(\d+)\b', line)
                if match:
                    dependencies.append(int(match[1]))
            continue
        if acceptance_level and fence is None:
            line = re.sub(r'^(\s*[-*+]\s+)\[[ xX]\]\s*', r'\1', line)
        output.append(line)
    return '\n'.join(output).strip(), dependencies


def rewrite_links(body, number, findings, root):
    root = root.resolve()

    def replace(match):
        label, target = match[1], match[2]
        target = target.strip('<>')
        parsed = urlsplit(target)
        if parsed.scheme or target.startswith('//') or target.startswith('#'):
            return match[0]
        # Issue-relative task/research paths have no surviving local authority.
        relative = parsed.path
        while relative.startswith('../'):
            relative = relative[3:]
        path = (root / relative.lstrip('/')).resolve()
        if path.is_relative_to(root) and path.exists():
            url = f'https://github.com/{REPO}/blob/master/{quote(path.relative_to(root).as_posix())}'
            if parsed.fragment:
                url += '#' + parsed.fragment
            findings.append({'issue': number, 'target': target, 'replacement': url})
            return f'[{label}]({url})'
        findings.append({'issue': number, 'target': target, 'replacement': None})
        return label
    body = re.sub(r'\[([^\]\n]+)\]\(([^\s)]+)\)', replace, body)
    # Convert reference links before removing definitions, including shortcut links.
    definitions = dict(re.findall(r'^\s{0,3}\[([^\]]+)\]:\s*(\S+)\s*$', body, re.MULTILINE))
    definitions = {label.lower(): target for label, target in definitions.items()}
    if definitions:
        body = re.sub(r'^\s{0,3}\[([^\]]+)\]:\s*(\S+)\s*$', '', body, flags=re.MULTILINE)

        def reference(match):
            label = match[1]
            key = (match[2] or label).lower()
            if key not in definitions:
                return match[0]
            inline = f'[{label}]({definitions[key]})'
            return replace(re.fullmatch(r'\[([^\]\n]+)\]\((.+)\)', inline))

        body = re.sub(r'\[([^\]\n]+)\](?:\[([^\]\n]*)\])?(?!\()', reference, body)

    return body


def convert(snapshot, root=ROOT):
    issues = sorted(snapshot['issues'], key=lambda i: i['number'])
    imported = {i['number'] for i in issues}
    fields = {item['content']['number']: item for item in snapshot['board']['items']
              if item.get('content', {}).get('type') == 'Issue'
              and item.get('repository') == f'https://github.com/{REPO}'}
    parents = {i['parent']['number'] for i in issues if i['parent']}
    if not parents <= imported:
        raise ValueError(f'Parents outside import batch: {sorted(parents - imported)}')
    records, links, dropped = [], [], []
    for issue in issues:
        number = issue['number']
        field = fields.get(number, {})
        if field.get('status') not in STATUS or field.get('priority') not in {'P0', 'P1', 'P2', 'P3', 'P4'} or field.get('effort') not in {'XS', 'S', 'M', 'L', 'XL'}:
            raise ValueError(f'Issue #{number}: missing or unsupported board fields: {field}')
        body, deps = sections(issue['body'] or '')
        deps = sorted(set(deps) | {d['number'] for d in issue['blocked_by']})
        for dep in deps:
            if dep not in imported:
                target = snapshot['dependency_targets'].get(str(dep))
                if not target or target['state'] != 'closed':
                    raise ValueError(f'Issue #{number}: dependency #{dep} is not imported or closed')
                dropped.append({'issue': number, 'target': dep,
                                'state_reason': target.get('state_reason'),
                                'manual_review': target.get('state_reason') == 'not_planned'})
        comments = issue['comments']
        if comments:
            body += '\n\n## Comments\n\n' + '\n\n'.join(
                f"**{c['created_at']}** — _{(c.get('user') or {}).get('login', 'deleted-user')}_: {c['body']}"
                for c in comments)
        body = rewrite_links(body, number, links, root)
        records.append({'ref': str(number), 'title': issue['title'], 'body': body,
                        'status': 'Tracking' if number in parents else STATUS[field['status']],
                        'priority': field['priority'], 'effort': field['effort'],
                        'labels': ','.join(label['name'] for label in issue['labels']),
                        'created': issue['createdAt'], 'external_ref': issue['url'],
                        'parent': '@' + str(issue['parent']['number']) if issue['parent'] else '',
                        'depends_on': ['@' + str(d) for d in deps if d in imported]})
    baseline = {'fetched_at': snapshot['fetched_at'], 'open_count': len(issues),
                'closed_count': snapshot['closed_count'], 'issue_numbers': sorted(imported),
                'board_counts': dict(Counter(fields[n]['status'] for n in imported)),
                'import_counts': dict(Counter(r['status'] for r in records)),
                'trackers': {str(n): [i['number'] for i in issues if i['parent'] and i['parent']['number'] == n] for n in sorted(parents)},
                'comment_count': sum(len(i['comments']) for i in issues),
                'dropped_dependencies': dropped, 'links': links}
    return records, baseline


def artifact(value):
    path = Path(value).expanduser().resolve()
    if path.is_relative_to(ROOT):
        raise argparse.ArgumentTypeError('Migration artifacts must be outside the repository')
    return path


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--fetch', action='store_true')
    source.add_argument('--snapshot', type=Path, help='Replay a saved fetch without GitHub')
    parser.add_argument('--output', required=True, type=artifact)
    parser.add_argument('--baseline', required=True, type=artifact)
    parser.add_argument('--save-snapshot', type=artifact)
    args = parser.parse_args()
    paths = [p for p in (args.output, args.baseline, args.save_snapshot, args.snapshot) if p]
    if len({p.resolve() for p in paths}) != len(paths):
        parser.error('Artifact paths must be distinct')
    try:
        snapshot = fetch() if args.fetch else json.loads(args.snapshot.read_text())
        records, baseline = convert(snapshot)
        write_json(args.output, records)
        write_json(args.baseline, baseline)
        if args.save_snapshot:
            write_json(args.save_snapshot, snapshot)
        result = subprocess.run(['ahm', '--dry-run', '--json', 'task', 'import',
                                 '--from-file', str(args.output)], capture_output=True, text=True)
        print(result.stdout, end='')
        print(result.stderr, end='', file=sys.stderr)
        return result.returncode
    except (OSError, ValueError, RuntimeError) as error:
        print(f'Import snapshot failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
