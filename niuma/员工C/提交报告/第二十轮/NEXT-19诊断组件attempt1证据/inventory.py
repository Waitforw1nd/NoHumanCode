import hashlib,json,os,sys
from pathlib import Path
run=Path(__file__).resolve().parent
repo=run.parents[3]
roots=[repo/'.local',repo/'NoManCode/rust-app/.local']
entries=[]; errors=[]
for root in roots:
 if not root.exists(): continue
 for current,dirs,files in os.walk(root,followlinks=False):
  current=Path(current)
  dirs[:]=sorted(d for d in dirs if current/d != run and d != 'target')
  for name in sorted(files):
   p=current/name
   try:
    if p.is_symlink(): entries.append({'path':p.relative_to(repo).as_posix(),'symlink':os.readlink(p)}); continue
    h=hashlib.sha256()
    with p.open('rb') as f:
     for block in iter(lambda:f.read(1048576),b''):h.update(block)
    entries.append({'path':p.relative_to(repo).as_posix(),'bytes':p.stat().st_size,'sha256':h.hexdigest()})
   except OSError as e: errors.append({'path':p.relative_to(repo).as_posix(),'error':type(e).__name__})
(run/'logs'/(sys.argv[1]+'.json')).write_text(json.dumps({'roots':[str(p.relative_to(repo)) for p in roots],'files':entries,'errors':errors},ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'files':len(entries),'errors':errors},ensure_ascii=False))
