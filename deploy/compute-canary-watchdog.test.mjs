import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {EventEmitter} from 'node:events';
import {PassThrough} from 'node:stream';
import {ENDPOINT,OUTAGE_SECONDS,initialState,observe,freshFeed,Journal,tick,alertConfig,cloudflareAccepted,emailFor,deliver,boundedJson,isolatedEnvironment,startWatchdog} from './compute-canary-watchdog.mjs';
const NOW=1_792_366_400;
const env={COVENANT_CANARY_ALERT_CLOUDFLARE_API_TOKEN:'offline-fixture-token',COVENANT_CANARY_ALERT_CLOUDFLARE_ACCOUNT_ID:'1'.repeat(32),COVENANT_CANARY_ALERT_FROM:'compute-alerts@opencovenant.org',COVENANT_CANARY_ALERT_TO:'alert@opencovenant.org'};
const config=alertConfig(env);
const good=()=>({schema:1,server_time_unix:NOW,generated_at_unix:NOW-1,stale_after_seconds:180,observed_state_age_seconds:1,is_stale:false,feed_status:'fresh',enabled:true,cadence_seconds:21600,next_due_at_unix:null,pause_reason:null,budget:null,monitor:null,latest:null,history:[]});
const accepted={success:true,errors:[],result:{delivered:[],queued:['alert@opencovenant.org'],permanent_bounces:[]}};
function temporary(t){const dir=fs.mkdtempSync(path.join(os.tmpdir(),'covenant-watchdog-'));t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));return dir;}
function outage(){return observe(observe(initialState(NOW),false,NOW),false,NOW+OUTAGE_SECONDS);}

