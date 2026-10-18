import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';
import {createSmokeRpc,MAINNET_GENESIS} from './smoke-rpc.mjs';
import {validateImage,smokeEnvironment,verifyHealth,fixtureDiagnostics} from './smoke.mjs';
const revision='a'.repeat(40),image=`ghcr.io/open-covenant/covenant/compute-node-public@sha256:${'b'.repeat(64)}`;
test('container qualification requires immutable package and exact revision',()=>{
 validateImage(image,revision);
 for(const bad of ['ghcr.io/open-covenant/covenant/compute-node-public:latest',image.replace('ghcr.io','example.com'),image+'suffix'])assert.throws(()=>validateImage(bad,revision));
 assert.throws(()=>validateImage(image,revision.slice(0,8)));
});
test('smoke identities are fresh and both paid admission paths are disabled',()=>{
 const first=smokeEnvironment(),second=smokeEnvironment();
 for(const name of ['COVENANT_GATEWAY_ADMISSIONS_ENABLED','COVENANT_X402_OPERATOR_ENABLED','COVENANT_INFERENCE_PROVIDER_ENABLED'])assert.equal(first[name],'false');
 assert.equal(Buffer.from(first.COVENANT_GATEWAY_IDENTITY_B64,'base64').length,32);
 assert.equal(JSON.parse(Buffer.from(first.COVENANT_GATEWAY_FEE_PAYER_KEY_B64,'base64')).length,64);
 assert.notEqual(first.COVENANT_GATEWAY_IDENTITY_B64,second.COVENANT_GATEWAY_IDENTITY_B64);
 assert.notEqual(first.COVENANT_GATEWAY_BRIDGE_SECRET,second.COVENANT_GATEWAY_BRIDGE_SECRET);
 assert.equal(first.USEPOD_TOKEN,undefined);assert.equal(first.COVENANT_COMPUTE_NODE_IDENTITY_B64,undefined);
 assert.equal(first.COVENANT_COMPUTE_COORDINATOR_URL,'http://smoke-rpc:14045/coordinator');
});
test('readiness rejects the wrong build or enabled admissions',()=>{
 const health={status:'ok',service:'covenant-compute-x402',protocol:'x402-v2',revision,admissions_enabled:false};verifyHealth(health,revision);
 assert.throws(()=>verifyHealth({...health,admissions_enabled:true},revision));assert.throws(()=>verifyHealth({...health,revision:'unknown'},revision));
});
test('isolated RPC permits only genesis and counts forbidden dispatch attempts',async t=>{
 assert.throws(()=>createSmokeRpc('devnet'));
 const {server,counts}=createSmokeRpc(MAINNET_GENESIS);await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 t.after(()=>new Promise(resolve=>server.close(resolve)));const url=`http://127.0.0.1:${server.address().port}`;
 const rpc=method=>fetch(url+'/rpc',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({jsonrpc:'2.0',id:1,method,params:[]})});
 const read=await rpc('getGenesisHash');assert.equal(read.status,200);assert.equal((await read.json()).result,MAINNET_GENESIS);
 for(const method of ['sendTransaction','simulateTransaction','getLatestBlockhash']){const refused=await rpc(method);assert.equal(refused.status,403);await refused.arrayBuffer();}
 const coordinator=await fetch(url+'/coordinator/federation/jobs',{method:'POST',body:'{}'});assert.equal(coordinator.status,503);await coordinator.arrayBuffer();
 assert.deepEqual(counts,{genesis:1,refused:3,coordinator:1});
});

test('fixture CLI is reachable by Node fetch on the configured smoke port',{timeout:5000},async t=>{
 const child=spawn(process.execPath,[fileURLToPath(new URL('./smoke-rpc.mjs',import.meta.url))],{env:{...process.env,SMOKE_GENESIS:MAINNET_GENESIS},stdio:['ignore','pipe','pipe']});
 t.after(async()=>{if(child.exitCode===null&&child.signalCode===null){const exited=once(child,'exit');child.kill('SIGTERM');await exited;}});
 const listening=await new Promise((resolve,reject)=>{
  child.once('error',reject);
  child.once('exit',code=>reject(Error(`Fixture exited before readiness (${code})`)));
  child.stdout.once('data',data=>{try{resolve(JSON.parse(data.toString()));}catch(error){reject(error);}});
 });
 const configured=new URL(smokeEnvironment().COVENANT_GATEWAY_SOLANA_RPC);
 assert.equal(listening.status,'listening');assert.equal(String(listening.port),configured.port);
 const response=await fetch(`http://127.0.0.1:${configured.port}/audit`,{signal:AbortSignal.timeout(1000)});
 assert.equal(response.status,200);assert.deepEqual(await response.json(),{genesis:0,refused:0,coordinator:0});
});

test('diagnostics refuse to inspect or log the private app container',async()=>{
 await assert.rejects(fixtureDiagnostics('covenant-gateway-012345abcdef'),/Fixture-only/);
 await assert.rejects(fixtureDiagnostics('arbitrary-container'),/Fixture-only/);
});
