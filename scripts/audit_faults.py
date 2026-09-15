#!/usr/bin/env python3
"""Failure, cancellation and restart controls on independently copied synthetic media.

Tool failures emulate ENOSPC/EIO; no production mount is altered. --baseline
records failures without aborting so the unchanged binary can be compared.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

from audit_fixtures import sha256

ROOT = Path(__file__).resolve().parents[1]
ACTIVE_CHILDREN = set()


def terminate_process(process, timeout=5):
    """Terminate one converter process and every child in its private group."""
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=timeout)


def terminate_active_children():
    for process in list(ACTIVE_CHILDREN):
        terminate_process(process)


def handle_sigterm(signum, _frame):
    # audit_faults starts each converter in its own session so the test can
    # inspect descendant cleanup.  The outer runner's process-group signal
    # cannot reach that nested session after this parent receives SIGTERM.
    terminate_active_children()
    raise SystemExit(128 + signum)


def alive(pid):
    result = subprocess.run(['ps', '-o', 'stat=', '-p', str(pid)], capture_output=True, text=True)
    return bool(result.stdout.strip()) and not result.stdout.strip().startswith('Z')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', action='store_true')
    parser.add_argument('--case', help='Run one named control for reproduction')
    opts = parser.parse_args()
    resources = Path(os.environ.get('DOVIFUSE_AUDIT_RESOURCES', ROOT))
    binary = Path(os.environ.get('DOVIFUSE_AUDIT_BIN', ROOT/'dovifuse_converter/target/debug/dovifuse_converter'))
    seeds = Path(os.environ['DOVIFUSE_AUDIT_FIXTURES'])
    manifest = json.loads((seeds/'manifest.json').read_text())
    assert manifest['kind'] == 'synthetic-mechanics-only'
    tools = {name: str(Path(shutil.which(name) or resources/'tools'/name).resolve())
             for name in ('dovi_tool', 'mkvmerge', 'mkvextract', 'mediainfo', 'ffmpeg', 'ffprobe')}
    root = Path(tempfile.mkdtemp(prefix='dovifuse-faults-'))
    print('Fault evidence:', root, flush=True)
    cases = [
        ('cancel-capture-descendants', 'standard', 'ffmpeg', ['-vf'], 'hang'),
        ('cancel-status-descendants', 'standard', 'dovi_tool', ['convert'], 'hang'),
        ('cancel-version-probe', 'standard', 'ffmpeg', ['-version'], 'hang'),
        ('simultaneous-output-reservation', 'standard', 'dovi_tool', ['convert'], 'hang'),
        ('crash-restart-refuses-stale-output', 'standard', 'dovi_tool', ['convert'], 'hang'),
        ('leader-exit-pipe-held', 'standard', 'dovi_tool', ['convert'], 'orphan'),
        ('extraction-io-error', 'standard', 'mkvextract', ['tracks'], 'fail'),
        ('conversion-error', 'standard', 'dovi_tool', ['convert'], 'fail'),
        ('remux-disk-full', 'standard', 'mkvmerge', ['-o'], 'fail'),
        ('decode-error', 'standard', 'ffmpeg', ['-vf'], 'fail'),
        ('scratch-disappeared', 'standard', 'dovi_tool', ['convert'], 'remove-scratch'),
        ('archive-disappeared', 'archive', 'dovi_tool', ['demux'], 'lose-archive'),
        ('replacement-readonly', 'standard', 'ffmpeg', ['-vf'], 'readonly-output'),
        ('report-replaced-after-validation', 'standard', 'ffmpeg', ['-vf'], 'replace-report'),
        ('source-changed-before-replacement', 'standard', 'ffmpeg', ['-vf'], 'mutate-source'),
        ('hybrid-edit-error', 'hybrid', 'dovi_tool', ['editor'], 'fail'),
        ('hybrid-injection-error', 'hybrid', 'dovi_tool', ['inject-rpu'], 'fail'),
        ('hybrid-remux-io-error', 'hybrid', 'mkvmerge', ['-o'], 'fail'),
        ('checker-decode-error', 'check', 'ffmpeg', ['-vf'], 'fail'),
        ('repair-injection-error', 'repair', 'dovi_tool', ['inject-rpu'], 'fail'),
    ]
    if opts.case:
        cases = [case for case in cases if case[0] == opts.case]
        assert cases, 'Unknown fault case: '+opts.case
    previous_sigterm = signal.getsignal(signal.SIGTERM)
    signal.signal(signal.SIGTERM, handle_sigterm)
    results = []
    app_replays = []
    for name, mode, tool, tokens, action in cases:
        work = root/name
        work.mkdir()
        for directory in ('media', 'scratch', 'archive', 'tools'):
            (work/directory).mkdir()
        shutil.copytree(resources/'config', work/'config')
        for seed in ('p7.mkv', 'dv.mkv', 'hdr.mkv', 'shifted.mkv'):
            assert sha256(seeds/seed) == manifest['sha256'][seed]
            shutil.copyfile(seeds/seed, work/'media'/seed)
        before = {p: sha256(p) for p in (work/'media').iterdir()}
        for executable in tools:
            shim = work/'tools'/executable
            shim.write_text('#!'+sys.executable+'\nexec(compile(open('+repr(str(ROOT/'scripts/audit_fault_tool.py'))+
                            ').read(), "audit_fault_tool.py", "exec"))\n')
            shim.chmod(0o755)
        config = dict(tools=tools, tool=tool, tokens=tokens, action=action, work=str(work),
                      error='No space left on device (injected)' if 'full' in name else 'Input/output error (injected)')
        (work/'config.json').write_text(json.dumps(config))
        env = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(work), DOVIFUSE_FAULT_CONFIG=str(work/'config.json'),
                   PATH=str(work/'tools')+os.pathsep+os.environ['PATH'],
                   DOVIFUSE_PROCESSING_LOG_FILE=str(work/'processing.log'))
        args = [str(binary), '--progress', 'jsonl', '--hwaccel', 'off', '--report', str(work/'report.json'),
                '--tmp-dir', str(work/'scratch')]
        if mode in ('standard', 'archive'):
            args += (['--archive-dir', str(work/'archive')] if mode == 'archive' else ['-n'])
            args += [str(work/'media/p7.mkv')]
        elif mode == 'hybrid':
            args += ['--hybrid', str(work/'media/dv.mkv'), str(work/'media/hdr.mkv')]
        elif mode == 'repair':
            args += ['--repair-sync', '5', '--allow-padding', str(work/'media/shifted.mkv')]
        else:
            args += ['--check', str(work/'media/dv.mkv')]
        process = None
        row = {'case': name}
        try:
            with (work/'run.log').open('w') as log:
                process = subprocess.Popen(args, env=env, stdout=log, stderr=log, start_new_session=True)
                ACTIVE_CHILDREN.add(process)
                deadline = time.monotonic()+60
                while not (work/'triggered').exists() and process.poll() is None and time.monotonic() < deadline:
                    time.sleep(.05)
                assert (work/'triggered').exists(), 'Fault stage was not reached'
                start = time.monotonic()
                if action == 'hang':
                    # The marker precedes child creation. Wait before sending cancellation.
                    while not (work/'child.pid').exists() and time.monotonic()-start < 2:
                        time.sleep(.02)
                    if name in ('simultaneous-output-reservation', 'crash-restart-refuses-stale-output'):
                        if name.startswith('crash'):
                            process.kill()
                            process.wait(timeout=5)
                        second_args = args.copy()
                        second_args[second_args.index('--report')+1] = str(work/'second.report.json')
                        clean = dict(env, DOVIFUSE_SCRIPT_DIR=str(resources), PATH=os.environ['PATH'])
                        second = subprocess.run(second_args, env=clean, capture_output=True, text=True, timeout=30)
                        (work/'second.log').write_text(second.stdout+second.stderr)
                        assert second.returncode != 0 and 'output already exists' in second.stdout+second.stderr
                        assert '"event":"completed"' not in second.stdout
                        assert (work/'media/p7.DV8_TMP.mkv').exists(), 'Second job removed first job output'
                        assert all(sha256(p) == h for p, h in before.items())
                        if name.startswith('crash'):
                            assert json.loads((work/'report.json').read_text())['execution'] == 'running'
                            assert '"event":"completed"' not in (work/'run.log').read_text()
                            row.update(status='passed', recovery='Interrupted report and owned temporary files retained; restart refused')
                            results.append(row)
                            print(json.dumps(row), flush=True)
                            continue
                    process.send_signal(signal.SIGTERM)
                code = process.wait(timeout=10 if action in ('hang', 'orphan') else 60)
                row['failure_or_cancellation_seconds'] = round(time.monotonic()-start, 3)
                assert code != 0, 'Fault reported success'
            events = [json.loads(s) for s in (work/'run.log').read_text().splitlines() if s.startswith('{')]
            assert not any(e.get('event') == 'completed' for e in events), 'False completion'
            assert any(e.get('event') in ('failed', 'cancelled') for e in events), 'Missing failure event'
            if action != 'replace-report':
                report = json.loads((work/'report.json').read_text())
                assert report['execution'] in ('failed', 'cancelled'), report
                assert report['validation'] != 'pass', report
            else:
                assert (work/'report.json').read_text() == 'unrelated replacement report'
            for path, digest in before.items():
                if action == 'replace-report' and path.name == 'p7.mkv':
                    assert sha256(path) != digest, 'Expected validated replacement before report failure'
                else:
                    assert sha256(path) == digest, 'Source changed: '+path.name
            if (work/'child.pid').exists():
                assert not alive(int((work/'child.pid').read_text())), 'Descendant survived'
            row['status'] = 'passed'
            if action != 'replace-report':
                app_replays.append({'name': 'fault-'+name, 'log': str(work/'run.log'),
                                    'report': str(work/'report.json'), 'exit_status': code})
        except (AssertionError, subprocess.TimeoutExpired) as exc:
            row.update(status='failed', error=str(exc))
        finally:
            (work/'media').chmod(0o755)
            # Kill only this disposable test process and explicitly recorded descendants.
            for pidfile in ('child.pid', 'triggered'):
                if (work/pidfile).exists():
                    value = (work/pidfile).read_text()
                    pid = int(value) if pidfile.endswith('.pid') else json.loads(value)['pid']
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            if process is not None and process.poll() is None:
                terminate_process(process)
            if process is not None:
                ACTIVE_CHILDREN.discard(process)
        results.append(row)
        print(json.dumps(row), flush=True)
    signal.signal(signal.SIGTERM, previous_sigterm)
    summary = {'kind': 'synthetic-fault-injection', 'binary_sha256': sha256(binary), 'cases': results,
               'limits': ['ENOSPC/EIO are tool-boundary injections, not a real disconnected NAS.',
                          'No power-loss or uninterruptible kernel I/O guarantee.']}
    (root/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    if os.environ.get('DOVIFUSE_APP_REPLAY_MANIFEST'):
        path = Path(os.environ['DOVIFUSE_APP_REPLAY_MANIFEST'])
        existing = json.loads(path.read_text())
        path.write_text(json.dumps(existing+app_replays, indent=2)+'\n')
    if not opts.baseline:
        assert all(r['status'] == 'passed' for r in results), root


if __name__ == '__main__':
    main()
