import fs from 'node:fs';
import path from 'node:path';
import {startWatchdog} from '../compute-canary-watchdog.mjs';
import * as runtime from './runtime.mjs';
const {privateDirectory,seed,latch,missing,clearStartingLatch,explicitBoolean,operatorPubkey,startInferenceOperator,ProcessGroup,waitFor}=runtime;

process.umask(0o077);
let group,stopWatchdog=()=>{};
try{
  const data=process.env.COVENANT_X402_DATA;
  if(!data||!path.isAbsolute(data))throw Error('Persistent directory required');
  const operatorEnabled=explicitBoolean(process.env,'COVENANT_X402_OPERATOR_ENABLED');
  explicitBoolean(process.env,'COVENANT_GATEWAY_ADMISSIONS_ENABLED');
  const gateway=path.join(data,'gateway'),operator=path.join(data,'operator'),budget=path.join(data,'budget');
  for(const dir of [gateway,budget])privateDirectory(dir);
  const disabled=path.join(gateway,'admissions.disabled'),starting=path.join(gateway,'admissions.starting');
  latch(starting,'Startup is not yet ready');
  group=new ProcessGroup({
    onOptionalFailure:reason=>latch(disabled,reason),
    onBeforeStop:()=>{stopWatchdog();latch(starting,'Shutdown in progress');},
  });
  for(const signal of ['SIGINT','SIGTERM'])process.on(signal,()=>group.stop(0));
  // Start independently before coordinator-dependent optional operator startup.
  // Configuration or observer failure never changes financial worker behavior.
  try{stopWatchdog=startWatchdog(process.env,data);}catch{process.stderr.write('External rental observer unavailable; check observer configuration.\n');}
  seed(path.join(gateway,'identity.key'),process.env.COVENANT_GATEWAY_IDENTITY_B64,32);
  // One key file per fee payer address, so rotating the fee payer seeds a new
  // file instead of colliding with the old persisted key. A volume seeded
  // before this layout keeps its single file until a new key is supplied.
  const feePayer=process.env.COVENANT_GATEWAY_FEE_PAYER;
  if(typeof feePayer!=='string'||!/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(feePayer))throw Error('Fee payer address required');
  const legacyFeePayerFile=path.join(gateway,'fee-payer.json');
  let feePayerFile=path.join(gateway,`fee-payer-${feePayer}.json`);
  if(process.env.COVENANT_GATEWAY_FEE_PAYER_KEY_B64===undefined&&!fs.existsSync(feePayerFile)&&fs.existsSync(legacyFeePayerFile))feePayerFile=legacyFeePayerFile;
  seed(feePayerFile,process.env.COVENANT_GATEWAY_FEE_PAYER_KEY_B64);
  // This key authenticates persisted payment seals: rotating it strands recovery.
  const bridgeSecret=process.env.COVENANT_GATEWAY_BRIDGE_SECRET;
  if(typeof bridgeSecret!=='string'||bridgeSecret.length<32)throw Error('Persistent bridge secret required');
  seed(path.join(gateway,'bridge-secret'),Buffer.from(bridgeSecret).toString('base64'));
  let tariffFiles={};
  const supportsTariffs=typeof runtime.materializeTariffs==='function';
  try{
    if(supportsTariffs)tariffFiles=runtime.materializeTariffs(process.env,data);
    // The observer-only overlay preserves a pre-tariff runtime and binaries.
    // It may keep serving legacy requests, but must never ignore tariff setup.
    else if(['COVENANT_GATEWAY_TARIFFS_JSON','COVENANT_GATEWAY_TARIFFS_FILE','COVENANT_COMPUTE_NODE_TARIFFS_FILE'].some(key=>process.env[key]!==undefined))throw Error('Tariff runtime unavailable');
  }catch{
    latch(disabled,supportsTariffs?'Invalid tariff configuration; payment recovery only':'Tariffs require the full pricing runtime image; payment recovery only');
    // A malformed optional price file must not stop the essential processes.
    tariffFiles={COVENANT_GATEWAY_TARIFFS_FILE:undefined,COVENANT_COMPUTE_NODE_TARIFFS_FILE:undefined};
  }
  const env={...process.env,...tariffFiles,
    COVENANT_GATEWAY_HOME:gateway,COVENANT_COMPUTE_NODE_HOME:operator,COVENANT_COMPUTE_BUDGET_HOME:budget,
    COVENANT_GATEWAY_LISTEN:`0.0.0.0:${process.env.PORT??'10000'}`,
    COVENANT_GATEWAY_BRIDGE_URL:'http://127.0.0.1:4043',COVENANT_GATEWAY_BRIDGE_PORT:'4043',
    COVENANT_GATEWAY_FEE_PAYER_KEY_FILE:feePayerFile,
    COVENANT_INFERENCE_PROVIDER_PORT:'4044',COVENANT_INFERENCE_PROVIDER_DB_PATH:path.join(data,'provider','calls.sqlite'),
    COVENANT_COMPUTE_OPENAI_URL:'http://127.0.0.1:4044/v1',
    COVENANT_COMPUTE_OPENAI_API_KEY:process.env.COVENANT_INFERENCE_PROVIDER_BEARER_TOKEN};
  delete env.COVENANT_GATEWAY_IDENTITY_B64;delete env.COVENANT_COMPUTE_NODE_IDENTITY_B64;delete env.COVENANT_GATEWAY_FEE_PAYER_KEY_B64;
  delete env.COVENANT_GATEWAY_TARIFFS_JSON;
  // Base facilitator credentials and the Base RPC URL, which may carry a
  // provider key, are for the verifier only.
  const verifierOnly=['CDP_API_KEY_ID','CDP_API_KEY_SECRET','COVENANT_GATEWAY_BASE_RPC','COVENANT_GATEWAY_BASE_FACILITATOR_URL'];
  const verifierEnv=Object.fromEntries(verifierOnly.filter(key=>env[key]!==undefined).map(key=>[key,env[key]]));
  for(const key of verifierOnly)delete env[key];
  group.start('payment verifier',process.execPath,['services/compute-x402-facilitator/server.mjs'],{...env,...verifierEnv});
  await waitFor('http://127.0.0.1:4043/health',{headers:{authorization:`Bearer ${env.COVENANT_GATEWAY_BRIDGE_SECRET}`},group});
  // Public port starts in recovery-only mode while the starting latch exists.
  group.start('inference gateway','covenant-compute-gateway',[],env);
  await waitFor(`http://127.0.0.1:${process.env.PORT??'10000'}/health`,{group});
  // A restart must not retry paid qualification after a latched failure.
  if(operatorEnabled&&missing(disabled)){
    try{
      for(const dir of [operator,path.join(data,'provider')])privateDirectory(dir);
      seed(path.join(operator,'identity.json'),process.env.COVENANT_COMPUTE_NODE_IDENTITY_B64,32);
      group.start('inference provider',process.execPath,['services/compute-inference-provider/server.mjs'],env,true);
      await waitFor('http://127.0.0.1:4044/health',{predicate:body=>body.ready===true,group});
      const pubkey=operatorPubkey(fs.readFileSync(path.join(operator,'identity.json')));
      if(!env.COVENANT_GATEWAY_APPROVED_OPERATOR_PUBKEYS?.split(',').map(x=>x.trim()).includes(pubkey))throw Error('Local operator must be approved');
      const started=Date.now();
      startInferenceOperator(group,env);
      const reputation=new URL(`/federation/operators/${pubkey}/reputation`,env.COVENANT_COMPUTE_COORDINATOR_URL);
      await waitFor(reputation,{group,attempts:180,predicate:body=>body.operator_pubkey_b58===pubkey&&body.matchable===true&&Number.isSafeInteger(body.seen_ms_ago)&&body.seen_ms_ago<=Date.now()-started});
      if(!group.optionalFailed&&!group.stopping)clearStartingLatch(starting);
    }catch{
      group.failOptional('Optional inference startup failed; review before re-enabling admissions');
      process.stderr.write('Inference admissions locked; payment recovery remains available.\n');
    }
  }
}catch{
  stopWatchdog();
  process.stderr.write('Gateway supervisor refused startup; check private configuration and persistent volume.\n');
  if(group)group.stop(1);else process.exitCode=1;
}
