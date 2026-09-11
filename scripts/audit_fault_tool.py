#!/usr/bin/env python3
"""Disposable audit-tool shim. Not bundled or used by normal conversions."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

config = json.loads(Path(os.environ['DV8_FAULT_CONFIG']).read_text())
tool = Path(sys.argv[0]).name
args = sys.argv[1:]
real = config['tools'][tool]
match = tool == config['tool'] and all(token in args for token in config['tokens'])
if not match:
    os.execv(real, [real, *args])

work = Path(config['work'])
action = config['action']
(work/'triggered').write_text(json.dumps({'tool': tool, 'args': args, 'pid': os.getpid()}))
if action in ('hang', 'orphan'):
    child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'])
    (work/'child.pid').write_text(str(child.pid))
    if action == 'orphan':
        sys.exit(0)
    time.sleep(120)
elif action == 'fail':
    print(config.get('error', 'Injected tool failure'), file=sys.stderr)
    sys.exit(74)
else:
    result = subprocess.run([real, *args])
    if result.returncode:
        sys.exit(result.returncode)
    if action == 'remove-scratch':
        for entry in (work/'scratch').iterdir():
            if entry.is_dir():
                shutil.rmtree(entry)
    elif action == 'readonly-output':
        (work/'media').chmod(0o555)
    elif action == 'lose-archive':
        (work/'archive').rmdir()
        (work/'archive').write_text('unavailable archive destination')
    elif action == 'replace-report':
        report = work/'report.json'
        report.unlink()
        report.write_text('unrelated replacement report')
    elif action == 'mutate-source':
        source = work/'media/p7.mkv'
        stat = source.stat()
        os.utime(source, ns=(stat.st_atime_ns, stat.st_mtime_ns+2_000_000_000))
    else:
        raise ValueError(action)
