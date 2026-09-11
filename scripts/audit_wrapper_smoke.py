#!/usr/bin/env python3
"""Exercise qBittorrent cleanup decisions with fake curl; no network or messages."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

root=Path(__file__).resolve().parents[1]
work=Path(tempfile.mkdtemp(prefix='dv8-wrapper-audit-'))
for rc in (1,0):
    case=work/str(rc);case.mkdir()
    tools=case/'tools';tools.mkdir()
    target=case/'movie.mkv';target.write_text('untouched')
    marker=case/'curl-calls'
    curl=tools/'curl'
    curl.write_text('#!/usr/bin/env python3\nimport sys\nfrom pathlib import Path\nwith Path('+repr(str(marker))+').open("a") as f: f.write(" ".join(sys.argv[1:])+"\\n")\nprint('+repr(json.dumps([{'hash':'fake','content_path':str(target),'save_path':str(case),'name':'movie.mkv'}]))+')\n')
    curl.chmod(0o755)
    launcher=case/'fake-converter'
    launcher.write_text('#!/bin/sh\necho "1 file(s) converted."\nexit '+str(rc)+'\n')
    launcher.chmod(0o755)
    env=dict(os.environ,PATH=str(tools)+os.pathsep+os.environ['PATH'],
             DV8_ENV_FILE=str(case/'no.env'),DV8_BASE_DIR=str(case),DV8_SCRIPT_PATH=str(launcher),
             DV8_RUN_DIR=str(case/'run'),DV8_TRIGGER_LOG_FILE=str(case/'index.log'),
             DV8_MEDIA_ROOTS=str(case/'no-media'),DV8_QBT_REMOVE_CONVERTED='true',
             DV8_AUTORUN_DRY_RUN='false',DV8_TELEGRAM_BOT_TOKEN='',DV8_TELEGRAM_CHAT_ID='',
             DV8_EL_RPU_DIR=str(case/'archive'),DV8_QBT_API_URL='http://fake.invalid')
    subprocess.run(['bash',str(root/'qbt_autorun_wrapper.sh'),str(target)],env=env,check=True,capture_output=True)
    deadline=time.monotonic()+10
    while time.monotonic()<deadline:
        if (case/'index.log').exists() and f'Completed rc={rc}' in (case/'index.log').read_text(): break
        time.sleep(.05)
    else: raise AssertionError('wrapper did not finish')
    if rc: assert not marker.exists(), 'partial failure contacted qBittorrent'
    else: assert '/torrents/delete' in marker.read_text(), 'success cleanup control did not run'
    assert target.read_text()=='untouched'
print(f'PASS: failed batch retains torrent; successful batch reaches fake cleanup; no network. {work}')
