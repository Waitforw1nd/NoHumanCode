import hashlib,json,re,sys
from pathlib import Path
r=Path(sys.argv[1]); out=Path(sys.argv[2]);
if out.exists(): raise RuntimeError('destination exists')
out.mkdir(parents=True)
files=[p for p in r.iterdir() if p.is_file() and p.suffix in {'.ps1','.py'}]+[p for p in (r/'logs').glob('*') if p.is_file()]
for p in (r/'tmp').rglob('*'):
 if p.is_file() and (p.name in {'case.txt','failure.txt','identity.txt','job-drop.txt','parent-exit.txt','sentinel.txt','sentinel-read.txt','controlled-exit.txt','ready-evidence.txt','cleanup.txt','process.json','host-process.json','legacy-process.json','proof-process.json','host-denied-process.json','lifecycle.jsonl','worktree-diagnostics.jsonl'} or p.name.startswith('child-') and p.suffix=='.log'):
  files.append(p)
rows=[]
for p in sorted(set(files)):
 try: text=p.read_bytes().decode('utf-8-sig')
 except UnicodeDecodeError: continue
 raw=p.read_bytes(); changes=[]
 for old,new,kind in [('E:\\\\code\\\\','../','e-json'),('../','../','e-native'),('../','../','e-slash'),('C:\\\\Program Files (x86)\\\\Windows Kits\\\\10','<WindowsSDK>','sdk-json'),('<WindowsSDK>','<WindowsSDK>','sdk'),('C:\\\\Program Files\\\\Microsoft Visual Studio\\\\2022\\\\Community','<VS2022>','vs-json'),('<VS2022>','<VS2022>','vs')]:
  n=text.count(old)
  if n: text=text.replace(old,new); changes.append({'kind':kind,'count':n})
 data=text.encode('utf-8'); rel=p.relative_to(r); dst=out/rel; dst.parent.mkdir(parents=True,exist_ok=True); dst.write_bytes(data); rows.append({'original':rel.as_posix(),'original_bytes':len(raw),'original_sha256':hashlib.sha256(raw).hexdigest(),'published':rel.as_posix(),'published_bytes':len(data),'published_sha256':hashlib.sha256(data).hexdigest(),'path_replacements':changes})
m={'original_root':'../nhc-d-default-composition/NoManCode/rust-app/.local/'+r.name,'entries':rows,'notes':'Text paths relative or environment placeholders; original databases and fixtures retained and inventoried; BOM removed.'}; mp=out/'manifest.json'; mp.write_text(json.dumps(m,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
for x in rows:
 a=(r/x['original']).read_bytes();b=(out/x['published']).read_bytes();assert len(a)==x['original_bytes'] and hashlib.sha256(a).hexdigest()==x['original_sha256'] and len(b)==x['published_bytes'] and hashlib.sha256(b).hexdigest()==x['published_sha256']
print(len(rows),hashlib.sha256(mp.read_bytes()).hexdigest())
