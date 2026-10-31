import sys, subprocess, json, hashlib, os
from pathlib import Path
run=Path(__file__).resolve().parent; work=run.parent.parent; repo=work.parent.parent
name=sys.argv[1]
paths=subprocess.check_output(['git','-C',str(repo),'ls-files','-z','--','NoManCode/rust-app']).split(b'\0')
entries=[]; errors=[]
for raw in filter(None,paths):
    path=raw.decode('utf-8'); full=repo/path
    try:
        data=full.read_bytes(); blob=subprocess.check_output(['git','-C',str(repo),'show',f'HEAD:{path}'])
        entries.append({'path':path,'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'blob_sha256':hashlib.sha256(blob).hexdigest(),'matches_blob':data==blob})
    except Exception as e: errors.append({'path':path,'error':str(e)})
out={'files':entries,'errors':errors}
(run/'logs'/f'{name}.json').write_text(json.dumps(out,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'count':len(entries),'mismatch':sum(not e['matches_blob'] for e in entries),'errors':errors}))
