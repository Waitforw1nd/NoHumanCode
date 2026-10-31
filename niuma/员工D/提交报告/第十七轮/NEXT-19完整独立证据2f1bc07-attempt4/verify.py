import json,sys,hashlib,pathlib
r=pathlib.Path(sys.argv[1]); a=json.loads((r/'logs/retained-before.json').read_text(encoding='utf-8-sig')); b=json.loads((r/'logs/retained-after.json').read_text(encoding='utf-8-sig')); x={v['path']:v for v in a['files']}; y={v['path']:v for v in b['files']}; missing=[p for p in x if p not in y]; changed=[p for p in x if p in y and x[p]!=y[p]]; checks=[]
for folder,meta,raw in [('nhc-t05-FJ29Kq','process.json','raw'),('nhc-t05-ARhXJT','process.json','raw'),('.tmpVBthyZ','legacy-process.json','legacy-raw'),('.tmpKKktEB','proof-process.json','proof-raw'),('.tmpMRLyzI','proof-process.json','proof-raw'),('.tmpSGMRwv','host-denied-process.json','host-denied-raw'),('nhc-result-host-YVUlDV','host-process.json','raw')]:
 d=r/'tmp'/folder; m=json.loads((d/meta).read_text()); files=[]
 for f in m['files']:
  p=d/raw/f['name']; match=p.exists()==f['present']
  if f['present'] and p.exists():
   data=p.read_bytes(); match &= hashlib.sha256(data).hexdigest()==f['sha256'] and ('bytes' not in f or len(data)==f['bytes'])
  files.append({'name':f['name'],'match':match})
 checks.append({'folder':folder,'pid':m['pid'],'phase':m.get('phase'),'kill':m['kill'],'reaped':m['reaped'],'files':files})
result={'before':len(x),'after':len(y),'missing':missing,'changed':changed,'errors_before':a['errors'],'errors_after':b['errors'],'raw_process_checks':checks}; (r/'logs/inventory-comparison.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8'); print(json.dumps(result,ensure_ascii=False)); sys.exit(bool(missing or changed or a['errors'] or b['errors'] or any(not c['reaped'] or c['kill']!='ok' or any(not f['match'] for f in c['files']) for c in checks)))
