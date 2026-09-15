#!/usr/bin/env python3
"""Report collision/failure/cancellation controls; disposable synthetic media only."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
ROOT=Path(__file__).resolve().parents[1]
WORK=Path(tempfile.mkdtemp(prefix='dovifuse-report-controls-',dir='/tmp'))
RES=Path(os.environ.get('DOVIFUSE_AUDIT_RESOURCES',ROOT))
BIN=Path(os.environ.get('DOVIFUSE_AUDIT_BIN',ROOT/'dovifuse_converter/target/debug/dovifuse_converter'))
SEEDS=Path(os.environ['DOVIFUSE_AUDIT_FIXTURES'])
ENV=dict(os.environ,DOVIFUSE_SCRIPT_DIR=str(RES),DOVIFUSE_PROCESSING_LOG_FILE=str(WORK/'processing.log'))
source=WORK/'source.mkv';shutil.copyfile(SEEDS/'dv.mkv',source)
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
original=sha(source)
def run(name,args,ok,env=ENV):
 p=subprocess.run([str(BIN),*map(str,args)],env=env,capture_output=True,text=True,timeout=180)
 (WORK/(name+'.log')).write_text(p.stdout+'\n'+p.stderr)
 assert (p.returncode==0)==ok,(name,p.returncode,p.stderr)
 return p
# Human progress must still populate checks, with the same conservative verdict.
human=WORK/'human.json'
run('human',['--check','--report',human,source],True)
r=json.loads(human.read_text());assert r['execution']=='completed' and len(r['checks'])>=5
assert r['inputs_before']==r['inputs_after']
# Existing report and source aliases must be rejected before touching any input.
old=human.read_bytes()
run('collision',['--check','--report',human,source],False);assert human.read_bytes()==old
link=WORK/'source-link';link.symlink_to(source)
run('input-alias',['--check','--report',link,source],False);assert sha(source)==original
# Directory passed as destination cannot become a report.
run('directory',['--check','--report',WORK,source],False)
# Valid CLI with unusable tool runtime still leaves a failed, readable report.
fake=WORK/'empty';fake.mkdir()
runtime=WORK/'runtime.json'
run('runtime',['--check','--report',runtime,source],False,dict(ENV,PATH=str(fake),DOVIFUSE_SCRIPT_DIR=str(fake)))
assert json.loads(runtime.read_text())['execution']=='failed'
# Dry runs record planned output identity and cannot validate actual media.
dry=WORK/'dry.json'
p=run('dry',['--hybrid','--dry-run','--delete-sources','--report',dry,source,SEEDS/'hdr.mkv'],True)
assert 'deprecated and ignored' in p.stdout+p.stderr
assert 'Would validate output and keep both originals' in p.stdout
assert 'delete both originals' not in p.stdout+p.stderr
assert sha(source)==original
help_result=run('help',['--help'],True)
assert '--delete-sources' not in help_result.stdout
assert json.loads(dry.read_text())['validation']=='inconclusive'
# Stall a real decoding phase after tool startup. Parent cancellation must still
# save its terminal report. This wrapper creates no media and has no network.
wrapped=WORK/'tools';wrapped.mkdir();ready=WORK/'ready';release=WORK/'release'
realff=shutil.which('ffmpeg',path=ENV['PATH']) or str(RES/'tools/ffmpeg')
wrapper=wrapped/'ffmpeg'
wrapper.write_text('#!'+sys.executable+'\nimport os,sys,time\n'
 'if "-version" in sys.argv: os.execv('+repr(realff)+',[ '+repr(realff)+',"-version"])\n'
 'open('+repr(str(ready))+',"w").write("ready")\n'
 'while not os.path.exists('+repr(str(release))+'): time.sleep(.05)\n'
 'os.execv('+repr(realff)+', ['+repr(realff)+']+sys.argv[1:])\n')
wrapper.chmod(0o755)
slow=dict(ENV,PATH=str(wrapped)+os.pathsep+ENV['PATH'])
def start(name):
 if ready.exists():ready.unlink()
 if release.exists():release.unlink()
 report=WORK/(name+'.json')
 p=subprocess.Popen([str(BIN),'--progress','jsonl','--check','--report',str(report),str(source)],env=slow,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
 deadline=time.monotonic()+30
 while not ready.exists() and p.poll() is None and time.monotonic()<deadline:time.sleep(.05)
 if not ready.exists():
  p.terminate();out,err=p.communicate(timeout=10);raise AssertionError((name,'decoder did not start',out,err))
 return p,report
p,cancelled=start('cancelled');p.send_signal(signal.SIGTERM);out,err=p.communicate(timeout=15)
(WORK/'cancelled.log').write_text(out+err)
assert p.returncode!=0 and json.loads(cancelled.read_text())['execution']=='cancelled'
assert '"event":"completed"' not in out
# A removed/replaced report path must never yield a completion event. Keep the
# replacement content intact while cancelling the owned decoder.
p,replaced=start('replaced');replaced.unlink();replaced.write_text('unrelated replacement');p.send_signal(signal.SIGTERM)
out,err=p.communicate(timeout=15);assert p.returncode!=0 and replaced.read_text()=='unrelated replacement'
assert '"event":"completed"' not in out and 'job_finalized' not in out
p,changed=start('changed-input')
stat=source.stat();os.utime(source,ns=(stat.st_atime_ns,stat.st_mtime_ns+2_000_000_000));release.write_text('resume')
out,err=p.communicate(timeout=60)
rchanged=json.loads(changed.read_text())
assert p.returncode!=0 and rchanged['execution']=='failed' and 'identity changed' in rchanged['error']
assert '\"event\":\"completed\"' not in out
assert sha(source)==original
summary={'human_checks':len(r['checks']),'existing_report':'preserved','input_alias':'rejected; input unchanged','runtime_failure':'failed report','dry_run':'inconclusive','cancellation':'cancelled report; no completion','replaced_report':'unrelated replacement preserved; no completion','changed_input':'failed; no completion or repair authority','source_sha256':original}
(WORK/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
print('Job report controls passed:',WORK,json.dumps(summary))
