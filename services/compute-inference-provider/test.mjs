import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import {spawn} from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';
import { configFromEnv,PolicyError } from './config.mjs';
import { createProvider,validateBody } from './server.mjs';
import { Ledger,ceilingCost } from './ledger.mjs';
process.env.NODE_ENV='test';
const P='COVENANT_INFERENCE_PROVIDER_';
const TOKEN='private_provider_token_for_test_only';
const AUTH='private_local_bearer_for_test_only';
const configuration=(dir,overrides={})=>({enabled:true,port:0,model:'qualified-text-model',dbPath:path.join(dir,'ledger.sqlite'),token:TOKEN,bearer:AUTH,lifetimeBudget:10000,dailyBudget:10000,inputPrice:1000000,outputPrice:2000000,maxInputBytes:512,maxInputTokens:2000,maxOutputTokens:100,timeoutMs:500,...overrides});
const request=(overrides={})=>({model:'qualified-text-model',messages:[{role:'user',content:'Write one sentence.'}],max_tokens:20,stream:false,...overrides});
const completion=(overrides={})=>({id:'chatcmpl-live-fixture',object:'chat.completion',created:1792270000,model:'qualified-text-model',choices:[{index:0,message:{role:'assistant',content:'The fixture returned this response.'},finish_reason:'stop'}],usage:{prompt_tokens:10,completion_tokens:5,total_tokens:15},...overrides});
const temp=t=>{const d=fs.mkdtempSync(path.join(os.tmpdir(),'covenant-provider-test-'));t.after(()=>fs.rmSync(d,{recursive:true,force:true}));return d;};
const post=(base,body=request(),headers={})=>fetch(`${base}/v1/chat/completions`,{method:'POST',headers:{authorization:`Bearer ${AUTH}`,'content-type':'application/json',...headers},body:JSON.stringify(body)});
async function appFor(t,config,fetcher){const app=createProvider(config,{testFetch:fetcher});const a=await app.listen();t.after(()=>app.close());return {...app,base:`http://127.0.0.1:${a.port}`};}
function validEnv(dir){return Object.fromEntries(Object.entries({ENABLED:'true',MODEL:'qualified-text-model',DB_PATH:path.join(dir,'ledger.sqlite'),BEARER_TOKEN:AUTH,LIFETIME_BUDGET_MICRO_USDC:'10000',DAILY_BUDGET_MICRO_USDC:'10000',MAX_PRICE_INPUT_MICRO_USDC_PER_M:'1000000',MAX_PRICE_OUTPUT_MICRO_USDC_PER_M:'2000000',MAX_INPUT_BYTES:'512',MAX_INPUT_TOKENS:'2000',MAX_OUTPUT_TOKENS:'100'}).map(([k,v])=>[P+k,v]).concat([['USEPOD_TOKEN',TOKEN]]));}

test('default disabled and malformed enabled policy refuses startup',t=>{assert.deepEqual(configFromEnv({}),{enabled:false,port:4044});const env=validEnv(temp(t));assert.equal(configFromEnv(env).model,'qualified-text-model');for(const change of [{[P+'ENABLED']:'yes'},{[P+'DAILY_BUDGET_MICRO_USDC']:'10001'},{[P+'MAX_OUTPUT_TOKENS']:'0'},{[P+'MAX_INPUT_TOKENS']:'100'},{[P+'MAX_PRICE_INPUT_MICRO_USDC_PER_M']:'1.5'},{USEPOD_TOKEN:undefined},{[P+'DB_PATH']:':memory:'}])assert.throws(()=>configFromEnv({...env,...change}),PolicyError);});

test('secrets read from plain files; conflicting env inputs fail closed',t=>{const dir=temp(t),env=validEnv(dir),file=path.join(dir,'token');fs.writeFileSync(file,TOKEN,{mode:0o600});delete env.USEPOD_TOKEN;env.USEPOD_TOKEN_FILE=file;assert.equal(configFromEnv(env).token,TOKEN);assert.throws(()=>configFromEnv({...env,USEPOD_TOKEN:TOKEN}),PolicyError);});

