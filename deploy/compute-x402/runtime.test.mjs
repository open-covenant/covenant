import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {EventEmitter} from 'node:events';
import {createRequire} from 'node:module';
const {getBase58Encoder}=createRequire(new URL('../../services/compute-x402-facilitator/package.json',import.meta.url))('@solana/kit');
import {privateDirectory,seed,latch,missing,clearStartingLatch,explicitBoolean,operatorPubkey,startInferenceOperator,ProcessGroup,waitFor} from './runtime.mjs';

function fixture(t){const dir=fs.mkdtempSync(path.join(os.tmpdir(),'covenant-supervisor-'));t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));return dir;}
test('operator launch uses the default serving command and optional-worker recovery',()=>{
 const env={COVENANT_COMPUTE_NODE_EXECUTOR:'openai-compat'},calls=[];
 const child=startInferenceOperator({start:(...args)=>{calls.push(args);return 'child';}},env);
 assert.equal(child,'child');assert.deepEqual(calls,[['inference operator','covenant-compute-node',[],env,true]]);
});
test('identity and seal secrets survive restart and reject rotation, malformed base64 and symlinks',t=>{
 const dir=fixture(t),key=path.join(dir,'key'),bytes=Buffer.alloc(32,7),encoded=bytes.toString('base64');
 privateDirectory(dir);seed(key,encoded,32);seed(key,undefined,32);seed(key,encoded,32);
 assert.deepEqual(fs.readFileSync(key),bytes);assert.equal(fs.statSync(key).mode&0o777,0o600);assert.equal(fs.statSync(dir).mode&0o777,0o700);
 assert.throws(()=>seed(key,Buffer.alloc(32,8).toString('base64'),32),/mismatch/);
 assert.throws(()=>seed(path.join(dir,'bad'),'%%%'+encoded,32),/encoding/);
 const link=path.join(dir,'link');fs.symlinkSync(key,link);assert.throws(()=>seed(link,encoded,32),/Unsafe/);
 const bridge=path.join(dir,'bridge-secret');seed(bridge,Buffer.from('a'.repeat(64)).toString('base64'));assert.throws(()=>seed(bridge,Buffer.from('b'.repeat(64)).toString('base64')),/mismatch/);
 // RFC8032 test vector, decoded with the official Solana codec.
 assert.equal(Buffer.from(getBase58Encoder().encode(operatorPubkey(Buffer.from('9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60','hex')))).toString('hex'),'d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a');
});
test('startup gate can clear without erasing a durable failure gate',t=>{
 const dir=fixture(t),starting=path.join(dir,'admissions.starting'),disabled=path.join(dir,'admissions.disabled');
 latch(starting,'starting');latch(disabled,'provider failed');latch(disabled,'different failure');clearStartingLatch(starting);
 assert.equal(missing(starting),true);assert.equal(missing(disabled),false);assert.equal(fs.readFileSync(disabled,'utf8'),'provider failed\n');
 const broken=path.join(dir,'broken');fs.symlinkSync(path.join(dir,'no-target'),broken);assert.equal(missing(broken),false);
 for(const value of [undefined,'yes','FALSE',''])assert.throws(()=>explicitBoolean({flag:value},'flag'));
 assert.equal(explicitBoolean({flag:'false'},'flag'),false);
});
function fakeSpawn(events){return(command)=>{const child=new EventEmitter();child.command=command;child.kill=signal=>{events.push(`${command}:${signal}`);queueMicrotask(()=>child.emit('exit',null,signal));};return child;};}
test('optional failure persists refusal before stopping workers and leaves recovery services alive',async()=>{
 const events=[],exits=[];const group=new ProcessGroup({spawnChild:fakeSpawn(events),onOptionalFailure:()=>events.push('latched'),onExit:code=>exits.push(code)});
 const bridge=group.start('bridge','bridge',[],{}),gateway=group.start('gateway','gateway',[],{});
 const provider=group.start('provider','provider',[],{},true);group.start('operator','operator',[],{},true);
 provider.emit('exit',1);await new Promise(resolve=>setImmediate(resolve));
 assert.deepEqual(events,['latched','operator:SIGTERM']);assert.deepEqual(exits,[]);assert.equal(group.stopping,false);
 assert.throws(()=>group.start('operator','replacement',[],{},true),/cancelled/);
 group.stop(0);await new Promise(resolve=>setImmediate(resolve));assert.deepEqual(exits,[0]);
 assert.ok(events.includes('bridge:SIGTERM'));assert.ok(events.includes('gateway:SIGTERM'));
});
test('essential child failure cancels further startup, including spawn errors',async()=>{
 const events=[],exits=[];const group=new ProcessGroup({spawnChild:fakeSpawn(events),onOptionalFailure:()=>{},onBeforeStop:()=>events.push('shutdown latch'),onExit:code=>exits.push(code)});
 const bridge=group.start('bridge','bridge',[],{});group.start('gateway','gateway',[],{});bridge.emit('error',new Error('spawn failed'));
 await new Promise(resolve=>setImmediate(resolve));assert.equal(events[0],'shutdown latch');assert.deepEqual(exits,[1]);assert.throws(()=>group.start('operator','operator',[],{},true),/cancelled/);
});
test('HTTP 200 blocked health is not provider readiness; stopping aborts boot',async()=>{
 let calls=0;await waitFor('http://localhost',{fetcher:async()=>({ok:true,json:async()=>({ready:++calls>1})}),predicate:body=>body.ready===true,intervalMs:0,attempts:2});assert.equal(calls,2);
 await assert.rejects(waitFor('http://localhost',{group:{stopping:true},fetcher:()=>{throw Error('must not fetch');}}),/cancelled/);
});

test('hung optional workers are killed without terminating payment recovery',async()=>{
 const events=[],exits=[];const spawnChild=command=>{const child=new EventEmitter();child.kill=signal=>{events.push(`${command}:${signal}`);if(signal==='SIGKILL')queueMicrotask(()=>child.emit('exit',null,signal));};return child;};
 const group=new ProcessGroup({spawnChild,shutdownMs:5,onOptionalFailure:()=>events.push('latched'),onExit:code=>exits.push(code)});
 group.start('bridge','bridge',[],{});group.start('operator','operator',[],{},true);group.failOptional('failed');
 await new Promise(resolve=>setTimeout(resolve,15));assert.deepEqual(events,['latched','operator:SIGTERM','operator:SIGKILL']);assert.deepEqual(exits,[]);
});
