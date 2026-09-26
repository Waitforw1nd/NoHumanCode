import fs from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
// Executes the shipped application functions with an explicit DOM/fetch fixture.
// This is behavior QA, not a browser render or screenshot test.
class Element {
 constructor(tag='div'){this.tagName=tag;this.children=[];this.value='';this.checked=false;this.disabled=false;this.dataset={};this.listeners={};this.classList={toggle(){},add(){}};this.style={};}
 append(...nodes){this.children.push(...nodes);}
 replaceChildren(...nodes){this.children=[...nodes];}
 get firstElementChild(){return this.children[0];}
 get lastElementChild(){return this.children.at(-1);}
 get options(){return this.children;}
 addEventListener(name,fn){this.listeners[name]=fn;}
 setAttribute(){} querySelectorAll(){return [];} querySelector(){return new Element();}
 remove(){} replaceWith(){} click(){return this.onclick?.();}
}
const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,new Element());return elements.get(id);};
const requests=[];let respond=()=>({ok:true,value:{}});let sequence=0;
const context=vm.createContext({document:{getElementById:get,createElement:tag=>new Element(tag),querySelectorAll:()=>[]},crypto:{randomUUID:()=>`key-${++sequence}`},EventSource:class{constructor(url){this.url=url;}addEventListener(){}close(){}},fetch:async(url,options)=>{const request={url,method:options.method,body:options.body?JSON.parse(options.body):null,headers:options.headers};requests.push(request);const response=await respond(request);return {ok:response.ok,status:response.status||200,json:async()=>response.value};},console});
const source=fs.readFileSync(new URL('../web/app.js',import.meta.url),'utf8');
const startup="(async()=>{try{const boot=await api('/bootstrap');token=boot.token;await loadSettings();await loadProjects();addMember();await history();}catch(e){notice(e.message,true);}})();";
assert(source.includes(startup),'startup fixture anchor changed');
vm.runInContext(source.replace(startup,''),context);
const run=code=>vm.runInContext(code,context);
const setup=()=>{run("selection={project_id:'project-a',revision:1};selectedConfig={project_id:'project-a',revision:2};chatDraft=null;chatRunId=null;chatTaskId=null;chatTaskState=null;chatContext=null;chatBinding=null;chatPending=null;projects=[];token='local-token';");get('chat-input').value='draft message';get('chat-route').value='route-a';requests.length=0;};
setup();get('chat-input').listeners.input();run("selection={project_id:'project-b',revision:2};selectedConfig={project_id:'project-b',revision:7};");
respond=()=>({ok:false,status:409,value:{code:'project_revision_conflict',message:'stale revision',error:'stale revision'}});
await assert.rejects(run('sendChat()'),/stale revision/);
assert.equal(requests.length,1);assert.equal(requests[0].url,'/api/projects/project-a/runs');assert.equal(requests[0].body.expected_config_revision,2);assert.equal(get('chat-input').value,'draft message');const fixedKey=requests[0].headers['Idempotency-Key'];
await assert.rejects(run('sendChat()'),/stale revision/);assert.equal(requests[1].headers['Idempotency-Key'],fixedKey);assert.equal(requests.length,2);
get('chat-input').value='modified after uncertain send';await assert.rejects(run('sendChat()'),/查询历史/);assert.equal(requests.length,2);
console.log('PASS project draft remains A after selecting B; conflict preserves text/key and never auto-retries');
setup();get('project-picker').value='project-b';respond=()=>({ok:false,status:409,value:{code:'project_revision_conflict',message:'selection changed',error:'selection changed'}});
await get('project-select').onclick();assert.equal(requests.length,1);assert.equal(requests[0].method,'PUT');assert.equal(requests[0].body.expected_revision,1);assert.equal(run('selection.project_id'),'project-a');assert.equal(get('chat-input').value,'draft message');
console.log('PASS selection CAS conflict is not refreshed or resent');
requests.length=0;run("projectEdit={project_id:'project-a',revision:2};");const text='{"allowed_route_ids":[],"default_route_id":null,"tools":false,"write_scopes":[],"allow_commands":false}';get('project-config').value=text;await get('project-config-save').onclick();assert.equal(requests.length,1);assert.deepEqual(requests[0].body,{expected_revision:2,config:JSON.parse(text)});assert.equal(get('project-config').value,text);assert.equal(run('projectEdit.revision'),2);
console.log('PASS config PUT is nested, tools boolean, stale configuration draft retained');
setup();run("selection={project_id:'project-b',revision:2};chatRunId='run-a';chatTaskId='task-a';chatContext={project:{id:'project-a',name:'A',root_path:'A-root'},session:{id:'session-a'},latest_turn:{id:'turn-a'},tasks:[{legacy_task_id:'task-a',agent_id:'agent-a'}]};");
respond=request=>{if(request.url==='/api/sessions/session-a/turns')return {ok:true,value:{}};if(request.url==='/api/runs/run-a')return {ok:true,value:{tasks:[{id:'task-next',status:'completed',messages:[],route:{id:'route-a'}}]}};if(request.url==='/api/tasks/task-next/project')return {ok:true,value:{status:'bound',binding:{project_id:'project-a'}}};if(request.url==='/api/runs/run-a/context')return {ok:true,value:{project:{id:'project-a',name:'A',root_path:'A-root'},session:{id:'session-a'},latest_turn:{id:'turn-next'},tasks:[{legacy_task_id:'task-next',agent_id:'agent-a'}]}};throw Error('unexpected '+request.url);};
await run('sendChat()');assert.deepEqual(requests[0].body,{agent_id:'agent-a',expected_last_turn_id:'turn-a',message:'draft message'});assert.equal(requests[0].url,'/api/sessions/session-a/turns');assert(!requests.some(r=>r.url.includes('/resume')));assert.equal(run('chatTaskId'),'task-next');assert.equal(run('chatEventSource.url'),'/api/sessions/session-a/events');assert.match(get('chat-project').textContent,/会话项目：A/);
console.log('PASS follow-up uses original Session new Turn and latest task mapping despite current B');

setup();run("chatRunId='run-a';chatTaskId='task-a';chatBinding={status:'bound'};chatContext={session:{id:'session-a'},latest_turn:{id:'turn-a'},tasks:[{legacy_task_id:'task-a',agent_id:'agent-a'}]};");respond=()=>{throw Error('connection lost');};await assert.rejects(run('sendChat()'),/结果未知/);const pendingKey=requests[0].headers['Idempotency-Key'];run("chatContext.latest_turn.id='accepted-turn';chatTaskId='accepted-task';chatContext.tasks=[{legacy_task_id:'accepted-task',agent_id:'accepted-agent'}];");await assert.rejects(run('sendChat()'),/结果未知/);assert.equal(requests[1].headers['Idempotency-Key'],pendingKey);assert.equal(requests[1].url,'/api/sessions/session-a/turns');assert.equal(requests[1].body.expected_last_turn_id,'turn-a');assert.equal(requests[1].body.agent_id,'agent-a');assert.equal(get('chat-input').value,'draft message');get('chat-input').value='different';await assert.rejects(run('sendChat()'),/结果待核对/);assert.equal(requests.length,2);console.log('PASS uncertain Session send retains original key/body; changed text cannot silently resend');

get('chat-input').value='draft message';run("chatContext.session.id='other-visible-session';");await assert.rejects(run('sendChat()'),/返回原会话/);assert.equal(requests.length,2);assert.equal(run('chatPending.session_id'),'session-a');console.log('PASS navigation to another Session blocks unknown pending send without discarding the snapshot');