// A stale feed is a reporting failure, not a failed hardware test.
test('fresh heartbeat and fresh intentional pause are visible',()=>{
  assert.equal(freshFeed(good(),NOW),true);
  assert.equal(freshFeed({...good(),enabled:false,feed_status:'disabled',pause_reason:'pilot_budget_exhausted'},NOW),true);
});
test('stale disabled snapshots and disabled skeletons remain missing visibility',()=>{
  assert.equal(freshFeed({...good(),enabled:false,feed_status:'disabled',is_stale:true},NOW),false);
  assert.equal(freshFeed({...good(),enabled:false,feed_status:'disabled',generated_at_unix:null,observed_state_age_seconds:null,is_stale:true},NOW),false);
});
test('invalid, future, cached and malformed freshness fields are rejected',()=>{
  for(const change of [{schema:2},{cadence_seconds:1},{stale_after_seconds:99999},{server_time_unix:NOW-121},{generated_at_unix:NOW+31},{generated_at_unix:NOW-181,observed_state_age_seconds:181},{observed_state_age_seconds:0},{enabled:'false'},{is_stale:'false'},{feed_status:'invalid'},{history:{}},{history:Array(41).fill({})},{latest:[]},{next_due_at_unix:-1},{pause_reason:'https://secret.test'}])assert.equal(freshFeed({...good(),...change},NOW),false,JSON.stringify(change));
  for(const invalid of [null,[],{},0,'ok'])assert.equal(freshFeed(invalid,NOW),false);
});
test('exactly 18 elapsed hours triggers one durable incident, not three polls',()=>{
  let state=initialState(NOW);
  state=observe(state,false,NOW);const id=state.incident_id;
  for(const at of [NOW+60,NOW+120,NOW+OUTAGE_SECONDS-1]){state=observe(state,false,at);assert.equal(state.outbox.length,0);}
  state=observe(state,false,NOW+OUTAGE_SECONDS);assert.equal(state.outbox.length,1);assert.equal(state.outbox[0].kind,'missing');assert.equal(state.incident_id,id);
  state=observe(state,false,NOW+OUTAGE_SECONDS*2);assert.equal(state.outbox.length,1);
});
test('clock rollback refuses to erase or shorten outage history',()=>{
  const state=observe(initialState(NOW),false,NOW+60);
  assert.throws(()=>observe(state,true,NOW));assert.equal(state.first_missing_at_unix,NOW+60);
});
test('fresh feed before threshold resets missing timer without a recovery message',()=>{
  let state=observe(initialState(NOW),false,NOW);state=observe(state,true,NOW+600);
  assert.equal(state.first_missing_at_unix,null);assert.equal(state.outbox.length,0);
  state=observe(state,false,NOW+700);assert.equal(state.first_missing_at_unix,NOW+700);
});
test('outage and reporting-restored notifications retain ordering and distinct claims',()=>{
  const missing=outage(), restored=observe(missing,true,NOW+OUTAGE_SECONDS+1);
  assert.deepEqual(restored.outbox.map(e=>e.kind),['missing','restored']);assert.equal(restored.outbox[0].id,missing.outbox[0].id);
  assert.equal(restored.outbox[1].incident_id,missing.incident_id);assert.equal(restored.incident_id,null);
  const text=emailFor(restored.outbox[1],config).text;assert.match(text,/does not mean a GPU rental check passed/);
  assert.equal(observe(restored,true,NOW+OUTAGE_SECONDS+2).outbox.length,2);
});
test('corrupt/private journal cannot silently reset outage accounting',t=>{
  const dir=temporary(t),journal=new Journal(dir);journal.write(outage());
  assert.equal(new Journal(dir).read(NOW+OUTAGE_SECONDS).outbox[0].kind,'missing');
  assert.equal(fs.statSync(journal.file).mode&0o777,0o600);assert.equal(fs.statSync(dir).mode&0o777,0o700);
  fs.writeFileSync(journal.file,'{broken');assert.throws(()=>journal.read(NOW));
});
test('journal rejects symlink, world-readable and wrong-endpoint state',t=>{
  const dir=temporary(t),journal=new Journal(dir);journal.write(initialState(NOW));fs.chmodSync(journal.file,0o644);assert.throws(()=>journal.read(NOW));
  fs.unlinkSync(journal.file);const target=path.join(dir,'other');fs.writeFileSync(target,JSON.stringify(initialState(NOW)),{mode:0o600});fs.symlinkSync(target,journal.file);assert.throws(()=>journal.read(NOW));
  assert.throws(()=>journal.write({...initialState(NOW),endpoint:'https://other.example/'}));
});
test('pending delivery survives a fresh process with exact same event ID',async t=>{
  const dir=temporary(t),journal=new Journal(dir);journal.write(observe(initialState(NOW),false,NOW));const seen=[];
  const offline=async()=>{throw Error('offline');};
  await tick({journal,config,now:NOW+OUTAGE_SECONDS,transport:offline,send:async event=>{seen.push(event.id);throw Error('unknown outcome');}});
  const disk=new Journal(dir).read(NOW+OUTAGE_SECONDS);assert.equal(disk.outbox.length,1);
  await tick({journal:new Journal(dir),config,now:NOW+OUTAGE_SECONDS+60,transport:offline,send:async event=>{seen.push(event.id);}});
  assert.equal(seen[0],seen[1]);assert.equal(new Journal(dir).read(NOW).outbox.length,0);assert.equal(new Journal(dir).read(NOW).delivered_count,1);
});
test('no alert before threshold and only fixed read-only probe occurs',async t=>{
  const journal=new Journal(temporary(t)),calls=[];let sends=0;
  const result=await tick({journal,config,now:NOW,transport:async(url,opts)=>{calls.push([url,opts]);return good();},send:async()=>{sends++;}});
  assert.equal(result.fresh,true);assert.deepEqual(calls,[[ENDPOINT,undefined]]);assert.equal(sends,0);
});
test('outbox exists on disk before any delivery attempt',async t=>{
  const journal=new Journal(temporary(t));journal.write(observe(initialState(NOW),false,NOW));
  await tick({journal,config,now:NOW+OUTAGE_SECONDS,transport:async()=>({}),send:async e=>{assert.equal(journal.read(NOW).outbox[0].id,e.id);}});
  assert.equal(journal.read(NOW).outbox.length,0);
});
test('malformed and recipient-mismatched Cloudflare results do not acknowledge delivery',()=>{
  assert.equal(cloudflareAccepted(accepted),true);
  for(const value of [null,{}, {...accepted,success:false},{...accepted,errors:[{}]}, {...accepted,result:{...accepted.result,queued:['elsewhere@example.com']}},{...accepted,result:{...accepted.result,permanent_bounces:['alert@opencovenant.org']}},{...accepted,result:{...accepted.result,suppressed_recipients:['alert@opencovenant.org']}},{...accepted,result:{...accepted.result,suppressed_recipients:'invalid'}}])assert.equal(cloudflareAccepted(value),false);
});
test('delivery uses fixed email endpoint, exact idempotency key and rejects vague success',async()=>{
  const event=outage().outbox[0];let seen;
  await deliver(event,config,async(url,opts)=>{seen={url,opts};return accepted;});
  assert.equal(seen.url,`https://api.cloudflare.com/client/v4/accounts/${config.account}/email/sending/send`);assert.equal(seen.opts.method,'POST');assert.equal(seen.opts.headers['idempotency-key'],event.id);
  assert.equal(JSON.parse(seen.opts.body).to,'alert@opencovenant.org');await assert.rejects(()=>deliver(event,config,async()=>({success:true})));
});
function fakeRequest({status=200,headers={'content-type':'application/json'},body='{}',hang=false}={}){
  return (_url,_opts,callback)=>{
    const req=new EventEmitter();req.destroy=()=>{};
    req.end=()=>{if(hang)return;const res=new PassThrough();res.statusCode=status;res.headers=headers;callback(res);queueMicrotask(()=>res.end(body));};
    return req;
  };
}
test('bounded transport accepts JSON and rejects redirects, HTML, encoding, oversize and truncated bodies',async()=>{
  assert.deepEqual(await boundedJson(ENDPOINT,{request:fakeRequest()}),{});
  for(const options of [{status:302},{headers:{'content-type':'text/html'}},{headers:{'content-type':'application/json','content-encoding':'gzip'}},{headers:{'content-type':'application/json','content-length':'999999'}},{headers:{'content-type':'application/json','content-length':'5'}},{body:'x'.repeat(20)},{body:'not json'},{body:Buffer.from([0x22,0xff,0x22])}])await assert.rejects(()=>boundedJson(ENDPOINT,{request:fakeRequest(options),limit:10}));
});
test('whole-request timeout covers a connection that never responds',async()=>{
  await assert.rejects(()=>boundedJson(ENDPOINT,{request:fakeRequest({hang:true}),timeoutMs:5}));
});
test('network transport refuses arbitrary hosts, redirects, paid endpoints and methods before connecting',async()=>{
  let calls=0;const request=()=>{calls++;throw Error('must not connect');};
  for(const [url,method,body] of [[ENDPOINT,'POST','{}'],['http://compute-api.opencovenant.org/rental-checks','GET'],['https://compute-api.opencovenant.org/leases','POST','{}'],['https://evil.example/','GET'],['https://api.cloudflare.com/client/v4/accounts/'+'1'.repeat(32)+'/workers/scripts/evil','POST','{}']])await assert.rejects(()=>boundedJson(url,{method,body,request}));
  assert.equal(calls,0);
});
test('child environment excludes wallet, provider, payment, shell and Node option secrets',t=>{
  const dirty={...env,PATH:'/unsafe',NODE_OPTIONS:'--require unsafe',COVENANT_GATEWAY_IDENTITY_B64:'secret',VAST_API_KEY:'secret',COVENANT_GATEWAY_BRIDGE_SECRET:'secret'};
  const childEnv=isolatedEnvironment(dirty,temporary(t));
  assert.deepEqual(Object.keys(childEnv).sort(),['COVENANT_CANARY_WATCHDOG_HOME',...Object.keys(env)].sort());
  assert.ok(!JSON.stringify(childEnv).includes('secret'));
});
test('opt-in isolated child uses crash-safe flock and restarts without financial callbacks',t=>{
  const calls=[],timers=[],logs=[];let child;
  const stop=startWatchdog({...env,COVENANT_CANARY_WATCHDOG_ENABLED:'true'},temporary(t),{
    spawnChild:(command,args,opts)=>{child=new EventEmitter();child.kill=signal=>calls.push({signal});calls.push({command,args,opts});return child;},
    setTimer:(fn,ms)=>{timers.push({fn,ms});return 1;},clearTimer:()=>{},log:()=>logs.push('restart')});
  assert.equal(calls[0].command,'/usr/bin/flock');assert.equal(calls[0].args[0],'--no-fork');assert.equal(calls[0].opts.env.COVENANT_CANARY_WATCHDOG_HOME.endsWith('/rental-watchdog'),true);
  child.emit('exit',1);child.emit('error',Error('duplicate'));assert.equal(timers.length,1);assert.equal(timers[0].ms,30000);timers[0].fn();assert.equal(calls.length,2);
  stop();assert.equal(calls.at(-1).signal,'SIGTERM');child.emit('exit',0);assert.equal(timers.length,1);
});
test('watcher defaults off and invalid config never starts a process',()=>{
  let calls=0;const spawnChild=()=>{calls++;};
  startWatchdog({},'/data',{spawnChild})();startWatchdog({COVENANT_CANARY_WATCHDOG_ENABLED:'false'},'/data',{spawnChild})();
  assert.throws(()=>startWatchdog({COVENANT_CANARY_WATCHDOG_ENABLED:'true'},'/data',{spawnChild}));
  assert.throws(()=>startWatchdog({COVENANT_CANARY_WATCHDOG_ENABLED:'yes'},'/data',{spawnChild}));assert.equal(calls,0);
});
