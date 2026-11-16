import ctypes as c,ctypes.wintypes as w,pathlib,json,hashlib,datetime,time,sqlite3
root=pathlib.Path(__file__).resolve().parent.parent
out=root/'.local/lifecycle/incident-cli-host-1'; obs=json.loads((out/'cim-observation.json').read_text(encoding='utf-8-sig'))
assert isinstance(obs,dict)
k=c.WinDLL('kernel32',use_last_error=True); n=c.WinDLL('ntdll',use_last_error=True)
k.OpenProcess.argtypes=[w.DWORD,w.BOOL,w.DWORD]; k.OpenProcess.restype=w.HANDLE
k.QueryFullProcessImageNameW.argtypes=[w.HANDLE,w.DWORD,w.LPWSTR,c.POINTER(w.DWORD)];k.QueryFullProcessImageNameW.restype=w.BOOL
k.GetProcessTimes.argtypes=[w.HANDLE,c.POINTER(w.FILETIME),c.POINTER(w.FILETIME),c.POINTER(w.FILETIME),c.POINTER(w.FILETIME)];k.GetProcessTimes.restype=w.BOOL
k.WaitForSingleObject.argtypes=[w.HANDLE,w.DWORD]; k.WaitForSingleObject.restype=w.DWORD
k.GetExitCodeProcess.argtypes=[w.HANDLE,c.POINTER(w.DWORD)];k.GetExitCodeProcess.restype=w.BOOL
k.TerminateProcess.argtypes=[w.HANDLE,w.UINT];k.TerminateProcess.restype=w.BOOL
k.CloseHandle.argtypes=[w.HANDLE];k.CloseHandle.restype=w.BOOL
n.NtQueryInformationProcess.argtypes=[w.HANDLE,w.ULONG,w.LPVOID,w.ULONG,c.POINTER(w.ULONG)]; n.NtQueryInformationProcess.restype=w.LONG
class US(c.Structure): _fields_=[('Length',w.USHORT),('MaximumLength',w.USHORT),('Buffer',c.c_void_p)]
h=k.OpenProcess(0x00100000|0x1000|0x0400|0x0010|1,False,obs['ProcessId']); assert h
# From here all observations and any later termination use this exact object handle.
def identity():
 size=w.DWORD(32768); buf=c.create_unicode_buffer(size.value); assert k.QueryFullProcessImageNameW(h,0,buf,c.byref(size))
 times=[w.FILETIME() for _ in range(4)]; assert k.GetProcessTimes(h,*[c.byref(x) for x in times]); ft=(times[0].dwHighDateTime<<32)|times[0].dwLowDateTime
 length=w.ULONG(); n.NtQueryInformationProcess(h,60,None,0,c.byref(length)); assert 0<length.value<1048576
 cb=c.create_string_buffer(length.value); status=n.NtQueryInformationProcess(h,60,cb,length,c.byref(length)); assert status==0,status
 u=US.from_buffer(cb); assert c.addressof(cb)<=u.Buffer and u.Buffer+u.Length<=c.addressof(cb)+len(cb)
 command=c.string_at(u.Buffer,u.Length).decode('utf-16-le')
 return dict(image=buf.value,creation_filetime=ft,command_line=command)
i=identity(); assert pathlib.Path(i['image']).resolve()==(root/'.local/lifecycle-target/debug/peachsh.exe').resolve(); assert i['command_line']==obs['CommandLine']; assert i['image']==obs['ExecutablePath']
created=datetime.datetime(1601,1,1,tzinfo=datetime.timezone.utc)+datetime.timedelta(microseconds=i['creation_filetime']//10)
assert abs((created-datetime.datetime.fromisoformat(obs['CreationDate'])).total_seconds())<0.00001
m=json.loads((root/'.local/lifecycle/integrated-external-1/manifest.json').read_text()); assert datetime.datetime.fromisoformat(m['started_utc'])<created<datetime.datetime.fromisoformat(m['ended_utc'])
data=root/'.local/lifecycle/integrated-external-1/tmp/.tmpeJelLY/server-data'
expected='"'+str(root/'.local/lifecycle-target/debug/peachsh.exe')+'" --port 54408 --data-dir '+str(data)+' --legacy-data '+str(data.parent/'legacy')+' --workspace '+str(data.parent/'workspace')
assert i['command_line']==expected
uri=(data/'peachsh.sqlite3').as_uri()+'?mode=ro'; db=sqlite3.connect(uri,uri=True)
tasks=db.execute('SELECT id, value FROM tasks').fetchall(); approvals=db.execute('SELECT task_id,tool_call_id,execution_state FROM approvals').fetchall()
assert len(tasks)==1 and tasks[0][0]=='locked-task'; t=json.loads(tasks[0][1]); assert t['status']=='interrupted',t['status']; assert approvals==[('locked-task','locked-call','not_started')],approvals
assert db.execute("SELECT count(*) FROM network_calls WHERE state='claimed'").fetchone()[0]==0
assert db.execute("SELECT count(*) FROM worktree_operations WHERE state='claimed'").fetchone()[0]==0
db.close()
assert k.WaitForSingleObject(h,0)==258

expected=json.loads((out/'capture.json').read_text()); assert i==expected['identity']
assert hashlib.sha256(pathlib.Path(i['image']).read_bytes()).hexdigest()==expected['binary_sha256']
unknown=False
try:
 (out/'action-before.json').write_text(json.dumps(dict(identity=i,capture_sha256=hashlib.sha256((out/'capture.json').read_bytes()).hexdigest(),action='TerminateProcess on separately validated handle to the same uniquely bound idle fixture Host',original_capture_helper_retains_handle=True),indent=2)+'\n')
 if not k.TerminateProcess(h,1):
  if k.WaitForSingleObject(h,0)!=0: raise OSError(c.get_last_error(),'Termination failed')
 deadline=time.monotonic()+5
 while True:
  wait=k.WaitForSingleObject(h,100)
  if wait==0:break
  if wait!=258:raise OSError(c.get_last_error(),'Wait failed')
  if time.monotonic()>deadline and not unknown:
   unknown=True; (out/'action-unknown.json').write_text(json.dumps({'reason':'original object not signalled within5s; retain exact handle'})); print('NHC_TERMINAL_UNKNOWN: retaining exact fixture Host handle',flush=True)
 code=w.DWORD(); assert k.GetExitCodeProcess(h,c.byref(code))
 (out/'action-terminal.json').write_text(json.dumps(dict(identity=i,exit_code=code.value,signalled=True,unknown_ever=unknown),indent=2)+'\n');assert k.CloseHandle(h)
 print('EXACT_FIXTURE_HOST_TERMINAL',flush=True)
except BaseException as error:
 (out/'action-unknown.json').write_text(json.dumps({'reason':str(error),'handle_retained':True}));print('NHC_TERMINAL_UNKNOWN: action error; original object handle retained',flush=True)
 while True: time.sleep(1)
