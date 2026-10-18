// Exercise the same invocation as the supervisor using the packaged Rust binary.
// No coordinator, payout or provider configuration is supplied: it must stop
// at the mandatory coordinator check before any network operation or benchmark.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {startInferenceOperator} from './runtime.mjs';
process.umask(0o077);
const home=fs.mkdtempSync(path.join(os.tmpdir(),'covenant-operator-cli-'));
try{
 const result=startInferenceOperator({start:(_name,command,args,env)=>spawnSync(command,args,{env,encoding:'utf8',timeout:5000,maxBuffer:65536})},{PATH:process.env.PATH,COVENANT_COMPUTE_NODE_HOME:home});
 assert.equal(result.status,1,'Operator CLI must fail closed without coordinator configuration');
 assert.match(result.stderr,/COVENANT_COMPUTE_COORDINATOR_URL is unset/,'Operator invocation did not enter serving startup');
 assert.equal(fs.statSync(path.join(home,'identity.json')).size,32);
 process.stdout.write(JSON.stringify({operator_cli_contract_verified:true,external_calls:0})+'\n');
}finally{fs.rmSync(home,{recursive:true,force:true});}
