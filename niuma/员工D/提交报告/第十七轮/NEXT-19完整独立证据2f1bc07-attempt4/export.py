from pathlib import Path
import hashlib,json
run=Path(__file__).resolve().parent
out=run.parents[4]/'fufu'/'niuma'/'员工D'/'提交报告'/'第十七轮'/'NEXT-19完整独立证据2f1bc07-attempt4'
if out.exists():raise RuntimeError('destination exists')
out.mkdir(parents=True)
files=[run/'run.ps1',run/'sources.py',run/'inventory.py',run/'verify.py',run/'export.py',*sorted((run/'logs').glob('*'))]
for p in sorted((run/'tmp').rglob('*')):
 if p.is_file() and (p.name in {'case.txt','lifecycle.jsonl','worktree-diagnostics.jsonl','process.json','legacy-process.json','proof-process.json','host-denied-process.json','host-process.json','first-host.stderr.log','first-host.stdout.log','second-host.stderr.log','second-host.stdout.log'} or (p.name.startswith('child-') and p.suffix=='.log')):
  files.append(p)
replacements=[('E:\\\\code\\\\nhc-d-tool-result-tests','../nhc-d-tool-result-tests','d-tree-json'),('../nhc-d-tool-result-tests','../nhc-d-tool-result-tests','d-tree'),('../nhc-d-tool-result-tests','../nhc-d-tool-result-tests','d-tree-slash'),('C:\\\\Program Files (x86)\\\\Windows Kits\\\\10','<WindowsSDK>','sdk-json'),('<WindowsSDK>','<WindowsSDK>','sdk'),('C:\\\\Program Files\\\\Microsoft Visual Studio\\\\2022\\\\Community','<VS2022>','vs-json'),('<VS2022>','<VS2022>','vs')]
rows=[]
for p in files:
 if not p.is_file():continue
 raw=p.read_bytes();text=raw.decode('utf-8-sig');changes=[]
 for old,new,name in replacements:
  count=text.count(old)
  if count:text=text.replace(old,new);changes.append({'kind':name,'count':count})
 data=text.encode('utf-8');rel=p.relative_to(run);dest=out/rel;dest.parent.mkdir(parents=True,exist_ok=True);dest.write_bytes(data)
 rows.append({'original':rel.as_posix(),'original_bytes':len(raw),'original_sha256':hashlib.sha256(raw).hexdigest(),'published':rel.as_posix(),'published_bytes':len(data),'published_sha256':hashlib.sha256(data).hexdigest(),'path_replacements':changes})
m={'original_root':'../nhc-d-tool-result-tests/NoManCode/rust-app/.local/next19-d-2f1bc07-independent-attempt4','entries':rows,'notes':'Text copies normalize local paths and remove UTF-8 BOM where present; original files untouched. Databases and other files remain in original fixture roots, recorded in retained-after.json.'}
p=out/'manifest.json';p.write_text(json.dumps(m,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
for row in rows:
 orig=(run/row['original']).read_bytes();pub=(out/row['published']).read_bytes()
 assert len(orig)==row['original_bytes'] and hashlib.sha256(orig).hexdigest()==row['original_sha256']
 assert len(pub)==row['published_bytes'] and hashlib.sha256(pub).hexdigest()==row['published_sha256']
print('count',len(rows),'verified',len(rows),'manifest_sha256',hashlib.sha256(p.read_bytes()).hexdigest())