test('disabled and authenticated models/health do not dispatch paid calls',async t=>{let paid=0;const a=await appFor(t,{enabled:false,port:0},async()=>{paid++;throw new Error();});assert.deepEqual((await (await fetch(`${a.base}/v1/models`)).json()).data,[]);assert.equal((await post(a.base)).status,503);assert.equal(paid,0);});

test('invalid auth, model, fields, stream, bytes and funds cause zero paid dispatch',async t=>{let calls=0;const a=await appFor(t,configuration(temp(t),{lifetimeBudget:100,dailyBudget:100}),async()=>{calls++;return Response.json(completion());});const cases=[{headers:{authorization:'Bearer incorrect'}},{body:request({model:'other'})},{body:request({stream:true})},{body:request({tools:[]})},{body:request({n:2})},{body:request({max_tokens:101})},{body:request({temperature:3})},{body:request({messages:[{role:'user',content:'x'.repeat(513)}]})},{body:request({messages:[{role:'user',content:[{type:'text',text:'hi'}]}]})},{body:request()}];for(const c of cases)assert.ok((await post(a.base,c.body??request(),c.headers)).status>=400);assert.equal(calls,0);assert.equal(a.ledger.totals().lifetime,0);});

test('reservation is durable before a genuine fixture HTTP dispatch; ceilings forwarded',async t=>{const dir=temp(t),config=configuration(dir);let calls=0;const upstream=http.createServer((req,res)=>{calls++;const db=new DatabaseSync(config.dbPath,{readOnly:true});const row=db.prepare('SELECT state,accounted FROM calls').get();assert.equal(row.state,'reserved');assert.equal(row.accounted,2040);db.close();assert.equal(req.headers['x-pod-max-price-input'],'1000000');assert.equal(req.headers['x-pod-max-price-output'],'2000000');assert.equal(req.headers.authorization,undefined);res.writeHead(200,{'content-type':'application/json','X-Balance-Remaining':'SECRET-MUST-NOT-FORWARD'});res.end(JSON.stringify(completion()));});await new Promise(r=>upstream.listen(0,'127.0.0.1',r));t.after(()=>new Promise(r=>upstream.close(r)));const fixture=`http://127.0.0.1:${upstream.address().port}/fixture`;const a=await appFor(t,config,async(url,init)=>{assert.equal(url,`https://api.usepod.ai/proxy/${TOKEN}/v1/chat/completions`);assert.equal(init.redirect,'error');assert.equal(JSON.parse(init.body).stream,false);return fetch(fixture,init);});const response=await post(a.base);assert.equal(response.status,200);assert.deepEqual(await response.json(),completion());assert.equal(response.headers.get('X-Balance-Remaining'),null);assert.equal(a.ledger.totals().lifetime,20);assert.equal(calls,1);});

test('completed response survives restart; replay and conflicting explicit keys never pay twice',async t=>{const config=configuration(temp(t));let count=0;const fetcher=async()=>{count++;return Response.json(completion());};let a=createProvider(config,{testFetch:fetcher}),addr=await a.listen(),base=`http://127.0.0.1:${addr.port}`;assert.equal((await post(base,request(),{'idempotency-key':'job-1'})).status,200);await a.close();a=createProvider(config,{testFetch:fetcher});addr=await a.listen();base=`http://127.0.0.1:${addr.port}`;t.after(()=>a.close());const replay=await post(base,request(),{'idempotency-key':'job-1'});assert.equal(replay.status,200);assert.equal(replay.headers.get('x-covenant-provider-replay'),'true');assert.equal((await post(base,request({max_tokens:21}),{'idempotency-key':'job-1'})).status,409);assert.equal(count,1);});

