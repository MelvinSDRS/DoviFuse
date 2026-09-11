#!/usr/bin/env python3
"""Disposable remux checks with independently authored tracks and metadata.
Extracted audio/subtitle/attachment bytes, chapter XML, and timestamps must survive.
"""
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import unicodedata
import wave
import xml.etree.ElementTree as ET

ROOT=Path(__file__).resolve().parents[1]
WORK=Path(tempfile.mkdtemp(prefix='dv8-preservation-',dir='/tmp'))
RESOURCES=Path(os.environ.get('DV8_AUDIT_RESOURCES',ROOT))
BIN=Path(os.environ.get('DV8_AUDIT_BIN',ROOT/'dv8_converter/target/debug/dv8_converter'))
SEEDS=Path(os.environ['DV8_AUDIT_FIXTURES'])
ENV=dict(os.environ,DV8_SCRIPT_DIR=str(RESOURCES),DV8_PROCESSING_LOG_FILE=str(WORK/'processing.log'))

def require(condition,message):
    if not condition:raise RuntimeError(message)

def run(args,name,expected=0):
    p=subprocess.run([str(a) for a in args],env=ENV,capture_output=True,text=True,timeout=180)
    (WORK/(name+'.log')).write_text(p.stdout+'\n'+p.stderr)
    require((p.returncode==0)==(expected==0),f'{name}: exit {p.returncode}; see {WORK}')
    return p

def manifest(path):
    return json.loads(run(['mkvmerge','-J',path],path.stem+'-identify').stdout)

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

manifest_seed=json.loads((SEEDS/'manifest.json').read_text())
for name in ['p7.mkv','hdr.mkv','dv.mkv']:
    require(digest(SEEDS/name)==manifest_seed['sha256'][name],'Seed integrity '+name)
with wave.open(str(WORK/'audio.wav'),'wb') as w:
    w.setnchannels(1);w.setsampwidth(2);w.setframerate(8000)
    w.writeframes(b''.join(((i*37)%6000-3000).to_bytes(2,'little',signed=True) for i in range(8000*6)))
(WORK/'subtitles.srt').write_text('1\n00:00:01,000 --> 00:00:02,000\nPréservation — test\n\n2\n00:00:04,000 --> 00:00:05,500\nDeuxième réplique\n')
(WORK/'attachment.bin').write_bytes(b'DV8 preservation attachment\x00\xff')
(WORK/'chapters.xml').write_text('''<?xml version="1.0"?><Chapters><EditionEntry><EditionUID>101</EditionUID><ChapterAtom><ChapterUID>102</ChapterUID><ChapterTimeStart>00:00:00.000000000</ChapterTimeStart><ChapterDisplay><ChapterString>Début</ChapterString><ChapterLanguage>fre</ChapterLanguage></ChapterDisplay></ChapterAtom><ChapterAtom><ChapterUID>103</ChapterUID><ChapterTimeStart>00:00:04.000000000</ChapterTimeStart><ChapterDisplay><ChapterString>Suite</ChapterString><ChapterLanguage>fre</ChapterLanguage></ChapterDisplay></ChapterAtom></EditionEntry></Chapters>''')
video_options=['--language','0:fr-CA','--track-name','0:Image cinéma','--default-track-flag','0:no','--forced-display-flag','0:yes','--track-enabled-flag','0:no','--hearing-impaired-flag','0:yes','--visual-impaired-flag','0:yes','--text-descriptions-flag','0:yes','--original-flag','0:yes','--commentary-flag','0:yes','--display-dimensions','0:1024x576','--stereo-mode','0:1']
for label,seed in [('standard','p7.mkv'),('hybrid','hdr.mkv')]:
    run(['mkvmerge','-o',WORK/(label+'.mkv'),'--title','Film de test — conservation','--chapters',WORK/'chapters.xml',
         '--attachment-mime-type','application/octet-stream','--attachment-name','test.bin','--attachment-description','Pièce jointe',
         '--attach-file',WORK/'attachment.bin','--language','0:fr-CA','--track-name','0:Son original',WORK/'audio.wav',
         *video_options,SEEDS/seed,'--language','0:fr-CA','--forced-display-flag','0:yes','--track-name','0:Sous-titres',WORK/'subtitles.srt',
         '--track-order','0:0,1:0,2:0'],'author-'+label)

