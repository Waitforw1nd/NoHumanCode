import hashlib,json,sys
from pathlib import Path
run=Path(__file__).resolve().parent
repo=run.parents[3]
out=Path(sys.argv[1]).resolve()
if out.exists(): raise RuntimeError('published directory exists')
out.mkdir(parents=True)
sha=lambda data:hashlib.sha256(data).hexdigest()
replacements=[('../nhc-d-tool-result-tests','../nhc-d-tool-result-tests'),('[msvc]','[msvc]'),('[windows-sdk]','[windows-sdk]')]
files=[]
sources=[run/'run.ps1',run/'inventory.py',run/'export.py']+sorted((run/'logs').glob('*'))
scene_specs=[('t05-before','tmp/nhc-t05-Htepo8/process.json','raw'),('t05-after','tmp/nhc-t05-CzJdlw/process.json','raw'),('legacy','tmp/.tmpen70xa/legacy-process.json','legacy-raw'),('proof-denied','tmp/.tmpRaXbRa/proof-process.json','proof-raw'),('proof-cancelled','tmp/.tmp1w0R5r/proof-process.json','proof-raw'),('host-denied','tmp/.tmpAp3iwF/host-denied-process.json','host-denied-raw')]
scenes=[]
for label,rel,rawdir in scene_specs:
    path=run/rel
    data=json.loads(path.read_text(encoding='utf-8-sig'))
    for entry in data['files']:
        raw=path.parent/rawdir/entry['name']
        assert raw.exists()==entry['present'],str(raw)
        if entry['present']:
            payload=raw.read_bytes()
            assert len(payload)==entry['bytes'] and sha(payload)==entry['sha256'],str(raw)
    scenes.append({'name':label,'process_file':path.relative_to(repo).as_posix(),'raw_dir':(path.parent/rawdir).relative_to(repo).as_posix(),'raw_files_verified':data['files'],'pid':data['pid'],'task':data['task'],'call':data['call'],'phase':data['phase'],'kill':data['kill'],'wait':data['wait'],'reaped':data['reaped']})
    sources.append(path)
    for f in sorted(path.parent.glob('*.log')): sources.append(f)
for source in sources:
    if not source.is_file(): continue
    payload=source.read_bytes()
    text=payload.decode('utf-8-sig')
    counts=[]
    for original,replacement in replacements:
        for style,needle in [('escaped',original.replace('\\','\\\\')),('ordinary',original)]:
            count=text.count(needle)
            if count:
                text=text.replace(needle,replacement)
                counts.append({'replacement':replacement,'form':style,'occurrences':count})
    name=source.name
    if source.parent.name!='logs' and source.parent!=run: name=source.parent.name+'-'+name
    dest=out/name
    if dest.exists(): raise RuntimeError('duplicate '+name)
    published=text.encode('utf-8')
    dest.write_bytes(published)
    files.append({'file':name,'source':source.relative_to(repo).as_posix(),'source_sha256':sha(payload),'source_bytes':len(payload),'published_sha256':sha(published),'published_bytes':len(published),'path_substitutions':counts})
manifest={'candidate':'3a1ea4f82b21413b98e5f8f0236e04e2b72067ef','attempt':1,'note':'Only text copies are path-relativized; raw crash evidence remains in the independent worktree. Raw DB files were not opened for recovery. Toolchain aliases are descriptive. Runner console and initial setup exception were not captured as original files.','files':files,'scenes':scenes}
(out/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
for row in files: assert sha((out/row['file']).read_bytes())==row['published_sha256']
print(json.dumps({'published_files':len(files),'scenes':len(scenes),'manifest_sha256':sha((out/'manifest.json').read_bytes())},ensure_ascii=False))
