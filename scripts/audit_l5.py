#!/usr/bin/env python3
"""Independent authored geometry, two-pass L5 boundaries, and output-loss guard."""
import array
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT=Path(__file__).resolve().parents[1]
WORK=Path(tempfile.mkdtemp(prefix='dv8-l5-audit-',dir='/tmp'))
RES=Path(os.environ.get('DV8_AUDIT_RESOURCES',ROOT))
BIN=Path(os.environ.get('DV8_AUDIT_BIN',ROOT/'dv8_converter/target/debug/dv8_converter'))
L5=Path(os.environ.get('DV8_L5_TIMELINE_BIN',ROOT/'dv8_converter/target/debug/examples/l5_timeline'))
SEEDS=Path(os.environ['DV8_AUDIT_FIXTURES'])
ENV=dict(os.environ,DV8_SCRIPT_DIR=str(RES),DV8_PROCESSING_LOG_FILE=str(WORK/'processing.log'))
ENV['PATH']=ENV.get('PATH','')+os.pathsep+str(RES/'tools')

def require(v,message):
    if not v:raise RuntimeError(message+'; see '+str(WORK))

def run(args,name,success=True,env=None):
    p=subprocess.run([str(a) for a in args],env=env or ENV,capture_output=True,text=True,timeout=180)
    (WORK/(name+'.log')).write_text(p.stdout+'\n'+p.stderr)
    require((p.returncode==0)==success,name+' unexpected exit '+str(p.returncode))
    return p

def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()

def write_json(name,value):
    p=WORK/name;p.write_text(json.dumps(value,indent=2)+'\n');return p

seed_manifest=json.loads((SEEDS/'manifest.json').read_text())
for name in ['dv.mkv','hdr.mkv']:
    require(sha(SEEDS/name)==seed_manifest['sha256'][name],'Seed changed')

# Mechanical target timeline is independently authored, including a one-frame
# interval. It is not inferred from the image detector under test.
expected={'crop':True,'presets':[{'id':4,'left':0,'right':0,'top':20,'bottom':20},{'id':9,'left':0,'right':0,'top':0,'bottom':0}],
          'edits':{'0-23':4,'24-24':9,'25-59':4,'60-89':9,'90-119':4}}
expected_path=write_json('expected.json',expected)
run([L5,expected_path,120,320,180,WORK/'target-config.json'],'compile-target-timeline')
run(['mkvextract',SEEDS/'dv.mkv','tracks','0:'+str(WORK/'dv.hevc')],'extract-seed')
run(['dovi_tool','extract-rpu',WORK/'dv.hevc','-o',WORK/'seed-rpu.bin'],'extract-seed-rpu')
trim=write_json('trim.json',{'mode':0,'remove':['120-1089']})
run(['dovi_tool','editor','-i',WORK/'seed-rpu.bin','-j',trim,'-o',WORK/'source-rpu.bin'],'trim-seed')
# Change length through remove+duplicate before applying TARGET frame indices.
align=write_json('alignment.json',{'mode':0,'remove':['117-119'],'duplicate':[{'source':0,'offset':0,'length':3}]})
run(['dovi_tool','editor','-i',WORK/'source-rpu.bin','-j',align,'-o',WORK/'aligned.bin'],'align')
run(['dovi_tool','editor','-i',WORK/'aligned.bin','-j',WORK/'target-config.json','-o',WORK/'l5.bin'],'apply-target-l5')
run(['dovi_tool','export','-i',WORK/'l5.bin','-d','level5='+str(WORK/'actual.json')],'export-l5')
run([L5,expected_path,120,320,180,WORK/'verified-config.json',WORK/'actual.json'],'verify-all-boundaries')
wrong=dict(expected);wrong['edits']={'0-24':4,'25-59':4,'60-89':9,'90-119':4}
write_json('lost-one-frame.json',wrong)
run([L5,expected_path,120,320,180,WORK/'must-not-exist.json',WORK/'lost-one-frame.json'],'one-frame-loss',False)
require(not (WORK/'must-not-exist.json').exists(),'Failed verification wrote an edit config')
# Aligning and target-range editing in ONE pass gives a different timeline.
wrong_order=json.loads((WORK/'target-config.json').read_text());wrong_order.update(json.loads(align.read_text()))
write_json('wrong-order.json',wrong_order)
run(['dovi_tool','editor','-i',WORK/'source-rpu.bin','-j',WORK/'wrong-order.json','-o',WORK/'wrong-order.bin'],'wrong-order-editor')
run(['dovi_tool','export','-i',WORK/'wrong-order.bin','-d','level5='+str(WORK/'wrong-order-export.json')],'wrong-order-export')
run([L5,expected_path,120,320,180,WORK/'wrong-order-accepted.json',WORK/'wrong-order-export.json'],'wrong-order-detected',False)