def snapshot(path,label):
    m=manifest(path); result={'track_headers':[],'streams':[],'timestamps':[],'attachments':[]}
    for track in m['tracks']:
        properties=track['properties']
        keys=['codec_id','language','language_ietf','track_name','default_track','forced_track','enabled_track',
              'flag_hearing_impaired','flag_visual_impaired','flag_text_descriptions','flag_original','flag_commentary']
        if track['type']=='video':keys+=['display_dimensions','display_unit','stereo_mode','pixel_dimensions']
        else:keys+=['uid','audio_channels','audio_sampling_frequency','codec_private_data']
        result['track_headers'].append({'type':track['type'],**{k:properties[k] for k in keys if k in properties}})
        tid=track['id'];out=WORK/f'{label}-{tid}.timestamps'
        run(['mkvextract',path,'timestamps_v2',f'{tid}:{out}'],f'{label}-{tid}-timestamps')
        result['timestamps'].append([float(x) for x in out.read_text().splitlines() if x and not x.startswith('#')])
        if track['type']!='video':
            out=WORK/f'{label}-{tid}.track'
            run(['mkvextract',path,'tracks',f'{tid}:{out}'],f'{label}-{tid}-extract')
            result['streams'].append(digest(out))
        else:
            out=WORK/f'{label}-video.hevc'
            run(['mkvextract',path,'tracks',f'{tid}:{out}'],label+'-extract-video')
            # Ignore Annex-B framing, DV RPU/EL and optional trailing padding;
            # compare the original base-layer coded picture NAL payloads.
            units=re.split(b'\x00\x00\x00?\x01',out.read_bytes())
            picture_hash=hashlib.sha256();count=0
            for nal in units:
                nal=nal.rstrip(b'\x00')
                if len(nal)<2:continue
                nal_type=(nal[0]>>1)&63;layer=((nal[0]&1)<<5)|(nal[1]>>3)
                if nal_type<=31 and layer==0:
                    picture_hash.update(len(nal).to_bytes(8,'little'));picture_hash.update(nal);count+=1
            require(count>0,'No base-layer coded pictures extracted')
            result['base_layer_vcl']={'count':count,'sha256':picture_hash.hexdigest()}
    for attachment in m.get('attachments',[]):
        out=WORK/f'{label}-{attachment["id"]}.attachment'
        run(['mkvextract',path,'attachments',f'{attachment["id"]}:{out}'],label+'-attachment')
        result['attachments'].append({'name':attachment['file_name'],'mime':attachment['content_type'],'sha256':digest(out)})
    result['chapters']=run(['mkvextract',path,'chapters'],label+'-chapters').stdout
    result['title']=m['container']['properties']['title']
    return result

results={}
for label in ['standard','hybrid']:
    source=WORK/(label+'.mkv');before=snapshot(source,label+'-before');source_hash=digest(source)
    video=before['track_headers'][1]
    require(video['type']=='video' and video['language_ietf']=='fr-CA' and unicodedata.normalize('NFC',video['track_name'])=='Image cinéma','Authored video headers missing')
    require(video['display_dimensions']=='1024x576' and video['stereo_mode']==1 and video['default_track'] is False and video['forced_track'] is True,'Authored display/flags missing')
    require(before['attachments'][0]['sha256']==digest(WORK/'attachment.bin'),'Authored attachment changed')
    with wave.open(str(WORK/'audio.wav'),'rb') as expected, wave.open(str(WORK/f'{label}-before-0.track'),'rb') as actual:
        require(expected.readframes(expected.getnframes())==actual.readframes(actual.getnframes()),'Authored PCM samples changed')
    require('Préservation — test' in (WORK/f'{label}-before-2.track').read_text(encoding='utf-8-sig'),'Authored subtitle missing')
    require('Début' in before['chapters'] and 'Suite' in before['chapters'],'Authored chapters missing')
    shutil.copyfile(source,WORK/(label+'-original.mkv'))
    output=source if label=='standard' else WORK/'result.mkv'
    command=[BIN,'--progress','jsonl','--hwaccel','off']
    command+=['-n',source] if label=='standard' else ['--hybrid','-o',output,SEEDS/'dv.mkv',source]
    events=[json.loads(line) for line in run(command,'convert-'+label).stdout.splitlines() if line.startswith('{')]
    require(any(e['event']=='completed' for e in events),'Missing conversion completion')
    after=snapshot(output,label+'-after')
    for key in ['track_headers','streams','attachments','chapters','title','base_layer_vcl']:
        require(before[key]==after[key],f'{label}: changed {key}')
    for a,b in zip(before['timestamps'],after['timestamps']):
        require(len(a)==len(b) and all(abs(x-y)<=1 for x,y in zip(a,b)),label+': timestamps changed')
    if label=='hybrid':require(digest(source)==source_hash,'Hybrid source modified')
    results[label]={'metadata':'preserved','audio_subtitles_attachments_sha256':'matched','base_layer_vcl':after['base_layer_vcl'],'chapters_xml':'matched','track_timestamps':'within 1ms','tracks':len(after['track_headers'])}