test('cross-instance concurrency reserves once; a blocked request never reaches the provider',async t=>{const config=configuration(temp(t));let release,started;const entered=new Promise(r=>started=r);const block=new Promise(r=>release=r);let count=0;const fetcher=async()=>{count++;started();await block;return Response.json(completion());};const a=await appFor(t,config,fetcher),b=await appFor(t,config,fetcher);const first=post(a.base,request(),{'idempotency-key':'first'});await entered;const second=await post(b.base,request(),{'idempotency-key':'second'});assert.equal(second.status,429);assert.equal(count,1);release();assert.equal((await first).status,200);});

test('unknown timeout retains full reservation across restart and day boundary, with no retry',async t=>{const config=configuration(temp(t),{timeoutMs:15});let count=0;const fetcher=async(_url,{signal})=>{count++;await new Promise((resolve,reject)=>{signal.addEventListener('abort',()=>reject(new Error(`DO NOT LEAK https://api.usepod.ai/proxy/${TOKEN}/v1`)),{once:true});});};let a=createProvider(config,{testFetch:fetcher}),addr=await a.listen(),base=`http://127.0.0.1:${addr.port}`;let response=await post(base,request(),{'idempotency-key':'unknown-1'});assert.equal(response.status,503);assert.ok(!(await response.text()).includes(TOKEN));assert.equal(a.ledger.totals().lifetime,2040);await a.close();a=createProvider(config,{testFetch:fetcher,clock:()=>Date.now()+172800000});addr=await a.listen();base=`http://127.0.0.1:${addr.port}`;t.after(()=>a.close());assert.equal(a.ledger.totals(Date.now()+172800000).daily,2040);assert.equal((await post(base,request(),{'idempotency-key':'unknown-1'})).status,503);assert.equal((await post(base,request(),{'idempotency-key':'new-key'})).status,429);assert.equal(count,1);});

test('process interruption between reserve and dispatch retains hold on reopen',t=>{const config=configuration(temp(t));const a=new Ledger(config);a.reserve('crashed','digest',20,1000);a.close();const b=new Ledger(config);t.after(()=>b.close());assert.throws(()=>b.reserve('another','newdigest',20,900000000),/provider_busy_or_unresolved/);assert.equal(b.totals(900000000).daily,2040);});

test('invalid upstream response/usage/redirect leaves uncertain reservation and generic error',async t=>{for(const [name,reply] of [['error',new Response(`https://api.usepod.ai/proxy/${TOKEN}/secret`,{status:500})],['missing_usage',Response.json(completion({usage:undefined}))],['overcap',Response.json(completion({usage:{prompt_tokens:5000,completion_tokens:5,total_tokens:5005}}))],['secret',Response.json(completion({id:TOKEN}))],['redirect',new Response(null,{status:302,headers:{location:'https://attacker.invalid'}})]]){await t.test(name,async t=>{let count=0;const a=await appFor(t,configuration(temp(t)),async()=>{count++;return reply;});const r=await post(a.base);assert.equal(r.status,503);assert.ok(!(await r.text()).includes(TOKEN));assert.equal(a.ledger.totals().lifetime,2040);assert.equal(a.ledger.totals().unresolved,1);assert.equal(count,1);});}});

test('daily committed costs roll off; lifetime costs and uncertain holds never do',t=>{const config=configuration(temp(t),{lifetimeBudget:2100,dailyBudget:2040}),a=new Ledger(config);t.after(()=>a.close());a.reserve('1','a',20,1000);a.complete('1',{inputTokens:10,outputTokens:5,response:'{}'},1000);assert.throws(()=>a.reserve('2','b',20,1001),/provider_budget_exhausted/);a.reserve('2','b',20,86402000);a.complete('2',{inputTokens:10,outputTokens:5,response:'{}'},86402000);assert.equal(a.totals(86402000).lifetime,40);assert.equal(a.totals(86402000).daily,20);assert.throws(()=>a.reserve('3','c',100,200000000),/provider_budget_exhausted/);});

test('changing provider identity cannot silently reuse existing accounting',t=>{const config=configuration(temp(t)),a=new Ledger(config);a.close();assert.throws(()=>new Ledger({...config,token:'another_valid_provider_token_secret'}),/identity changed/);});

