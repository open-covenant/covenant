// Executed as node inside the image. No private state or fingerprints leave it.
import fs from 'node:fs';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
const root='/data/gateway';
const hash=value=>crypto.createHash('sha256').update(value).digest('hex');
const snapshot={};
for(const name of ['identity.key','fee-payer.json','bridge-secret']){
  const file=`${root}/${name}`,metadata=fs.lstatSync(file),bytes=fs.readFileSync(file);
  assert(metadata.isFile()&&!metadata.isSymbolicLink());
  assert.equal(metadata.mode&0o777,0o600);assert.equal(metadata.uid,process.getuid());
  snapshot[name]={hash:hash(bytes),inode:metadata.ino};
  if(name==='identity.key')assert.deepEqual(bytes,Buffer.from(process.env.COVENANT_GATEWAY_IDENTITY_B64,'base64'));
  if(name==='fee-payer.json')assert.deepEqual(bytes,Buffer.from(process.env.COVENANT_GATEWAY_FEE_PAYER_KEY_B64,'base64'));
  if(name==='bridge-secret')assert.equal(bytes.toString(),process.env.COVENANT_GATEWAY_BRIDGE_SECRET);
}
assert.equal(fs.statSync('/data').mode&0o777,0o700);
assert.equal(fs.statSync(root).mode&0o777,0o700);
assert.equal(fs.statSync(`${root}/gateway.json`).mode&0o777,0o600);
const journal=JSON.parse(fs.readFileSync(`${root}/gateway.json`,'utf8'));
assert.equal(journal.version,2);assert.deepEqual(journal.entries,{});
assert.equal(journal.reserve_micro_usdc,Number(process.env.COVENANT_GATEWAY_RESERVE_MICRO_USDC));
// Only the recovery clock is expected to advance between starts.
snapshot.journal=hash(JSON.stringify({version:journal.version,namespace:journal.namespace,reserve:journal.reserve_micro_usdc,entries:journal.entries}));
assert(fs.lstatSync(`${root}/admissions.starting`).isFile());
assert.equal(fs.existsSync('/data/operator'),false);assert.equal(fs.existsSync('/data/provider'),false);
const budgets=fs.readdirSync('/data/budget');assert.equal(budgets.length,1);
assert.deepEqual(fs.readdirSync(`/data/budget/${budgets[0]}`),[]);
const saved='/data/smoke-invariants.json';
if(process.argv[2]==='record'){
  const fd=fs.openSync(saved,'wx',0o600);try{fs.writeFileSync(fd,JSON.stringify(snapshot));fs.fsyncSync(fd);}finally{fs.closeSync(fd);}
}else{
  assert.equal(process.argv[2],'compare');assert.deepEqual(snapshot,JSON.parse(fs.readFileSync(saved,'utf8')));
}
process.stdout.write(JSON.stringify({stable_private_state:true,job_entries:0,operator_started:false,provider_started:false}));