# MakeMKV inputs intentionally trigger MKVToolNix's UID regeneration. Verify
# payloads and the actual chapter/tag references, rather than discarding UID
# comparisons for every container or requiring MakeMKV's old numeric values.
propedit = shutil.which('mkvpropedit') or str(RESOURCES/'vendor/MKVToolNix/mkvpropedit')
version=run(['mkvmerge','--version'],'makemkv-tool-version').stdout
major=int(re.search(r'\bv(\d+)\.',version).group(1))
# Automatic MakeMKV track-UID regeneration was introduced with v84. The
# Ubuntu 24.04 suite also covers v82, which legitimately retains those UIDs.
regenerates_uids=major>=84
for label in ['standard', 'hybrid']:
    source=WORK/(label+'-makemkv.mkv')
    shutil.copyfile(WORK/(label+'-original.mkv'),source)
    audio_uid=manifest(source)['tracks'][0]['properties']['uid']
    chapters=ET.fromstring((WORK/'chapters.xml').read_text())
    for atom in chapters.iter('ChapterAtom'):
        ET.SubElement(ET.SubElement(atom,'ChapterTrack'),'ChapterTrackNumber').text=str(audio_uid)
    chapter_file=WORK/(label+'-makemkv-chapters.xml')
    chapter_file.write_text('<?xml version="1.0"?>\n'+ET.tostring(chapters,encoding='unicode'))
    tags=WORK/(label+'-makemkv-tags.xml')
    tags.write_text(f'<?xml version="1.0"?>\n<Tags><Tag><Targets><TrackUID>{audio_uid}</TrackUID></Targets><Simple><Name>AUDIT_LABEL</Name><String>preserve-audio</String></Simple></Tag><Tag><Targets><ChapterUID>103</ChapterUID></Targets><Simple><Name>AUDIT_CHAPTER</Name><String>preserve-chapter</String></Simple></Tag></Tags>')
    run([propedit,source,'--chapters',chapter_file,'--tags','all:'+str(tags),
         '--edit','info','--set','writing-application=MakeMKV v1.15.3 darwin(x64-release)'],label+'-makemkv-author')
    before=snapshot(source,label+'-makemkv-before');source_hash=digest(source)
    output=source if label=='standard' else WORK/'makemkv-hybrid-output.mkv'
    command=[BIN,'--progress','jsonl','--hwaccel','off']
    command+=['-n',source] if label=='standard' else ['--hybrid','-o',output,SEEDS/'dv.mkv',source]
    run(command,label+'-makemkv-convert')
    after=snapshot(output,label+'-makemkv-after')
    new_uid=manifest(output)['tracks'][0]['properties']['uid']
    require((new_uid!=audio_uid)==regenerates_uids,'Unexpected MakeMKV UID behavior for '+version.strip())
    for value in [before,after]:
        for track in value['track_headers']: track.pop('uid',None)
    for key in ['track_headers','streams','attachments','title','base_layer_vcl']:
        require(before[key]==after[key],label+': MakeMKV changed '+key)
    for a,b in zip(before['timestamps'],after['timestamps']):
        require(len(a)==len(b) and all(abs(x-y)<=1 for x,y in zip(a,b)),label+': MakeMKV timestamps changed')
    normalized=[]
    for value,uid in [(before,audio_uid),(after,new_uid)]:
        tree=ET.fromstring(value['chapters'])
        for node in tree.iter('ChapterTrackNumber'):
            require(node.text==str(uid),label+': chapter track reference was not remapped')
            node.text='audio-track'
        for name in ['EditionUID','ChapterUID']:
            for index,node in enumerate(tree.iter(name)): node.text=str(index)
        normalized.append(ET.tostring(tree))
    require(normalized[0]==normalized[1],label+': MakeMKV chapter structure/content changed')
    output_chapters=ET.fromstring(after['chapters'])
    chapter_uids={atom.findtext('ChapterDisplay/ChapterString'):atom.findtext('ChapterUID') for atom in output_chapters.iter('ChapterAtom')}
    output_tags=ET.fromstring(run(['mkvextract',output,'tags'],label+'-makemkv-tags').stdout)
    observed={simple.findtext('Name'):(simple.findtext('String'),tag.findtext('Targets/TrackUID'),tag.findtext('Targets/ChapterUID')) for tag in output_tags.findall('Tag') for simple in tag.findall('Simple') if simple.findtext('Name','').startswith('AUDIT_')}
    require(observed=={'AUDIT_LABEL':('preserve-audio',str(new_uid),None),
                       'AUDIT_CHAPTER':('preserve-chapter',None,chapter_uids['Suite'])},label+': MakeMKV tag targets/content changed')
    if label=='hybrid':require(digest(source)==source_hash,'MakeMKV hybrid source changed')
    results[label+'-makemkv']={'payloads':'matched','mkvmerge_version':version.strip(),
                              'track_uids':'regenerated as documented' if regenerates_uids else 'retained by MKVToolNix before v84',
                              'chapter_and_tag_references':'remapped correctly','chapter_content':'preserved'}

