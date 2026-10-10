#!/usr/bin/env python3
"""Offline fixtures for the one-time backlog conversion; no external commands."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import subprocess

spec = importlib.util.spec_from_file_location('importer', Path(__file__).with_name('import-gh-issues-to-ahm.py'))
importer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(importer)


def issue(number, body='', parent=None, blocked=()):
    return {'number': number, 'title': f'Issue {number}', 'body': body,
            'createdAt': '2026-10-09T00:00:00Z', 'url': f'https://github.com/travisennis/cake/issues/{number}',
            'labels': [{'name': 'type:docs'}, {'name': 'area:docs'}],
            'parent': {'number': parent} if parent else None,
            'blocked_by': [{'number': n} for n in blocked], 'comments': []}


def snapshot(issues, statuses=None):
    return {'fetched_at': '2026-10-09T00:00:00Z', 'issues': issues,
            'closed_count': 10, 'dependency_targets': {},
            'board': {'items': [{'content': {'type': 'Issue', 'number': i['number']},
                                 'repository': 'https://github.com/travisennis/cake',
                                 'status': (statuses or {}).get(i['number'], 'Ready'),
                                 'priority': 'P1', 'effort': 'S'} for i in issues]}}


class ConversionTests(unittest.TestCase):
    def test_sorted_allocation_trackers_children_and_board_statuses(self):
        data = snapshot([issue(30, parent=10), issue(10), issue(20)],
                        {10: 'Backlog', 20: 'Blocked'})
        records, baseline = importer.convert(data)
        self.assertEqual([r['ref'] for r in records], ['10', '20', '30'])
        self.assertEqual([r['status'] for r in records], ['Tracking', 'Blocked', 'Pending'])
        self.assertEqual(records[2]['parent'], '@10')
        self.assertEqual(records[0]['labels'], 'type:docs,area:docs')
        self.assertEqual(baseline['trackers'], {'10': [30]})
        self.assertEqual(baseline['board_counts'], {'Backlog': 1, 'Blocked': 1, 'Ready': 1})

    def test_dependency_sources_merge_and_closed_not_planned_are_reported(self):
        data = snapshot([issue(1, '## Depends on\n- #2\n- #3\nNone — #4 is closed', blocked=(2, 5)), issue(2)])
        data['dependency_targets'] = {'3': {'state': 'closed', 'state_reason': 'not_planned'},
                                      '5': {'state': 'closed', 'state_reason': 'completed'}}
        records, baseline = importer.convert(data)
        self.assertEqual(records[0]['depends_on'], ['@2'])
        self.assertNotIn('Depends on', records[0]['body'])
        self.assertEqual([d['target'] for d in baseline['dropped_dependencies']], [3, 5])
        self.assertTrue(baseline['dropped_dependencies'][0]['manual_review'])
        self.assertFalse(baseline['dropped_dependencies'][1]['manual_review'])

    def test_missing_board_or_open_dependency_or_parent_refuses(self):
        for data in [snapshot([issue(1, parent=9)]),
                     snapshot([issue(1, blocked=(9,))]),
                     snapshot([issue(1)], {1: 'Done'})]:
            with self.subTest(data=data), self.assertRaises(ValueError):
                importer.convert(data)

    def test_sections_preserve_nonacceptance_checkboxes_and_fenced_examples(self):
        body = ('## Work\n- [ ] keep\n## Migrated from ahm\nold\n### Nested\nold too\n'
                '## Acceptance Criteria\n- [ ] run\n* [x] done\n### Detail\n- [X] yes\n'
                '```md\n## Depends on\n- #99\n- [ ] example\n```\n'
                '## Next\n- [ ] keep too\n## Depends on\n- #2\ntext #3\n## End\nend')
        cleaned, deps = importer.sections(body)
        self.assertEqual(deps, [2])
        self.assertNotIn('old', cleaned)
        self.assertIn('- run\n* done\n### Detail\n- yes', cleaned)
        self.assertIn('## Depends on\n- #99\n- [ ] example', cleaned)
        self.assertIn('- [ ] keep too', cleaned)

    def test_comments_and_links_are_preserved_or_reported(self):
        data = snapshot([issue(1, '[good](../../docs/a.md#section) [gone](../completed/1.md) [web](https://example.com)')])
        data['issues'][0]['comments'] = [{'created_at': '2026-10-09T01:00:00Z', 'user': {'login': 'trav'}, 'body': 'Evidence [a](docs/a.md)'}]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'docs').mkdir()
            (root / 'docs/a.md').write_text('test')
            records, baseline = importer.convert(data, root)
        body = records[0]['body']
        self.assertIn('[good](https://github.com/travisennis/cake/blob/master/docs/a.md#section)', body)
        self.assertIn('gone [web](https://example.com)', body)
        self.assertIn('**2026-10-09T01:00:00Z** — _trav_: Evidence', body)
        self.assertEqual(len(baseline['links']), 3)
        self.assertIsNone(baseline['links'][1]['replacement'])
        self.assertEqual(baseline['comment_count'], 1)

    def test_reference_links_keep_external_urls_and_strip_missing_targets(self):
        findings = []
        body = '[site][web] [missing][] [web]\n\n[web]: https://example.com\n[missing]: ../gone.md'
        rewritten = importer.rewrite_links(body, 1, findings, importer.ROOT)
        self.assertIn('[site](https://example.com)', rewritten)
        self.assertIn('missing [web](https://example.com)', rewritten)
        self.assertEqual(findings, [{'issue': 1, 'target': '../gone.md', 'replacement': None}])

    def test_only_parent_404_is_treated_as_absent(self):
        for missing_ok, error, expected in [(True, 'HTTP 404: Not Found', None),
                                           (True, 'HTTP 403: Forbidden', RuntimeError),
                                           (False, 'HTTP 404: Not Found', RuntimeError)]:
            with self.subTest(error=error, missing_ok=missing_ok), patch.object(
                    importer.subprocess, 'run', return_value=subprocess.CompletedProcess(
                        ['gh'], 1, '', error)):
                if expected is None:
                    self.assertIsNone(importer.gh('api', 'parent', missing_ok=missing_ok))
                else:
                    with self.assertRaises(expected):
                        importer.gh('api', 'parent', missing_ok=missing_ok)

    def test_replay_always_previews_and_returns_importer_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'snapshot.json'
            importer.write_json(source, snapshot([issue(1)]))
            argv = ['importer', '--snapshot', str(source), '--output', str(root / 'out.json'),
                    '--baseline', str(root / 'baseline.json')]
            with patch.object(importer.sys, 'argv', argv), patch.object(
                    importer.subprocess, 'run', return_value=subprocess.CompletedProcess(
                        ['ahm'], 1, '', 'refused')) as run, patch('builtins.print'):
                self.assertEqual(importer.main(), 1)
                self.assertEqual(run.call_args.args[0][:5],
                                 ['ahm', '--dry-run', '--json', 'task', 'import'])
            self.assertTrue((root / 'baseline.json').exists())

    def test_artifacts_cannot_enter_checkout(self):
        with self.assertRaises(importer.argparse.ArgumentTypeError):
            importer.artifact(str(importer.ROOT / 'import.json'))


if __name__ == '__main__':
    unittest.main()
