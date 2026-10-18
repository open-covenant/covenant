import http from 'node:http';
import crypto from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { configFromEnv } from './config.mjs';
import { Ledger,Refusal,ceilingCost } from './ledger.mjs';

const safeObject=v=>v!==null&&typeof v==='object'&&!Array.isArray(v);
const allowed=new Set(['model','messages','stream','max_tokens','temperature','top_p','seed','stop','presence_penalty','frequency_penalty']);
export function validateBody(body,c){
 if(!safeObject(body)||Object.keys(body).some(k=>!allowed.has(k)))throw new Refusal('unsupported_request',400);
 if(body.model!==c.model||(body.stream!==undefined&&body.stream!==false))throw new Refusal('unsupported_model_or_stream',400);
 if(!Array.isArray(body.messages)||body.messages.length<1||body.messages.length>32)throw new Refusal('invalid_messages',400);
 if(body.messages.some(m=>!safeObject(m)||Object.keys(m).some(k=>!['role','content'].includes(k))||!['system','user','assistant'].includes(m.role)||typeof m.content!=='string'))throw new Refusal('invalid_messages',400);
 const inputBytes=Buffer.byteLength(JSON.stringify(body.messages));
 if(inputBytes>c.maxInputBytes)throw new Refusal('input_limit_exceeded',413);
 const output=body.max_tokens??c.maxOutputTokens;
 if(!Number.isInteger(output)||output<1||output>c.maxOutputTokens)throw new Refusal('output_limit_exceeded',400);
 for(const [k,lo,hi] of [['temperature',0,2],['top_p',0,1],['presence_penalty',-2,2],['frequency_penalty',-2,2]])if(body[k]!==undefined&&(!Number.isFinite(body[k])||body[k]<lo||body[k]>hi))throw new Refusal('invalid_generation_parameter',400);
 if(body.seed!==undefined&&(!Number.isSafeInteger(body.seed)||Math.abs(body.seed)>2147483647))throw new Refusal('invalid_seed',400);
 if(body.stop!==undefined){const stop=typeof body.stop==='string'?[body.stop]:body.stop;if(!Array.isArray(stop)||!stop.length||stop.length>4||stop.some(x=>typeof x!=='string'||!x.length||Buffer.byteLength(x)>128))throw new Refusal('invalid_stop',400);}
 const normalized={model:c.model,messages:body.messages.map(m=>({role:m.role,content:m.content})),stream:false,max_tokens:output};
 for(const k of ['temperature','top_p','seed','stop','presence_penalty','frequency_penalty'])if(body[k]!==undefined)normalized[k]=body[k];
 return normalized;
}
const authorized=(header,token)=>{
 if(typeof header!=='string'||header.length>1200)return false;
 const expected=Buffer.from(`Bearer ${token}`),given=Buffer.from(header);
 return given.length===expected.length&&crypto.timingSafeEqual(given,expected);
};
async function readRequest(req,limit){let size=0,parts=[];for await(const chunk of req){size+=chunk.length;if(size>limit)throw new Refusal('request_too_large',413);parts.push(chunk);}try{return JSON.parse(Buffer.concat(parts).toString('utf8'));}catch{throw new Refusal('invalid_json',400);}}
async function readResponse(response,limit){let size=0,parts=[];if(!response.body)throw new Refusal('provider_invalid_response');for await(const chunk of response.body){size+=chunk.length;if(size>limit){await response.body.cancel().catch(()=>{});throw new Refusal('provider_response_too_large');}parts.push(chunk);}return Buffer.concat(parts).toString('utf8');}
function parseResponse(raw,c,diagnostics){
 diagnostics.response_sha256=crypto.createHash('sha256').update(raw).digest('hex');
 diagnostics.response_bytes=Buffer.byteLength(raw);
 if(raw.includes(c.token)||raw.includes(c.bearer))throw new Refusal('provider_response_contains_secret');
 let value;try{value=JSON.parse(raw);}catch{throw new Refusal('provider_response_invalid_json');}
 if(!safeObject(value))throw new Refusal('provider_response_not_object');
 // JSON escaping must not bypass secret detection after the operator decodes content.
 const inspect=[value];while(inspect.length){const part=inspect.pop();if(typeof part==='string'&&(part.includes(c.token)||part.includes(c.bearer)))throw new Refusal('provider_response_contains_secret');if(part&&typeof part==='object')for(const item of Object.values(part))inspect.push(item);}

 if(typeof value.model==='string'){
  diagnostics.observed_model_sha256=crypto.createHash('sha256').update(value.model).digest('hex');
  diagnostics.model_matches=value.model===c.model;
 }
 if(Array.isArray(value.choices))diagnostics.choice_count=value.choices.length;
 const choice=value.choices?.[0],usage=value.usage;
 if(safeObject(choice)){
  diagnostics.finish_reason=['stop','length','tool_calls','function_call','content_filter'].includes(choice.finish_reason)?choice.finish_reason:'other';
  diagnostics.content_kind=typeof choice.message?.content==='string'?'string':Array.isArray(choice.message?.content)?'array':choice.message?.content===null?'null':'other';
 }
 if(safeObject(usage))for(const field of ['prompt_tokens','completion_tokens','total_tokens'])if(Number.isSafeInteger(usage[field])&&usage[field]>=0)diagnostics[field]=usage[field];
 if(value.error)throw new Refusal('provider_response_error');
 if(value.model!==c.model)throw new Refusal('provider_response_model_mismatch');
 if(typeof value.id!=='string'||!value.id.length)throw new Refusal('provider_response_id_missing');
 if(!Array.isArray(value.choices)||value.choices.length!==1)throw new Refusal('provider_response_choice_count');
 if(!safeObject(choice)||!safeObject(choice.message))throw new Refusal('provider_response_choice_shape');
 if(typeof choice.message.content!=='string'||!choice.message.content.length)throw new Refusal('provider_response_content_missing');
 if(choice.message.tool_calls||choice.message.function_call)throw new Refusal('provider_response_tools_unsupported');
 if(!['stop','length'].includes(choice.finish_reason))throw new Refusal('provider_response_finish_reason');
 if(!safeObject(usage)||!Number.isSafeInteger(usage.prompt_tokens)||usage.prompt_tokens<1||!Number.isSafeInteger(usage.completion_tokens)||usage.completion_tokens<1)throw new Refusal('provider_usage_missing');
 if(usage.total_tokens!==undefined&&usage.total_tokens!==usage.prompt_tokens+usage.completion_tokens)throw new Refusal('provider_usage_invalid');
 // Only successful, validated response bytes are retained. Diagnostic fields never contain strings from the provider.
 return {inputTokens:usage.prompt_tokens,outputTokens:usage.completion_tokens,response:raw};
}
function send(res,status,body,headers={}){res.writeHead(status,{'content-type':'application/json','cache-control':'no-store',...headers});res.end(typeof body==='string'?body:JSON.stringify(body));}

