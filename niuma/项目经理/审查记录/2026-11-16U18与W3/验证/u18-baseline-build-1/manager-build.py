"""Build fixed unbound Host/foreign worker inputs for W3 negative controls."""
from pathlib import Path
import hashlib,json,os,sys
root=Path(__file__).resolve().parent.parent
sys.path.insert(0,str(root/'NoManCode/rust-app/tools/lifecycle'))
from lifecycle_gates import ENVIRONMENT,Gate,run_attempt,snapshot,git_text,write_new
expected=dict(head='1d09a551199292df0b719f6faa99f3799e02afef',product_tree='dec9a1e0fc0fbd0a0822dc49b41124015c4202fe',rust_tree='a4cee432f0a2be0d326519840881d8c7d1b12b26')
out=root/'.local/lifecycle/u18-baseline-build-1'
target=root.parent/'nhc-lifecycle-20261116-integration/.local/lifecycle-target'
out.parent.mkdir(parents=True,exist_ok=True)
# run_attempt requires a new attempt directory, so create TMP only via its parent elsewhere.
temp=root/'.local/u18-build-tmp'
temp.mkdir(parents=True,exist_ok=True)
env=dict(os.environ,**ENVIRONMENT,CARGO_TARGET_DIR=str(target),TMP=str(temp),TEMP=str(temp))
env.pop('NHC_PACKAGED_WORKER_SHA256',None)
invoke=root/'.local/u18-baseline-invoke.ps1'
steps=[dict(name='baseline-bins',argv=['pwsh','-NoProfile','-File',str(invoke)],command=['cargo','build','--bins','--offline','--locked','--message-format=json-render-diagnostics'],timeout_seconds=1800)]
gate=root.parent/'fufu/.git/nhc-heavy-window.lock'
# The helper bytes are outside product input; record them independently before running.
record=root/'.local/u18-baseline-helper-inputs.json'
write_new(record,dict(expected=expected,host_digest_absent=True,files=[dict(path=p.relative_to(root).as_posix(),sha256=hashlib.sha256(p.read_bytes()).hexdigest()) for p in [Path(__file__),invoke]]))
code,manifest=run_attempt(root,out,gate,expected,steps,env,dict(mode='baseline-artifact-build',purpose='Real unbound Host and foreign worker for W3 negative controls; no product tests'))
if code:sys.exit(code)
log=(out/'01-baseline-bins.log').read_text(encoding='utf-8-sig')
messages=[]
for line in log.splitlines():
 try: messages.append(json.loads(line))
 except json.JSONDecodeError: pass
assert any(m.get('reason')=='build-finished' and m.get('success') is True for m in messages)
fixed=root/'.local/u18-fixtures/baseline-binaries'
fixed.mkdir(parents=True,exist_ok=False)
artifacts=[]
for name,source in [('peachsh','src/main.rs'),('peachsh-wasm-worker','src/bin/peachsh-wasm-worker.rs')]:
 matches=[m for m in messages if m.get('reason')=='compiler-artifact' and m.get('target',{}).get('name')==name and m.get('executable')]
 assert len(matches)==1,(name,len(matches))
 m=matches[0];assert Path(m['target']['src_path']).resolve()==(root/'NoManCode/rust-app'/source).resolve()
 src=Path(m['executable']);raw=src.read_bytes();dest=fixed/(name+'.exe')
 with dest.open('xb') as stream:stream.write(raw);stream.flush();os.fsync(stream.fileno())
 assert dest.read_bytes()==raw
 artifacts.append(dict(name=name,source=src.as_posix(),fixed=dest.relative_to(root).as_posix(),bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest(),cargo_target_source=m['target']['src_path']))
assert snapshot(root)['head']==expected['head']
write_new(root/'.local/u18-baseline-selection.json',dict(candidate=expected,purpose='Unbound Host and foreign real worker; not a trusted packaged pair',host_digest_absent=True,artifacts=artifacts))
print(json.dumps(dict(status='passed',fixed_inputs=len(artifacts),gate_retained=True)))
