const fs=require('fs'),vm=require('vm'),assert=require('assert/strict');
class Element {
 constructor(tag){this.tagName=tag;this.children=[];this.textContent='';this.disabled=false;this.hidden=false;this.value='';this.dataset={};this.classList={toggle(){},add(){}};}
 append(...items){this.children.push(...items);}
 replaceChildren(...items){this.children=[...items];}
 addEventListener(){} setAttribute(){} click(){return this.onclick?.();}
}
const elements=new Map(),requests=[],replies=[];
const context=vm.createContext({console,document:{getElementById(id){if(!elements.has(id))elements.set(id,new Element('div'));return elements.get(id)},createElement(tag){return new Element(tag)},querySelectorAll(){return []}},crypto:{randomUUID(){return 'snapshot-key'}},fetch:async(path,options)=>{requests.push({path,...options});const next=replies.shift();if(next instanceof Error)throw next;assert(next,'unexpected fetch');return {ok:next.status<400,status:next.status,json:async()=>next.body}},EventSource:class{}});
const source=fs.readFileSync('NoManCode/rust-app/web/app.js','utf8').replace(/\(async\(\)=>\{try\{const boot=await api\('\/bootstrap'\);[\s\S]*?\}\)\(\);/,'');
vm.runInContext(source,context);
const walk=n=>[n,...n.children.flatMap(walk)];
const find=(n,text)=>walk(n).find(x=>x.textContent===text);
const reply=(status,body)=>replies.push({status,body});
(async()=>{
 const panel=vm.runInContext("authorizationPanel({kind:'route',id:'route-a'})",context);
 reply(200,{subject:{kind:'route',id:'route-a'},origin:'https://example.com:443',base_url:'https://example.com:443/v1',credential_ref:'route::route-a',config_revision:1,credential_revision:2,binding_digest:'a'.repeat(64),purposes:['models','completion'],authorization:null});
 await find(panel,'查看网络授权目标').click();assert.equal(requests.length,1);assert.equal(requests[0].path,'/api/network/preview');
 const checks=walk(panel).filter(n=>n.type==='checkbox');assert.equal(checks.length,2);assert(checks.every(n=>n.checked===false));
 const confirm=find(panel,'确认以上目标与选中用途');await confirm.click();assert.equal(requests.length,1,'no selected purpose must not authorize');checks[1].checked=true;
 reply(409,{error:{code:'stale',message:'版本已变化',dispatch_state:'not_sent',calls:[],retryable:false}});await confirm.click();assert.equal(requests.length,2);const snapshot=JSON.parse(requests[1].body);assert.equal(snapshot.config_revision,1);assert.deepEqual(snapshot.purposes,['completion']);assert.equal(requests[1].headers['Idempotency-Key'],'snapshot-key');assert(elements.get('notice').textContent.includes('版本已变化'));
 reply(201,{authorization:{authorization_id:'auth-a',state:'active'}});await confirm.click();assert.equal(requests.length,3,'409 must not silently preview/retry');assert.equal(requests[2].body,requests[1].body);assert.equal(requests[2].headers['Idempotency-Key'],requests[1].headers['Idempotency-Key']);
 const approvals=vm.runInContext("approvalPanel('task-a')",context);
 reply(200,{approvals:[{id:'approval-a',tool_name:'run_command',preview:'echo hello',status:'approved',execution_state:'not_started',command_network_ack:'required',network_notice:{version:1,may_access_network:true,outbound_sandbox:false}}]});
 await find(approvals,'查看审批与命令联网确认').click();assert(find(approvals,'此命令可能访问任意网络，当前没有OS出站沙箱'));const approve=find(approvals,'补充联网确认');assert(approve,'old approved/required must remain actionable');const before=requests.length;await approve.click();assert.equal(requests.length,before,'unchecked acknowledgement must not submit');walk(approvals).find(n=>n.type==='checkbox').checked=true;
 reply(200,{status:'approved'});reply(200,{approvals:[]});await approve.click();await new Promise(r=>setImmediate(r));const decision=requests.find(r=>r.path==='/api/approvals/approval-a/decision');assert.deepEqual(JSON.parse(decision.body),{decision:'approve',acknowledge_command_network:true});assert(!requests.some(r=>r.path.includes('/resume')));
 reply(409,{error:{message:'第二步未准入',dispatch_state:'not_sent',calls:[{call_id:'call-a',purpose:'models',state:'finished'}]}});let error;try{await vm.runInContext("api('/routes/route-a/check',{})",context)}catch(e){error=e;}assert(error.message.includes('call-a'));assert(error.message.includes('此前步骤可能已联网'));assert.equal(error.dispatch_state,'not_sent');
 replies.push(new Error('disconnect'));await assert.rejects(vm.runInContext("api('/network/calls/call-a')",context),e=>!e.message.includes('结果未知'));
 replies.push(new Error('disconnect'));await assert.rejects(vm.runInContext("api('/routes/route-a/models',{})",context),e=>e.message.includes('结果未知'));
 console.log('PASS web: explicit snapshot/key, unchecked purpose/command acknowledgement, approved-required sidecar flow, no auto-resume/retry, receipt visibility and read/write failure distinction');
})().catch(e=>{console.error(e);process.exitCode=1});