export function createProvider(config,{testFetch,clock=Date.now}={}){
 if(testFetch&&process.env.NODE_ENV!=='test')throw new Error('test transport is unavailable');
 const fetcher=testFetch??globalThis.fetch;
 const ledger=config.enabled?new Ledger(config):null;
 const availability=()=>{
  if(!config.enabled)return {ready:false,status:'disabled'};
  const totals=ledger.totals(clock());
  const maximum=ceilingCost(config.maxInputTokens,config.inputPrice)+ceilingCost(config.maxOutputTokens,config.outputPrice);
  const ready=totals.unresolved===0&&totals.lifetime+maximum<=config.lifetimeBudget&&totals.daily+maximum<=config.dailyBudget;
  return {ready,status:ready?'ready':'blocked'};
 };
 const server=http.createServer(async(req,res)=>{
  let reservedId,diagnostics={};
  try{
   if(!['127.0.0.1','::1','::ffff:127.0.0.1'].includes(req.socket.remoteAddress))throw new Refusal('loopback_required',403);
   const url=new URL(req.url,'http://127.0.0.1');
   if(req.method==='GET'&&url.pathname==='/health'){send(res,200,{...availability(),model:config.enabled?config.model:null});return;}
   if(req.method==='GET'&&url.pathname==='/v1/models'){send(res,200,{object:'list',data:availability().ready?[{id:config.model,object:'model',owned_by:'covenant-provider'}]:[]});return;}
   if(req.method!=='POST'||url.pathname!=='/v1/chat/completions'||url.search)throw new Refusal('not_found',404);
   if(!config.enabled)throw new Refusal('provider_disabled');
   if(!authorized(req.headers.authorization,config.bearer))throw new Refusal('unauthorized',401);
   if(req.headers['content-type']?.split(';')[0].trim()!=='application/json'||req.headers['content-encoding'])throw new Refusal('unsupported_content_type',415);
   const body=validateBody(await readRequest(req,config.maxInputBytes+8192),config);
   const serialized=JSON.stringify(body),digest=crypto.createHash('sha256').update(serialized).digest('hex');
   const key=req.headers['idempotency-key'];
   if(key!==undefined&&(typeof key!=='string'||!/^[A-Za-z0-9._:-]{1,160}$/.test(key)))throw new Refusal('invalid_idempotency_key',400);
   const id=key?`key:${key}`:`request:${crypto.randomUUID()}`;
   const reservation=ledger.reserve(id,digest,body.max_tokens,clock());
   if(reservation.cached){send(res,200,reservation.cached,{'x-covenant-provider-replay':'true'});return;}
   reservedId=id;
   const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),config.timeoutMs);
   let completion;
   try{
    const response=await fetcher(`https://api.usepod.ai/proxy/${encodeURIComponent(config.token)}/v1/chat/completions`,{
     method:'POST',redirect:'error',headers:{'content-type':'application/json','User-Agent':'Covenant-x402/0.1','X-Pod-Max-Price-Input':String(config.inputPrice),'X-Pod-Max-Price-Output':String(config.outputPrice)},
     body:serialized,signal:controller.signal});
    diagnostics.http_status=response.status;
    if(!response.ok){await response.body?.cancel().catch(()=>{});throw new Refusal('provider_rejected_or_unknown');}
    completion=parseResponse(await readResponse(response,Math.min(8_388_608,config.maxOutputTokens*256+65_536)),config,diagnostics);
   }finally{clearTimeout(timer);}
   ledger.complete(id,completion,clock());reservedId=undefined;
   send(res,200,completion.response,{'x-covenant-provider-replay':'false'});
  }catch(error){
   const code=error instanceof Refusal?error.code:'provider_outcome_unknown';
   if(reservedId){try{ledger.unknown(reservedId,code,diagnostics);}catch{/* The persisted reservation still blocks new dispatch. */}}
   if(!res.headersSent&&!res.destroyed)send(res,error instanceof Refusal?error.status:503,{error:{type:'covenant_provider_error',code,message:code}});
   else res.destroy();
  }
 });
 server.requestTimeout=15_000;server.headersTimeout=10_000;server.keepAliveTimeout=5_000;server.maxHeadersCount=30;
 return {server,ledger,listen:()=>new Promise((resolve,reject)=>{server.once('error',reject);server.listen(config.port,'127.0.0.1',()=>resolve(server.address()));}),close:()=>new Promise(resolve=>server.close(()=>{ledger?.close();resolve();}))};
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
 process.umask(0o077);
 try{const config=configFromEnv(),app=createProvider(config);await app.listen();process.stdout.write(`Inference provider ${config.enabled?'enabled':'disabled'} on loopback:${config.port}\n`);let stopping=false;for(const signal of ['SIGINT','SIGTERM'])process.on(signal,async()=>{if(stopping)return;stopping=true;await app.close();process.exit(0);});}
 catch{process.stderr.write('Inference provider startup refused: invalid policy or unavailable durable state.\n');process.exitCode=1;}
}