test('microunit accounting rounds upward with exact integer arithmetic',()=>{assert.equal(ceilingCost(1,1),1);assert.equal(ceilingCost(1000001,999999),1000000);assert.equal(validateBody(request({max_tokens:undefined}),configuration('/tmp')).max_tokens,100);});

 test('identical legitimate jobs without an explicit key receive independent inference',async t=>{let calls=0;const a=await appFor(t,configuration(temp(t)),async()=>{calls++;return Response.json(completion({id:`chatcmpl-${calls}`}));});const one=await (await post(a.base)).json(),two=await (await post(a.base)).json();assert.notEqual(one.id,two.id);assert.equal(calls,2);assert.equal(a.ledger.totals().lifetime,40);});

test('local health/catalog remove capacity when budget is insufficient or outcome is unresolved',async t=>{const a=await appFor(t,configuration(temp(t)),async()=>{throw new Error('unknown');});assert.equal((await (await fetch(`${a.base}/health`)).json()).ready,true);assert.equal((await (await fetch(`${a.base}/v1/models`)).json()).data.length,1);await post(a.base);assert.equal((await (await fetch(`${a.base}/health`)).json()).status,'blocked');assert.equal((await (await fetch(`${a.base}/v1/models`)).json()).data.length,0);const b=await appFor(t,configuration(temp(t),{lifetimeBudget:100,dailyBudget:100}),async()=>{throw new Error('must not dispatch');});assert.equal((await (await fetch(`${b.base}/health`)).json()).ready,false);assert.equal((await (await fetch(`${b.base}/v1/models`)).json()).data.length,0);});

test('a nonempty result with zero usage cannot release the reserved budget',async t=>{const a=await appFor(t,configuration(temp(t)),async()=>Response.json(completion({usage:{prompt_tokens:0,completion_tokens:0,total_tokens:0}})));assert.equal((await post(a.base)).status,503);assert.equal(a.ledger.totals().lifetime,2040);assert.equal(a.ledger.totals().unresolved,1);});

 test('fresh concurrent processes serialize initialization and a single spending reservation',async t=>{const config=configuration(temp(t)),moduleUrl=new URL('./ledger.mjs',import.meta.url).href;const outputs=await Promise.all(Array.from({length:4},(_,i)=>new Promise((resolve,reject)=>{const code=`import {Ledger} from ${JSON.stringify(moduleUrl)};const l=new Ledger(${JSON.stringify(config)});try{l.reserve('process-${i}','digest-${i}',20);process.stdout.write('reserved');}catch(e){process.stdout.write(e.code??'unexpected');}finally{l.close();}`;const child=spawn(process.execPath,['--input-type=module','-e',code],{stdio:['ignore','pipe','pipe']});let out='',err='';child.stdout.on('data',b=>out+=b);child.stderr.on('data',b=>err+=b);child.on('error',reject);child.on('close',status=>status===0?resolve(out):reject(new Error(`child failed: ${err}`)));})));assert.equal(outputs.filter(x=>x==='reserved').length,1);assert.equal(outputs.filter(x=>x==='provider_busy_or_unresolved').length,3);});

