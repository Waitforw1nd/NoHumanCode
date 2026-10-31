import sys, subprocess, json, hashlib, os
from pathlib import Path
run=Path(__file__).resolve().parent; work=run.parent.parent; repo=work.parent.parent
name=sys.argv[1]
paths=subprocess.check_output(['git','-C',str(repo),'ls-files','-z','--','NoManCode']).split(b'\0')
entries=[]; errors=[]
for raw in filter(None,paths):
    path=raw.decode('utf-8'); full=repo/path
    try:
        data=full.read_bytes(); blob=subprocess.check_output(['git','-C',str(repo),'rev-parse',f'HEAD:{path}']).decode().strip(); actual=subprocess.check_output(['git','-C',str(repo),'hash-object',f'--path={path}',str(full)]).decode().strip()
        entries.append({'path':path,'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'blob':blob,'worktree_git_blob':actual,'matches_blob':actual==blob})
    except Exception as e: errors.append({'path':path,'error':str(e)})
out={'files':entries,'errors':errors}
(run/'logs'/f'{name}.json').write_text(json.dumps(out,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'count':len(entries),'mismatch':sum(not e['matches_blob'] for e in entries),'errors':errors}))

if errors or any(not e['matches_blob'] for e in entries): sys.exit(1)
