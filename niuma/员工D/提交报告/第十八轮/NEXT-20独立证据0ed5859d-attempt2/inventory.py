import sys, json, hashlib, os
from pathlib import Path
run=Path(__file__).resolve().parent; work=run.parent.parent
roots=[Path('../nhc-d-tool-result-tests/NoManCode/rust-app/.local')/x for x in ['next19-d-3a1ea4f-attempt1/tmp','next17-a10-evidence','next19-d-386db7a-s06-attempt1','next19-d-8e4f6c5-s06-attempt2','next19-d-82865ba-s06-attempt3','next19-d-cbdba68-independent-attempt2','next19-d-81b9b2b-independent-attempt3','next19-d-2f1bc07-independent-attempt4']]+[Path('../nhc-b-default-composition/NoManCode/rust-app/.local')/x for x in ['next20-b-46d5a0b3-attempt1','next20-b-de658989-attempt2']]+[work/'.local'/'next20-d-44391a64-attempt1',work/'.local'/'next17-a10-evidence',run/'tmp']
entries=[]; errors=[]
for root in roots:
    if not root.exists(): continue
    for current, dirs, files in os.walk(root,followlinks=False,onerror=lambda e:errors.append(str(e))):
        dirs.sort()
        for name in sorted(files):
            p=Path(current)/name
            try:
                if p.is_symlink(): entries.append({'path':str(p),'symlink':os.readlink(p)}); continue
                h=hashlib.sha256()
                with p.open('rb') as f:
                    for block in iter(lambda:f.read(1048576),b''):h.update(block)
                entries.append({'path':str(p),'bytes':p.stat().st_size,'sha256':h.hexdigest()})
            except OSError as e:errors.append({'path':str(p),'error':str(e)})
(run/'logs'/f'{sys.argv[1]}.json').write_text(json.dumps({'files':entries,'errors':errors},ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'count':len(entries),'errors':errors}))




