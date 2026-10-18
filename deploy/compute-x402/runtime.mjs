import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {spawn} from 'node:child_process';

function syncDirectory(file){const fd=fs.openSync(path.dirname(file),'r');try{fs.fsyncSync(fd);}finally{fs.closeSync(fd);}}
export function privateDirectory(directory){
  fs.mkdirSync(directory,{recursive:true,mode:0o700});
  if(!fs.lstatSync(directory).isDirectory())throw Error('Unsafe persistent directory');
  fs.chmodSync(directory,0o700);
}
export function seed(file,encoded,length){
  let bytes;
  if(encoded!==undefined){
    bytes=Buffer.from(encoded,'base64');
    if(!bytes.length||bytes.toString('base64')!==encoded||(length!==undefined&&bytes.length!==length))throw Error('Invalid identity seed encoding or length');
  }
  try{
    if(!fs.lstatSync(file).isFile())throw Error('Unsafe persistent identity');
    const actual=fs.readFileSync(file);
    if((length!==undefined&&actual.length!==length)||(bytes&&(actual.length!==bytes.length||!crypto.timingSafeEqual(actual,bytes))))throw Error('Persistent identity mismatch');
    fs.chmodSync(file,0o600);return;
  }catch(error){if(error.code!=='ENOENT')throw error;}
  if(!bytes)throw Error('A new volume requires explicit identity seeds');
  const fd=fs.openSync(file,'wx',0o600);
  try{fs.writeFileSync(fd,bytes);fs.fsyncSync(fd);}finally{fs.closeSync(fd);}
  syncDirectory(file);
}
export function latch(file,reason){
  try{const fd=fs.openSync(file,'wx',0o600);try{fs.writeFileSync(fd,`${reason}\n`);fs.fsyncSync(fd);}finally{fs.closeSync(fd);}syncDirectory(file);}
  catch(error){if(error.code!=='EEXIST')throw error;/* Any existing path already denies admission. */}
}
export function missing(file){try{fs.lstatSync(file);return false;}catch(error){return error.code==='ENOENT';}}
export function clearStartingLatch(file){fs.unlinkSync(file);syncDirectory(file);}
export function explicitBoolean(env,name){if(!['true','false'].includes(env[name]))throw Error(`Explicit ${name} required`);return env[name]==='true';}
export function startInferenceOperator(group,env){
  // The node's serving mode is the default command; `serve` is not a subcommand.
  return group.start('inference operator','covenant-compute-node',[],env,true);
}
export function operatorPubkey(seedBytes){
  const key=crypto.createPrivateKey({key:Buffer.concat([Buffer.from('302e020100300506032b657004220420','hex'),seedBytes]),format:'der',type:'pkcs8'});
  const bytes=crypto.createPublicKey(key).export({format:'der',type:'spki'}).subarray(-32);
  const alphabet='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  let n=BigInt(`0x${bytes.toString('hex')}`),encoded='';while(n){encoded=alphabet[Number(n%58n)]+encoded;n/=58n;}
  for(const byte of bytes){if(byte!==0)break;encoded='1'+encoded;}return encoded;
}
export class ProcessGroup{
  constructor({onOptionalFailure,onBeforeStop=()=>{},onExit=code=>process.exit(code),spawnChild=spawn,shutdownMs=10_000,cwd='/opt/covenant'}){
    Object.assign(this,{onOptionalFailure,onBeforeStop,onExit,spawnChild,shutdownMs,cwd});this.children=[];this.stopping=false;this.optionalFailed=false;
  }
  start(name,command,args,env,optional=false){
    if(this.stopping||optional&&this.optionalFailed)throw Error('Startup cancelled');
    const child=this.spawnChild(command,args,{cwd:this.cwd,stdio:'inherit',env});
    const record={child,name,optional,done:false};this.children.push(record);
    const ended=()=>{if(record.done)return;record.done=true;if(this.stopping)return;
      process.stderr.write(`${name} stopped\n`);
      if(optional)this.failOptional(`${name} stopped`);else this.stop(1);
    };
    child.once('error',ended);child.once('exit',ended);return child;
  }
  failOptional(reason){
    if(this.optionalFailed||this.stopping)return;this.optionalFailed=true;
    try{this.onOptionalFailure(reason);}catch{this.stop(1);return;}
    // Persist the admission refusal before stopping any optional worker.
    for(const {child,optional,done} of this.children)if(optional&&!done)child.kill('SIGTERM');
    const timeout=setTimeout(()=>{for(const {child,optional,done} of this.children)if(optional&&!done)child.kill('SIGKILL');},this.shutdownMs);
    timeout.unref();
  }
  stop(code){
    if(this.stopping)return;this.stopping=true;
    try{this.onBeforeStop();}catch{/* All processes are still terminated if the disk failed. */}
    const pending=this.children.filter(record=>!record.done);
    const waits=pending.map(({child})=>new Promise(resolve=>{child.once('exit',resolve);child.once('error',resolve);}));
    for(const {child} of pending)child.kill('SIGTERM');
    const timeout=setTimeout(()=>{for(const {child,done} of pending)if(!done)child.kill('SIGKILL');this.onExit(code);},this.shutdownMs);
    Promise.all(waits).then(()=>{clearTimeout(timeout);this.onExit(code);});
  }
}
export async function waitFor(url,{headers={},predicate=()=>true,group,attempts=60,fetcher=fetch,intervalMs=500}={}){
  for(let i=0;i<attempts;i++){
    if(group?.stopping)throw Error('Startup cancelled');
    try{const r=await fetcher(url,{headers,signal:AbortSignal.timeout(1000),redirect:'error'});if(r.ok&&predicate(await r.json()))return;}catch{}
    await new Promise(resolve=>setTimeout(resolve,intervalMs));
  }
  throw Error('Service did not become ready');
}
