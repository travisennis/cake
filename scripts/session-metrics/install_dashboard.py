#!/usr/bin/env python3
"""Install, open, or remove cake's local macOS metrics schedule (no model calls)."""
import argparse
import os
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path

import dashboard

LABELS = ('com.cake.metrics.refresh', 'com.cake.metrics.reminder')
PYTHON_FALLBACKS = ('/opt/homebrew/bin/python3', '/usr/local/bin/python3', '/usr/bin/python3')


def python_candidates(python):
    """Prefer the installer's interpreter, then stable Homebrew and system paths."""
    return list(dict.fromkeys([str(python), *PYTHON_FALLBACKS]))


def jobs(root, python, sessions, telemetry):
    runtime = root / 'runtime'
    script = runtime / 'metrics_job.sh'
    common = dict(ProcessType='Background')
    refresh = dict(common, Label=LABELS[0],
                   ProgramArguments=['/bin/sh', str(script), 'refresh', str(root),
                                     str(sessions), str(telemetry),
                                     *python_candidates(python)],
                   StartCalendarInterval=dict(Hour=8, Minute=0), RunAtLoad=True,
                   StandardOutPath=str(root / 'refresh.log'),
                   StandardErrorPath=str(root / 'refresh.log'))
    reminder = dict(common, Label=LABELS[1],
                    ProgramArguments=['/bin/sh', str(script), 'remind', str(root)],
                    StartCalendarInterval=dict(Weekday=1, Hour=9, Minute=0),
                    StandardOutPath=str(root / 'reminder.log'),
                    StandardErrorPath=str(root / 'reminder.log'))
    return [refresh, reminder]


def launchctl(*args, check=True):
    return subprocess.run(['/bin/launchctl', *args], check=check, capture_output=True, text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['install', 'open', 'uninstall'])
    parser.add_argument('--root', type=Path, default=dashboard.DEFAULT_OUTPUT)
    parser.add_argument('--agents-dir', type=Path, default=Path.home() / 'Library/LaunchAgents')
    parser.add_argument('--no-load', action='store_true', help='Prepare files without registering jobs (for validation)')
    parser.add_argument('--sessions-dir', type=Path)
    parser.add_argument('--telemetry-dir', type=Path)
    ns = parser.parse_args()
    root = ns.root.expanduser().resolve()
    agents = ns.agents_dir.expanduser().resolve()
    domain = f'gui/{os.getuid()}'
    if ns.action == 'open':
        subprocess.run(['/usr/bin/open', str(root / 'index.html')], check=True)
        return
    if ns.action == 'uninstall':
        for label in LABELS:
            if not ns.no_load:
                launchctl('bootout', f'{domain}/{label}', check=False)
            (agents / (label + '.plist')).unlink(missing_ok=True)
        print(f'Schedule removed. Reports and history preserved at {root}')
        return
    if sys.platform != 'darwin' and not ns.no_load:
        parser.error('Scheduling requires macOS; dashboard.py works without a scheduler.')
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(root, 0o700)
    runtime = root / 'runtime'
    runtime.mkdir(exist_ok=True, mode=0o700)
    source = Path(__file__).resolve().parent
    for name in ('dashboard.py', 'dashboard.html', 'cakelib.py', 'install_dashboard.py',
                 'metrics_job.sh'):
        shutil.copy2(source / name, runtime / name)
    sessions = dashboard.cakelib.resolve_sessions_dir(ns).expanduser().resolve()
    telemetry = dashboard.cakelib.resolve_telemetry_dir(ns).expanduser().resolve()
    # Keep the interpreter unresolved: Homebrew revises the Cellar directory
    # behind the opt symlink, and the job script falls back if it disappears.
    definitions = jobs(root, Path(sys.executable).absolute(), sessions, telemetry)
    # Validate and generate before replacing an existing schedule.
    subprocess.run(definitions[0]['ProgramArguments'], check=True)
    agents.mkdir(parents=True, exist_ok=True)
    for job in definitions:
        path = agents / (job['Label'] + '.plist')
        if not ns.no_load:
            launchctl('bootout', f"{domain}/{job['Label']}", check=False)
        dashboard.atomic_write(path, plistlib.dumps(job).decode())
        if not ns.no_load:
            result = launchctl('bootstrap', domain, str(path), check=False)
            if result.returncode:
                raise SystemExit(f"Could not register {job['Label']}: {result.stderr.strip()}")
    print(f"Dashboard: {root / 'index.html'}\nDaily refresh 08:00; Monday reminder 09:00 (local time).")
    print(f"Open: {sys.executable} {runtime / 'install_dashboard.py'} open")


if __name__ == '__main__':
    main()
