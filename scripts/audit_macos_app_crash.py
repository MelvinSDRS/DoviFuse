#!/usr/bin/env python3
"""Force-kill an isolated native AppModel host; requires a logged-in Mac GUI.

Separate from CI report replay: compiles the real AppModel in a disposable app,
holds validation on synthetic media, kills only that app, then verifies restart.
"""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
WORK = Path(tempfile.mkdtemp(prefix='dv8-native-crash-', dir='/tmp'))
RES = Path(os.environ['DV8_AUDIT_RESOURCES'])
APP = WORK/'AuditHost.app'
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()


def wait_for(predicate, seconds=60):
    deadline = time.monotonic()+seconds
    while time.monotonic() < deadline:
        if predicate(): return
        time.sleep(.05)
    raise RuntimeError('Native app audit timed out: '+str(WORK))


def processes():
    rows = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,comm='], text=True)
    return [(int(p),int(parent),name) for p,parent,name in (row.strip().split(None,2) for row in rows.splitlines())]


subprocess.run(['ditto', str(RES.parent.parent), str(APP)], check=True)
contents = APP/'Contents'
executable = contents/'MacOS/AuditHost'
subprocess.run(['xcrun','swiftc','-swift-version','6','-strict-concurrency=complete','-parse-as-library',
                str(ROOT/'macapp/DV8Maker/AppModel.swift'), str(ROOT/'macapp/DV8Maker/ScratchCapacity.swift'),
                str(ROOT/'macapp/Tests/AppCrashAuditHost.swift'),'-o',str(executable)], check=True)
plist_path=contents/'Info.plist'
info=plistlib.loads(plist_path.read_bytes())
info.update(CFBundleExecutable='AuditHost',CFBundleIdentifier='local.dv8.crashaudit.'+WORK.name,CFBundleName='DV8 Audit Host')
plist_path.write_bytes(plistlib.dumps(info))
source=WORK/'source.mkv'
shutil.copyfile(Path(os.environ['DV8_AUDIT_FIXTURES'])/'p7.mkv', source)
before=digest(source)
(WORK/'scratch').mkdir()
shim=contents/'Resources/tools/ffmpeg'
shim.write_text('#!/usr/bin/python3\nimport json,os,subprocess,sys,time\nfrom pathlib import Path\n'
                'if "-version" in sys.argv: sys.exit(subprocess.call(['+repr(str(RES/'tools/ffmpeg'))+']+sys.argv[1:]))\n'
                'Path(os.environ["DV8_NATIVE_AUDIT_ROOT"],"held.json").write_text(json.dumps({"pid":os.getpid()}))\n'
                'time.sleep(3600)\n')
shim.chmod(0o755)
subprocess.run(['codesign','--force','--deep','--sign','-',str(APP)],check=True)
env=dict(os.environ,DV8_NATIVE_AUDIT_ROOT=str(WORK))
host=restart=None
converter_pid=held_pid=None
try:
    with (WORK/'host.log').open('w') as log:
        host=subprocess.Popen([str(executable),'--run'],env=env,stdout=log,stderr=log)
        wait_for(lambda:(WORK/'held.json').exists())
        held_pid=json.loads((WORK/'held.json').read_text())['pid']
        converter_pid=next(pid for pid,parent,name in processes() if parent==host.pid and name.endswith('/dv8_converter'))
        reports=Path.home()/'Library/Application Support/DV8 Maker/Reports'
        report=next(p for p in reports.glob('*.json') if str(source) in json.loads(p.read_text()).get('arguments',[]))
        assert json.loads(report.read_text())['execution']=='running'
        host.kill();host.wait(timeout=5)
        assert digest(source)==before
        saved=json.loads(report.read_text())
        assert saved['execution']!='completed' and saved['validation']!='pass'
        restart=subprocess.Popen([str(executable)],env=env,stdout=log,stderr=log)
        wait_for(lambda:(WORK/'restart.json').exists())
        state=json.loads((WORK/'restart.json').read_text())
        assert state=={'isRunning':False,'phase':'Ready','hasOutput':False,'hasReport':False},state
        assert not any(parent==restart.pid and name.endswith('/dv8_converter') for _,parent,name in processes())
        # Force-kill cannot ask the converter to cancel. The audit supervisor
        # explicitly stops the isolated surviving job after testing restart.
        os.kill(converter_pid,signal.SIGTERM)
        wait_for(lambda:not any(pid in (converter_pid,held_pid) for pid,_,_ in processes()),10)
        assert digest(source)==before and report.exists()
        assert json.loads(report.read_text())['execution']!='completed'
        result={'kind':'native-AppModel-host-force-kill','source_retained':True,
                'diagnostic_report_retained':True,'restart':'idle; no automatic resume or false completion',
                'supervisor_cleanup':'isolated converter and held tool stopped within 10 seconds',
                'limits':['Uses production AppModel with an audit-only native window/entry point; does not test UI file selection.',
                          'SIGKILL does not provide ordinary cancellation; surviving job is explicitly stopped by the test supervisor.']}
        (WORK/'summary.json').write_text(json.dumps(result,indent=2))
        print('Native app interruption passed:',WORK,json.dumps(result))
finally:
    for process in (restart,host):
        if process and process.poll() is None: process.kill();process.wait(timeout=5)
    for pid in (converter_pid,held_pid):
        if pid:
            command=subprocess.run(['ps','-p',str(pid),'-o','command='],capture_output=True,text=True)
            if str(WORK) not in command.stdout: continue
            try: os.kill(pid,signal.SIGKILL)
            except ProcessLookupError: pass
