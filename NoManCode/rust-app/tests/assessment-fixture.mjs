// Isolated application assessment; all upstream credentials below are fake.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import {spawn} from 'node:child_process';
const root=path.resolve(process.argv[2]);
fs.mkdirSync(path.join(root,'workspace'),{recursive:true});
const calls=[];
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const fixture=http.createServer(async(req,res)=>{
  if(req.url==='/snapshot'){res.setHeader('content-type','application/json');res.end(JSON.stringify(calls));return;}
  const sentAccountToken=req.headers.authorization==='Bearer fake-account-assessment';
  if(req.url.endsWith('/api/user/self')){
    calls.push({path:req.url,accountToken:sentAccountToken});
    res.setHeader('content-type','application/json');res.end(JSON.stringify({success:true,data:{username:'隔离测试账号',quota:2500000,used_quota:500000}}));return;
  }
  if(req.url.endsWith('/models')){
    calls.push({path:req.url,accountToken:sentAccountToken});
    res.setHeader('content-type','application/json');res.end(JSON.stringify({data:[{id:'fixture-sol'},{id:'fixture-astra'}]}));return;
  }
  if(req.url!=='/v1/chat/completions'){res.writeHead(404);res.end();return;}
  let raw='';for await(const c of req)raw+=c;
  const body=JSON.parse(raw),last=body.messages.at(-1),text=String(last.content);
  calls.push({path:req.url,model:body.model,accountToken:sentAccountToken,max_tokens:body.max_tokens,last:text.slice(0,160)});
  if(text==='rate-limit'){res.writeHead(429,{'content-type':'application/json'});res.end(JSON.stringify({error:{message:'fixture rate limit'}}));return;}
  res.writeHead(200,{'content-type':'text/event-stream'});
  const emit=v=>res.write('data: '+JSON.stringify(v)+'\n\n');
  if(text==='write-test'){
    emit({choices:[{delta:{tool_calls:[{index:0,id:'fixture-write',type:'function',function:{name:'write_file',arguments:JSON.stringify({path:'src/result.txt',content:'assessment-rust-ok'})}}]},finish_reason:'tool_calls'}]});
  } else if(text==='truncated'){
    emit({choices:[{delta:{content:'partial-before-interruption'}}]});res.end();return;
  } else {
    const answer=text.includes('17 * 19')?'323':text.includes('Completed predecessor')?'模拟汇总完成':last.role==='tool'?'文件写入完成':text==='slow'?'slow-response':'模拟执行完成';
    for(const part of answer.match(/.{1,2}/gu)){
      if(res.destroyed)return;
      emit({choices:[{delta:{content:part}}]});await sleep(text==='slow'?5000:75);
    }
    emit({choices:[{delta:{},finish_reason:'stop'}],usage:{prompt_tokens:10,completion_tokens:10,total_tokens:20}});
  }
  res.end('data: [DONE]\n\n');
});
await new Promise(r=>fixture.listen(0,'127.0.0.1',r));
const upstream=`http://127.0.0.1:${fixture.address().port}`;
const origin='http://127.0.0.1:3093';
const logfile=fs.openSync(path.join(root,'native.log'),'a');
const child=spawn(path.resolve('../bin/peachsh.exe'),['--port','3093','--data-dir',path.join(root,'data'),'--workspace',path.join(root,'workspace'),'--legacy-data',path.join(root,'no-legacy')],{windowsHide:true,stdio:['ignore',logfile,logfile]});
let token;
for(let n=0;n<80;n++){
  try{token=(await(await fetch(origin+'/api/bootstrap')).json()).token;if(token)break;}catch{}
  await sleep(100);
}
if(!token)throw Error('isolated app failed to start');
const settings={workspace:path.join(root,'workspace'),max_concurrency:3,newapi:null,routes:[['sol','fixture-sol'],['astra','fixture-astra']].map(([id,model])=>({id,name:'隔离 '+id,base_url:upstream+'/v1',model,max_tokens:128,parallel_limit:1,key_env:null}))};
const response=await fetch(origin+'/api/settings',{method:'PUT',headers:{'content-type':'application/json','x-peachsh-token':token},body:JSON.stringify({settings,keys:{sol:'fake-sol-assessment',astra:'fake-astra-assessment'}})});
if(!response.ok)throw Error('isolated config failed');
fs.writeFileSync(path.join(root,'runtime.json'),JSON.stringify({root,origin,upstream,pid:child.pid,fixturePid:process.pid}));
console.log('READY '+origin);
process.on('SIGINT',()=>{child.kill();fixture.close();});
process.on('SIGTERM',()=>{child.kill();fixture.close();});