test('audited terminal loss retains both full budgets forever, blocks its key and permits only bounded new work',t=>{
 const config=configuration(temp(t),{lifetimeBudget:4100,dailyBudget:4100}),digest='a'.repeat(64),auditReference=`sha256:${'b'.repeat(64)}`;
 let ledger=new Ledger(config);ledger.reserve('key:failed',digest,20,1000);ledger.unknown('key:failed','provider_response_model_mismatch');
 const loss={id:'key:failed',requestDigest:digest,reservedMicroUsdc:2040,auditReference};
 assert.throws(()=>ledger.abandonUnknown({...loss,auditReference:'freeform secret or unaudited'}),/invalid_loss_reconciliation/);
 assert.throws(()=>ledger.abandonUnknown({...loss,requestDigest:'c'.repeat(64)}),/loss_reconciliation_mismatch/);
 assert.throws(()=>ledger.abandonUnknown({...loss,reservedMicroUsdc:20}),/loss_reconciliation_mismatch/);
 assert.equal(ledger.totals(1001).unresolved,1);
 assert.deepEqual(ledger.abandonUnknown(loss,2000),{state:'abandoned_full_loss',accounted_micro_usdc:2040,audit_reference:auditReference});
 assert.equal(ledger.abandonUnknown(loss,2001).accounted_micro_usdc,2040);
 assert.throws(()=>ledger.abandonUnknown({...loss,auditReference:`sha256:${'c'.repeat(64)}`}),/loss_reconciliation_conflict/);
 assert.throws(()=>ledger.db.prepare('DELETE FROM abandoned_losses').run(),/immutable/);
 ledger.close();ledger=new Ledger(config);t.after(()=>ledger.close());
 assert.deepEqual({...ledger.totals(900000000)},{lifetime:2040,daily:2040,unresolved:0});
 assert.throws(()=>ledger.reserve('key:failed',digest,20,900000000),/provider_request_abandoned/);
 assert.throws(()=>ledger.reserve('key:failed','different',20,900000000),/idempotency_conflict/);
 ledger.reserve('key:new','new',20,900000000);ledger.complete('key:new',{inputTokens:10,outputTokens:5,response:'{}'},900000001);
 assert.deepEqual({...ledger.totals(900000001)},{lifetime:2060,daily:2060,unresolved:0});
 ledger.reserve('key:last','last',20,900000002);ledger.complete('key:last',{inputTokens:10,outputTokens:5,response:'{}'},900000003);
 assert.throws(()=>ledger.reserve('key:over','over',20,900000004),/provider_budget_exhausted/);
 assert.equal(ledger.inspect('key:failed').state,'unknown');assert.equal(ledger.inspect('key:failed').accounted,2040);
});

test('loss reconciliation cannot abandon an in-flight or completed reservation',t=>{
 const ledger=new Ledger(configuration(temp(t)));t.after(()=>ledger.close());const digest='a'.repeat(64),loss={id:'job',requestDigest:digest,reservedMicroUsdc:2040,auditReference:`sha256:${'b'.repeat(64)}`};
 ledger.reserve('job',digest,20);assert.throws(()=>ledger.abandonUnknown(loss),/loss_reconciliation_mismatch/);
 ledger.complete('job',{inputTokens:10,outputTokens:5,response:'{}'});assert.throws(()=>ledger.abandonUnknown(loss),/loss_reconciliation_mismatch/);
 assert.equal(ledger.totals().lifetime,20);
});

test('legacy calls migrate additively and uncertain records retain identical financial fields',t=>{
 const config=configuration(temp(t)),db=new DatabaseSync(config.dbPath);
 db.exec("CREATE TABLE calls(id TEXT PRIMARY KEY,digest TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('reserved','unknown','complete')),reserved INTEGER NOT NULL,accounted INTEGER NOT NULL,created_ms INTEGER NOT NULL,finished_ms INTEGER,input_price INTEGER NOT NULL,output_price INTEGER NOT NULL,max_input_tokens INTEGER NOT NULL,max_output_tokens INTEGER NOT NULL,input_tokens INTEGER,output_tokens INTEGER,response TEXT,error_code TEXT)");
 db.prepare("INSERT INTO calls(id,digest,state,reserved,accounted,created_ms,input_price,output_price,max_input_tokens,max_output_tokens) VALUES(?,?,'unknown',2040,2040,1000,1000000,2000000,2000,20)").run('legacy','a'.repeat(64));
 const before=db.prepare('SELECT * FROM calls').get();db.close();const ledger=new Ledger(config);t.after(()=>ledger.close());
 ledger.abandonUnknown({id:'legacy',requestDigest:'a'.repeat(64),reservedMicroUsdc:2040,auditReference:`sha256:${'b'.repeat(64)}`});
 assert.deepEqual(ledger.db.prepare('SELECT * FROM calls').get(),before);assert.equal(ledger.totals().unresolved,0);assert.equal(ledger.totals().daily,2040);
});

