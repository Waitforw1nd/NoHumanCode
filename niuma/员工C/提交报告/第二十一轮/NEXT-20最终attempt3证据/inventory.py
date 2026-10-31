import hashlib,json,os,sys
from pathlib import Path
run=Path(__file__).resolve().parent;repo=run.parents[3];trees=[('current',repo),('previous',repo.parent/'nhc-c-tool-call-workflow')];files=[];errors=[]
for label,tree in trees:
 for root in [tree/'.local',tree/'NoManCode/rust-app/.local']:
  if not root.exists():continue
  for cur,dirs,names in os.walk(root,followlinks=False):
   cur=Path(cur);dirs[:]=sorted(d for d in dirs if cur/d != run and d!='target')
   for name in sorted(names):
    p=cur/name;rel=label+'/'+p.relative_to(tree).as_posix()
    try:
     if p.is_symlink():files.append({'path':rel,'symlink':os.readlink(p)});continue
     h=hashlib.sha256()
     with p.open('rb') as f:
      for block in iter(lambda:f.read(1048576),b''):h.update(block)
     files.append({'path':rel,'bytes':p.stat().st_size,'sha256':h.hexdigest()})
    except OSError as e:errors.append({'path':rel,'error':type(e).__name__})
(run/'logs'/(sys.argv[1]+'.json')).write_text(json.dumps({'files':files,'errors':errors},ensure_ascii=False,indent=2)+'\n',encoding='utf-8');print(json.dumps({'files':len(files),'errors':errors},ensure_ascii=False))
