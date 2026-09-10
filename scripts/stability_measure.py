#!/usr/bin/env python3
"""Prepare independent media copies and measure an explicitly supplied audit command.

Evidence stays at the caller's chosen private path. RSS and disk peaks are sampled,
not exact kernel high-water marks. No source deletion, mounting or hardlinking.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import time


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(4 * 1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def tree_bytes(root):
    total = 0
    for directory, _, files in os.walk(root):
        for name in files:
            path = Path(directory)/name
            try:
                if not path.is_symlink():
                    total += path.stat().st_size
            except FileNotFoundError:
                pass
    return total


def process_sample(parent, known):
    text = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,rss=,stat='], text=True)
    rows = {}
    for line in text.splitlines():
        pid, ppid, rss, state = line.split(maxsplit=3)
        rows[int(pid)] = (int(ppid), int(rss)*1024, state)
    selected = {parent} | (known & rows.keys())
    while True:
        children = {pid for pid, (ppid, _, _) in rows.items() if ppid in selected}
        if children <= selected:
            break
        selected |= children
    active = {pid for pid in selected if pid in rows and not rows[pid][2].startswith('Z')}
    return sum(rows[pid][1] for pid in active), active


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    copy = commands.add_parser('copy')
    copy.add_argument('source', type=Path)
    copy.add_argument('destination', type=Path)
    run = commands.add_parser('run')
    run.add_argument('--evidence', required=True, type=Path)
    run.add_argument('--scratch', required=True, type=Path)
    run.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.action == 'copy':
        before = args.source.stat()
        if shutil.disk_usage(args.destination.parent).free < before.st_size + 5*1024**3:
            raise RuntimeError('Insufficient copy destination space including 5 GiB reserve')
        digest = hashlib.sha256()
        with args.source.open('rb') as source, args.destination.open('xb') as destination:
            for chunk in iter(lambda: source.read(4*1024*1024), b''):
                digest.update(chunk)
                destination.write(chunk)
            destination.flush()
            os.fsync(destination.fileno())
        after = args.source.stat()
        assert (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) == (
            after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'Source changed during copy'
        assert not args.source.samefile(args.destination), 'Copy aliases source'
        assert sha256(args.destination) == digest.hexdigest(), 'Copy checksum mismatch'
        print(json.dumps({'source': str(args.source), 'copy': str(args.destination),
                          'bytes': before.st_size, 'sha256': digest.hexdigest(), 'independent_copy': True}))
        return
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        parser.error('Missing command')
    args.scratch.mkdir(parents=True, exist_ok=True)
    # Reserve evidence first; never overwrite a previous measurement.
    with args.evidence.open('x') as evidence, args.evidence.with_suffix('.log').open('x') as log:
        started = time.monotonic()
        known = set()
        peak_rss = peak_scratch = 0
        samples = 0
        with subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT) as process:
            while process.poll() is None:
                rss, active = process_sample(process.pid, known)
                known |= active
                peak_rss = max(peak_rss, rss)
                peak_scratch = max(peak_scratch, tree_bytes(args.scratch))
                samples += 1
                time.sleep(.5)
            _, survivors = process_sample(process.pid, known)
        json.dump({'command': command, 'platform': platform.platform(), 'exit_status': process.returncode,
                   'elapsed_seconds': time.monotonic()-started, 'peak_process_tree_rss_bytes_sampled': peak_rss,
                   'peak_scratch_bytes_sampled': peak_scratch, 'sample_interval_seconds': .5, 'samples': samples,
                   'remaining_scratch_bytes': tree_bytes(args.scratch), 'surviving_pids': sorted(survivors),
                   'scope': 'Observed command only; not full-workflow or playback acceptance'}, evidence, indent=2)
        evidence.write('\n')
    raise SystemExit(process.returncode)


if __name__ == '__main__':
    main()
