"""Local manager-only fixed artifact build; shares D's retained process supervisor."""
import hashlib, json, os, pathlib, sys
root = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(root/'NoManCode/rust-app/tools/lifecycle'))
import lifecycle_gates as gates

attempt = sys.argv[1]
if not gates.re.fullmatch(r'[a-z0-9-]+', attempt):
    raise ValueError('Invalid attempt')
out = root/'.local/lifecycle'/attempt
target = root/'.local/lifecycle-target'
temp = out/'tmp'
source = gates.snapshot(root)
expected = {key: source[key] for key in ('head','product_tree','rust_tree')}
helper = root/'.local/manager-fixed-invoke.ps1'
helper_before = helper.read_bytes()
env = dict(os.environ, **gates.ENVIRONMENT, CARGO_TARGET_DIR=str(target), TMP=str(temp), TEMP=str(temp))
steps = [dict(name=name, argv=['pwsh','-NoProfile','-File',str(helper),'-Stage',stage],
              command=command, timeout_seconds=600) for name,stage,command in [
    ('fixed-lib','lib',['cargo','test','--offline','--locked','--lib','--no-run','--message-format=json']),
    ('fixed-probe','probe',['cargo','build','--offline','--locked','--example','process_lifetime_probe','--message-format=json'])]]
def invoke(argv,cwd,environment,timeout,log):
    if helper.read_bytes()!=helper_before: raise ValueError('Local invocation helper changed')
    temp.mkdir(parents=True,exist_ok=True)
    target.mkdir(exist_ok=True)
    return gates.execute(argv,cwd,environment,timeout,log)
code,manifest=gates.run_attempt(root,out,pathlib.Path(gates.git_text(root,'rev-parse','--path-format=absolute','--git-common-dir'))/'nhc-heavy-window.lock',
    expected,steps,env,{'mode':'manager-fixed-build','helper_sha256':gates.digest(helper_before)},invoke=invoke)
if code==0:
    if helper.read_bytes()!=helper_before: raise ValueError('Local invocation helper drift')
    selected={'candidate':expected,'source_attempt':str(out.relative_to(root)),'helper_sha256':gates.digest(helper_before)}
    for index,name,kind in [(1,'peachsh','lib'),(2,'process_lifetime_probe','example')]:
        messages=[]
        for line in (out/manifest['steps'][index-1]['log']).read_bytes().splitlines():
            try: messages.append(json.loads(line))
            except (ValueError,UnicodeDecodeError): pass
        if not any(m.get('reason')=='build-finished' and m.get('success') is True for m in messages):
            raise ValueError('No Cargo successful completion')
        paths={m['executable'] for m in messages if m.get('reason')=='compiler-artifact' and m.get('target',{}).get('name')==name
               and kind in m.get('target',{}).get('kind',[]) and m.get('executable') and (kind!='lib' or m.get('profile',{}).get('test'))}
        if len(paths)!=1: raise ValueError('Ambiguous current executable')
        path=pathlib.Path(paths.pop())
        selected['test' if kind=='lib' else 'probe']={'path':str(path),'sha256':gates.digest(path.read_bytes())}
    gates.write_new(root/'.local/lifecycle'/f'{attempt}-selection.json',selected)
print(json.dumps({'attempt':attempt,'status':manifest['status'],'exit_code':code,'gate_retained':True}))
sys.exit(code)
