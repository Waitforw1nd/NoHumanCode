// Local UI acceptance fixture. Not part of the native application runtime.
import http from 'node:http';
const server=http.createServer(async(req,res)=>{
  if(req.url==='/v1/models'){res.setHeader('content-type','application/json');res.end(JSON.stringify({data:[{id:'fixture-sol'},{id:'fixture-astra'}]}));return;}
  if(req.url==='/api/user/self'){res.setHeader('content-type','application/json');res.end(JSON.stringify({success:true,data:{username:'本地验收账号',quota:2500000,used_quota:500000,group:'UI fixture'}}));return;}
  if(req.url!=='/v1/chat/completions'){res.writeHead(404);res.end();return;}
  let text='';for await(const chunk of req){text+=chunk;}const body=JSON.parse(text);
  res.writeHead(200,{'content-type':'text/event-stream','cache-control':'no-cache'});
  const answer=body.messages.some(m=>String(m.content).includes('Completed predecessor'))?'模拟汇总已完成。所有前置成员均成功，模型路由和成员身份保持独立。':'模拟任务已完成。Rust 服务接收请求、流式展示和持久化均已连通。';
  for(const part of answer.match(/.{1,4}/gu)){res.write('data: '+JSON.stringify({choices:[{delta:{content:part}}]})+'\n\n');await new Promise(resolve=>setTimeout(resolve,45));}
  res.end('data: '+JSON.stringify({choices:[{delta:{},finish_reason:'stop'}],usage:{prompt_tokens:10,completion_tokens:10,total_tokens:20}})+'\n\ndata: [DONE]\n\n');
});
server.listen(3098,'127.0.0.1',()=>console.log('UI fixture on loopback 3098'));