test('invalid provider responses persist fixed reason codes and safe diagnostics without raw bodies or secrets',async t=>{
 for(const [name,reply,code] of [
  ['model',completion({model:'private-unexpected-model'}),'provider_response_model_mismatch'],
  ['content',completion({choices:[{message:{content:null},finish_reason:'stop'}]}),'provider_response_content_missing'],
  ['finish',completion({choices:[{message:{content:'PRIVATE RESPONSE BODY'},finish_reason:'https://provider.invalid/private-error'}]}),'provider_response_finish_reason'],
  ['missing-usage',completion({usage:undefined}),'provider_usage_missing'],
  ['json','not json PRIVATE RESPONSE BODY','provider_response_invalid_json'],
  ['secret',completion({id:TOKEN}),'provider_response_contains_secret'],
  ['escaped-secret',JSON.stringify(completion({id:TOKEN})).replace(TOKEN,'\\u0070'+TOKEN.slice(1)),'provider_response_contains_secret']
 ])await t.test(name,async t=>{
  const raw=typeof reply==='string'?reply:JSON.stringify(reply),app=await appFor(t,configuration(temp(t)),async()=>new Response(raw,{status:200}));
  const response=await post(app.base,request(),{'idempotency-key':'diagnostic'});assert.equal(response.status,503);assert.equal((await response.json()).error.code,code);
  const row=app.ledger.inspect('key:diagnostic'),serialized=JSON.stringify(row);
  assert.equal(row.error_code,code);assert.equal(row.diagnostics.http_status,200);assert.match(row.diagnostics.response_sha256,/^[a-f0-9]{64}$/);
  assert.equal(row.diagnostics.response_bytes,Buffer.byteLength(raw));
  for(const secret of [TOKEN,AUTH,'PRIVATE RESPONSE BODY','private-unexpected-model','https://provider.invalid'])assert.ok(!serialized.includes(secret));
  assert.equal(app.ledger.db.prepare('SELECT response FROM calls').get().response,null);
  assert.deepEqual({...app.ledger.totals()},{lifetime:2040,daily:2040,unresolved:1});
 });
});


test('local reconciliation CLI requires exact audit fields and never changes or retries the failed call',async t=>{
 const dir=temp(t),config=configuration(dir),ledger=new Ledger(config),digest='a'.repeat(64),audit='b'.repeat(64);
 ledger.reserve('key:cli',digest,20);ledger.unknown('key:cli','provider_response_model_mismatch');ledger.close();
 const run=args=>new Promise((resolve,reject)=>{const child=spawn(process.execPath,[new URL('./reconcile.mjs',import.meta.url).pathname,...args],{env:validEnv(dir),stdio:['ignore','pipe','pipe']});let stdout='',stderr='';child.stdout.on('data',x=>stdout+=x);child.stderr.on('data',x=>stderr+=x);child.on('error',reject);child.on('close',status=>resolve({status,stdout,stderr}));});
 const inspect=await run(['--inspect','key:cli']);assert.equal(inspect.status,0);assert.equal(JSON.parse(inspect.stdout).reserved,2040);
 const bad=await run(['--abandon-full-reservation','--id','key:cli','--request-sha256',digest,'--reserved-micro-usdc','20','--audit-sha256',audit]);assert.equal(bad.status,1);assert.ok(!bad.stderr.includes(TOKEN));
 const good=await run(['--abandon-full-reservation','--id','key:cli','--request-sha256',digest,'--reserved-micro-usdc','2040','--audit-sha256',audit]);assert.equal(good.status,0);assert.equal(JSON.parse(good.stdout).state,'abandoned_full_loss');
 const reopened=new Ledger(config);t.after(()=>reopened.close());assert.equal(reopened.totals().lifetime,2040);assert.equal(reopened.totals().unresolved,0);assert.throws(()=>reopened.reserve('key:cli',digest,20),/provider_request_abandoned/);
});