# Production guard: replace only L5 at injection, preserving picture/scene data.
real_dovi=shutil.which('dovi_tool',path=ENV['PATH']);require(real_dovi,'No dovi_tool')
fault=WORK/'fault-tools';fault.mkdir();wrapper=fault/'dovi_tool'
mutation=write_json('mutation.json',{'mode':0,'active_area':{'crop':True,'presets':[{'id':0,'left':0,'right':0,'top':10,'bottom':10}],'edits':{'all':0}}})
wrapper.write_text('#!'+sys.executable+'\nimport subprocess,sys\nargs=sys.argv[1:]\n'
    'if "inject-rpu" in args:\n'
    ' i=args.index("-r")+1\n'
    ' changed='+repr(str(WORK/'mutated-rpu.bin'))+'\n'
    ' subprocess.run(['+repr(real_dovi)+',"editor","-i",args[i],"-j",'+repr(str(mutation))+',"-o",changed],check=True)\n'
    ' args[i]=changed\n'
    'sys.exit(subprocess.call(['+repr(real_dovi)+']+args))\n')
wrapper.chmod(0o755)
donor=WORK/'donor.mkv';target=WORK/'target.mkv'
shutil.copyfile(SEEDS/'dv.mkv',donor);shutil.copyfile(SEEDS/'hdr.mkv',target)
hashes=[sha(donor),sha(target)]
fault_resources=WORK/'fault-resources';(fault_resources/'tools').mkdir(parents=True)
(fault_resources/'tools/dovi_tool').symlink_to(wrapper)
(fault_resources/'config').symlink_to(RES/'config',target_is_directory=True)
fault_env=dict(ENV,PATH=str(fault)+os.pathsep+ENV['PATH'],DV8_SCRIPT_DIR=str(fault_resources))
p=run([BIN,'--progress','jsonl','--hybrid','--letterbox','off','--delete-sources','-o',WORK/'output.mkv',donor,target],'output-l5-loss',False,fault_env)
require('Output L5 frame intervals differ' in p.stdout+p.stderr,'Wrong output L5 guard failure')
require([sha(donor),sha(target)]==hashes,'Sources changed on L5 failure')
require((WORK/'output.FAILED.mkv').exists() and '"event":"completed"' not in p.stdout,'False success or missing failed output')
# Even a successful mechanically verified run retains inputs while full picture
# area validation is unavailable.
p=run([BIN,'--progress','jsonl','--hybrid','--delete-sources','-o',WORK/'retained.mkv',donor,target],'inconclusive-retains-inputs')
require('"event":"completed"' in p.stdout and 'source files' in p.stdout,'No usable output/retention notice')
require([sha(donor),sha(target)]==hashes,'Unverified active-area run deleted sources')
require(any(e.get('key')=='active_area_picture' and e.get('status')=='inconclusive' for e in [json.loads(line) for line in p.stdout.splitlines() if line.startswith('{')]),'Missing machine-readable inconclusive result')
result={'two_pass_target_ranges':'verified including one-frame interval','single_pass_wrong_order':'rejected','one_frame_l5_loss':'rejected','output_l5_mutation':'rejected; sources retained; FAILED output kept','unverified_picture_area':'successful mechanical output retains both inputs','private_media_used':False,'automatic_active_area_acceptance':False}
(WORK/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('L5 audit passed:',WORK,json.dumps(result))
