#!/usr/bin/env python3
"""P5 rejection and removed experimental CLI entry points on main."""
import hashlib,json,os,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
RESOURCES=Path(os.environ.get('DV8_AUDIT_RESOURCES',ROOT))
BIN=Path(os.environ.get('DV8_AUDIT_BIN',ROOT/'dv8_converter/target/debug/dv8_converter'))
FIX=Path(os.environ['DV8_AUDIT_FIXTURES'])
WORK=Path(tempfile.mkdtemp(prefix='dv8-p5-disabled-'))
inputs=[FIX/'p5-mechanics.mkv',FIX/'hdr.mkv']
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
before={str(p):digest(p) for p in inputs}
env=dict(os.environ,DV8_SCRIPT_DIR=str(RESOURCES),DV8_PROCESSING_LOG_FILE=str(WORK/'processing.log'))
replays=[];results={}
for name,flags in [('default',[]),('force',['--force']),('skip-grade',['--skip-grade-check']),('metadata',['--grade-check','metadata']),('force-skip',['--force','--skip-grade-check']),('dry-run',['--dry-run'])]:
    output=WORK/(name+'.mkv');report=WORK/(name+'.json');log=WORK/(name+'.log')
    p=subprocess.run([str(BIN),'--report',str(report),'--progress','jsonl','--hybrid','--delete-sources',*flags,'-o',str(output),*map(str,inputs)],env=env,capture_output=True,text=True,timeout=30)
    log.write_text(p.stdout+'\n'+p.stderr)
    assert p.returncode!=0 and not output.exists(),name
    assert 'Profile 5 hybrid is disabled' in p.stdout+p.stderr,name
    data=json.loads(report.read_text());assert data['execution']=='failed' and data['validation']=='fail',name
    events=[json.loads(s) for s in p.stdout.splitlines() if s.startswith('{')]
    assert not any(e.get('event')=='completed' for e in events),name
    assert data['checks']==[e for e in events if e.get('event')=='check_result'],name
    replays.append(dict(name='p5-disabled-'+name,log=str(log),report=str(report),exit_status=p.returncode))
    results[name]='rejected; no output or source removal'
for flag in ['--analyze-pair','--probe-color-backend']:
    p=subprocess.run([str(BIN),flag,str(inputs[0])],env=env,capture_output=True,text=True,timeout=10)
    assert p.returncode and 'Unknown flag' in p.stderr+p.stdout,flag
    results[flag]='not available on main'
assert before=={str(p):digest(p) for p in inputs}
manifest=os.environ.get('DV8_APP_REPLAY_MANIFEST')
if manifest:
    path=Path(manifest);path.write_text(json.dumps(json.loads(path.read_text())+replays,indent=2))
(WORK/'summary.json').write_text(json.dumps(dict(controls=results,source_sha256_before_and_after=before),indent=2)+'\n')
print('P5-disabled main controls passed:',WORK,json.dumps(results))
