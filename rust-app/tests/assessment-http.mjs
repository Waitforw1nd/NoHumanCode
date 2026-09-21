// Black-box assessment of the release binary in assessment-fixture.mjs only.
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
const root=path.resolve(process.argv[2]);
const env=JSON.parse(fs.readFileSync(path.join(root,'runtime.json')));
if(env.origin!=='http://127.0.0.1:3093')throw Error('This test must use isolated port 3093');
const {token}=await(await fetch(env.origin+'/api/bootstrap')).json();
const results=[];
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function api(route,body,method='POST',extra={}){
  const response=await fetch(env.origin+'/api'+route,{method:body===undefined?'GET':method,headers:{'content-type':'application/json','x-peachsh-token':token,...extra},body:body===undefined?undefined:JSON.stringify(body)});
  return {status:response.status,data:await response.json()};
}
const request=prompt=>({title:'HTTP assessment: '+prompt,tasks:[{name:'worker',role:'tester',route_id:'sol',prompt,depends_on:[],write_scopes:[],tools:false,allow_commands:false,max_rounds:3}]});
async function settled(id){for(let n=0;n<100;n++){const r=(await api('/runs/'+id)).data;if(r.tasks.every(t=>!['running','queued'].includes(t.status)))return r;await sleep(100);}throw Error('task did not settle');}
async function test(name,fn){try{const details=await fn();results.push({name,result:'PASS',details});}catch(e){results.push({name,result:'FAIL',details:e.message});}}
const check=(value,message)=>{if(!value)throw Error(message);};
await test('429 is surfaced as failed task',async()=>{const r=await api('/runs',request('rate-limit'));const final=await settled(r.data.id);check(final.tasks[0].status==='failed'&&final.tasks[0].error.includes('429'),'rate limit hidden');return final.tasks[0].error;});
await test('truncated stream preserves partial output and resumes',async()=>{
  const r=await api('/runs',request('truncated'));const final=await settled(r.data.id);const t=final.tasks[0];
  check(t.status==='failed'&&t.output==='partial-before-interruption','partial output was lost');
  await api('/tasks/'+t.id+'/resume',{message:'resume assessment'});
  const resumed=(await settled(r.data.id)).tasks[0];check(resumed.status==='completed'&&resumed.id===t.id&&resumed.route.model===t.route.model,'resume failed');return {status:resumed.status,stableIdentity:true};
});
await test('cancel a streaming task',async()=>{
  const r=await api('/runs',request('slow'));await sleep(200);const now=performance.now();await api('/tasks/'+r.data.tasks[0].id+'/cancel',{});
  const done=await settled(r.data.id);check(done.tasks[0].status==='cancelled','cancel failed');return {status:done.tasks[0].status,elapsed_ms:Math.round(performance.now()-now)};
});
await test('idempotent concurrent repeats create one run',async()=>{
  const key='assessment-concurrent-'+Date.now();const outcomes=await Promise.all(Array.from({length:8},()=>api('/runs',request('idem'), 'POST',{'idempotency-key':key})));
  const ids=new Set(outcomes.map(o=>o.data.id));check(ids.size===1&&outcomes.every(o=>o.status===200),'duplicate run created');await settled(outcomes[0].data.id);return {requests:8,runs:ids.size};
});
await test('idempotency rejects changed body',async()=>{
  const key='assessment-payload-'+Date.now();const first=await api('/runs',request('first'),'POST',{'idempotency-key':key});
  const second=await api('/runs',request('different'),'POST',{'idempotency-key':key});await settled(first.data.id);
  check(second.status===409||second.status===400,`changed body returned HTTP ${second.status}, old run reused=${second.data.id===first.data.id}`);
});
// Fake alternate recipient, using no real account or model credential.
const received=[];
const alternate=http.createServer((req,res)=>{
  received.push({path:req.url,receivedFakeAccountToken:req.headers.authorization==='Bearer fake-account-assessment'});
  res.setHeader('content-type','application/json');res.end(JSON.stringify(req.url.endsWith('/models')?{data:[{id:'fixture-sol'}]}:{success:true,data:{quota:2500000,used_quota:500000}}));
});
await new Promise(r=>alternate.listen(0,'127.0.0.1',r));
const other=`http://127.0.0.1:${alternate.address().port}`;
const original=(await api('/settings')).data.settings;
await test('account token is not sent when rebinding a different origin with empty token',async()=>{
  await api('/newapi',{account:{base_url:env.upstream,user_id:'42',quota_per_unit:500000},token:'fake-account-assessment'},'PUT');
  const before=received.length;const r=await api('/newapi',{account:{base_url:other,user_id:'42',quota_per_unit:500000},token:null},'PUT');
  check(received.length===before&&r.status>=400,`HTTP ${r.status}; alternate origin received saved account token=${received.slice(before).some(v=>v.receivedFakeAccountToken)}`);
});
await test('account credential cannot be used by an inference route',async()=>{
  const settings={...original,routes:[...original.routes,{...original.routes[0],id:'newapi-account',name:'namespace probe',base_url:other+'/v1',key_env:null}]};
  const saved=await api('/settings',{settings,keys:{}},'PUT');if(saved.status>=400)return {rejected:true};
  const before=received.length;const r=await api('/routes/newapi-account/models');
  check(!received.slice(before).some(v=>v.receivedFakeAccountToken),`HTTP ${r.status}; inference /models received saved account token`);
});
await api('/settings',{settings:original,keys:{}},'PUT');
alternate.closeAllConnections();await new Promise(r=>alternate.close(r));
// Small sequential loopback sample only; not an upstream model benchmark.
const samples=[];for(let n=0;n<30;n++){const t=performance.now();await api('/health');samples.push(performance.now()-t);}samples.sort((a,b)=>a-b);
results.push({name:'loopback health latency, sequential 30 requests',result:'MEASURED',details:{p50_ms:+samples[15].toFixed(2),p95_ms:+samples[28].toFixed(2)}});
fs.writeFileSync(path.join(root,'http-results.json'),JSON.stringify(results,null,2));
console.log(JSON.stringify(results,null,2));
process.exitCode=results.some(r=>r.result==='FAIL')?1:0;