# A successful mux that loses a header must still fail before source replacement/deletion.
import sys
real_mkvmerge=shutil.which('mkvmerge',path=ENV['PATH'])
require(real_mkvmerge is not None,'Missing mkvmerge')
fake=WORK/'fault-tools';fake.mkdir()
wrapper=fake/'mkvmerge'
wrapper.write_text('#!'+sys.executable+'\nimport subprocess,sys\nargs=sys.argv[1:]\n'
                   'if any(a.endswith(".hevc") for a in args):\n'
                   ' for i,a in enumerate(args[:-1]):\n'
                   '  if a=="--language": args[i+1]="0:en"\n'
                   'sys.exit(subprocess.call(['+repr(real_mkvmerge)+']+args))\n')
wrapper.chmod(0o755)
ENV['PATH']=str(fake)+os.pathsep+ENV['PATH']
for label in ['standard','hybrid']:
    source=WORK/(label+'-guard.mkv');shutil.copyfile(WORK/(label+'-original.mkv'),source)
    source_hash=digest(source)
    donor=WORK/'guard-donor.mkv'
    if not donor.exists():shutil.copyfile(SEEDS/'dv.mkv',donor)
    donor_hash=digest(donor)
    output=WORK/'guard-output.mkv'
    command=[BIN,'--progress','jsonl','--hwaccel','off']
    command+=['-n',source] if label=='standard' else ['--hybrid','--delete-sources','-o',output,donor,source]
    p=run(command,'lost-header-'+label,expected=1)
    require('Remux changed track' in p.stdout+p.stderr,label+': wrong guard failure')
    require(digest(source)==source_hash and digest(donor)==donor_hash,label+': source changed after failed preservation')
    require('"event":"completed"' not in p.stdout,label+': false success')
    if label=='standard':require(not (WORK/'standard-guard.DV8_TMP.mkv').exists(),'Temporary standard output retained')
    else:require((WORK/'guard-output.FAILED.mkv').exists(),'Failed hybrid was not retained for inspection')
    results[label+'-lost-header']='rejected; sources preserved'
(WORK/'summary.json').write_text(json.dumps(results,indent=2)+'\n')
print('Preservation passed:',WORK,json.dumps(results))
