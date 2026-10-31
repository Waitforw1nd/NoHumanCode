import hashlib,json,os
from pathlib import Path
run=Path(__file__).resolve().parent; root=run/'tmp'; entries=[]; errors=[]
for current,dirs,files in os.walk(root,followlinks=False):
 dirs.sort()
 for name in sorted(files):
  p=Path(current)/name
  try:
   if p.is_symlink(): entries.append({'path':p.relative_to(root).as_posix(),'symlink':os.readlink(p)});continue
   h=hashlib.sha256()
   with p.open('rb') as f:
    for chunk in iter(lambda:f.read(1048576),b''):h.update(chunk)
   entries.append({'path':p.relative_to(root).as_posix(),'bytes':p.stat().st_size,'sha256':h.hexdigest()})
  except OSError as e:errors.append({'path':p.relative_to(root).as_posix(),'error':type(e).__name__})
(run/'logs/tmp-inventory.json').write_text(json.dumps({'root':'tmp','files':entries,'errors':errors},ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'files':len(entries),'errors':errors},ensure_ascii=False))
