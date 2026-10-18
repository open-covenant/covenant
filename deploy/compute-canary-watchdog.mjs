// External, read-only coordinator observer. This process has no rental/payment API.
import fs from 'node:fs';
import path from 'node:path';
import https from 'node:https';
import crypto from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';

export const ENDPOINT='https://compute-api.opencovenant.org/rental-checks';
export const OUTAGE_SECONDS=3*6*60*60;
export const PROBE_SECONDS=60;
const MAX_BODY=256*1024, MAX_STATE=128*1024, RECIPIENT='alert@opencovenant.org';
const ALERT_KEYS=['COVENANT_CANARY_ALERT_CLOUDFLARE_API_TOKEN','COVENANT_CANARY_ALERT_CLOUDFLARE_ACCOUNT_ID','COVENANT_CANARY_ALERT_FROM','COVENANT_CANARY_ALERT_TO'];
const object=v=>v!==null&&typeof v==='object'&&!Array.isArray(v);
const natural=v=>Number.isSafeInteger(v)&&v>=0;
const nullableTime=v=>v===null||natural(v);
const uuid=v=>typeof v==='string'&&/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(v);
function requireValue(ok){if(!ok)throw Error('Watchdog state or configuration invalid');}

export function alertConfig(env){
  const token=env[ALERT_KEYS[0]], account=env[ALERT_KEYS[1]], from=env[ALERT_KEYS[2]], to=env[ALERT_KEYS[3]];
  requireValue(typeof token==='string'&&token.length>0&&token.length<=4096&&/^[\x21-\x7e]+$/.test(token));
  requireValue(typeof account==='string'&&/^[0-9a-fA-F]{32}$/.test(account));
  requireValue(typeof from==='string'&&from.length<=254&&/^[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?$/.test(from));
  requireValue(to===RECIPIENT);
  return {token,account,from,to};
}

// Readiness is independent of scheduling: a recent budget-pause heartbeat is
// visible, whereas an old disabled snapshot or a configuration skeleton is not.
export function freshFeed(v, now){
  if(!natural(now)||!object(v)||v.schema!==1||v.cadence_seconds!==21600||v.stale_after_seconds!==180
    ||typeof v.enabled!=='boolean'||!['fresh','disabled'].includes(v.feed_status)||v.is_stale!==false
    ||!natural(v.server_time_unix)||Math.abs(v.server_time_unix-now)>120
    ||!natural(v.generated_at_unix)||v.generated_at_unix===0||v.generated_at_unix>now+30
    ||v.generated_at_unix>v.server_time_unix+30||now-v.generated_at_unix>180
    ||v.observed_state_age_seconds!==Math.max(0,v.server_time_unix-v.generated_at_unix)
    ||v.observed_state_age_seconds>180||!Array.isArray(v.history)||v.history.length>40
    ||!(v.latest===null||object(v.latest))||!(v.monitor===null||object(v.monitor))
    ||!(v.budget===null||object(v.budget))||!nullableTime(v.next_due_at_unix)
    ||!(v.pause_reason===null||typeof v.pause_reason==='string'&&/^[a-z_]{1,80}$/.test(v.pause_reason)))return false;
  return v.enabled?v.feed_status==='fresh':v.feed_status==='disabled';
}

export function initialState(now){
  requireValue(natural(now));
  return {schema:1,endpoint:ENDPOINT,created_at_unix:now,last_probe_at_unix:null,last_fresh_at_unix:null,
    first_missing_at_unix:null,incident_id:null,incident_alert_created:false,outbox:[],delivered_count:0};
}
export function validateState(v){
  requireValue(object(v)&&v.schema===1&&v.endpoint===ENDPOINT&&natural(v.created_at_unix)
    &&nullableTime(v.last_probe_at_unix)&&nullableTime(v.last_fresh_at_unix)&&nullableTime(v.first_missing_at_unix)
    &&(v.incident_id===null||uuid(v.incident_id))&&typeof v.incident_alert_created==='boolean'
    &&natural(v.delivered_count)&&Array.isArray(v.outbox)&&v.outbox.length<=32);
  requireValue((v.first_missing_at_unix===null)===(v.incident_id===null));
  requireValue(!v.incident_alert_created||v.incident_id!==null);
  const ids=new Set();
  for(const e of v.outbox){
    requireValue(object(e)&&uuid(e.id)&&uuid(e.incident_id)&&!ids.has(e.id)
      &&['missing','restored'].includes(e.kind)&&natural(e.observed_at_unix)&&natural(e.first_missing_at_unix)
      &&e.observed_at_unix>=e.first_missing_at_unix);
    ids.add(e.id);
  }
  return v;
}
export function observe(previous, fresh, now, makeId=crypto.randomUUID){
  validateState(previous);requireValue(typeof fresh==='boolean'&&natural(now));
  // A clock rollback cannot create a premature notification or erase an outage.
  requireValue(now>=previous.created_at_unix&&(previous.last_probe_at_unix===null||now>=previous.last_probe_at_unix));
  const state=structuredClone(previous);state.last_probe_at_unix=now;
  const enqueue=kind=>{
    requireValue(state.outbox.length<32);
    state.outbox.push({id:makeId(),incident_id:state.incident_id,kind,observed_at_unix:now,first_missing_at_unix:state.first_missing_at_unix});
  };
  if(fresh){
    state.last_fresh_at_unix=now;
    if(state.incident_alert_created)enqueue('restored');
    state.first_missing_at_unix=null;state.incident_id=null;state.incident_alert_created=false;
  }else{
    if(state.first_missing_at_unix===null){state.first_missing_at_unix=now;state.incident_id=makeId();}
    if(!state.incident_alert_created&&now-state.first_missing_at_unix>=OUTAGE_SECONDS){enqueue('missing');state.incident_alert_created=true;}
  }
  return validateState(state);
}

export class Journal{
  constructor(directory){
    requireValue(typeof directory==='string'&&path.isAbsolute(directory));
    fs.mkdirSync(directory,{recursive:true,mode:0o700});
    requireValue(fs.lstatSync(directory).isDirectory());fs.chmodSync(directory,0o700);
    this.directory=directory;this.file=path.join(directory,'observer.json');
  }
  read(now){
    let fd;
    try{
      fd=fs.openSync(this.file,fs.constants.O_RDONLY|fs.constants.O_NOFOLLOW);
      const stat=fs.fstatSync(fd);requireValue(stat.isFile()&&stat.size<=MAX_STATE&&(stat.mode&0o077)===0);
      return validateState(JSON.parse(fs.readFileSync(fd,'utf8')));
    }catch(error){if(error.code==='ENOENT')return initialState(now);throw Error('Watchdog journal unavailable');}
    finally{if(fd!==undefined)fs.closeSync(fd);}
  }
  write(state){
    validateState(state);const bytes=JSON.stringify(state)+'\n';requireValue(Buffer.byteLength(bytes)<=MAX_STATE);
    const temporary=path.join(this.directory,`.observer-${crypto.randomUUID()}.tmp`);
    let fd;
    try{
      fd=fs.openSync(temporary,'wx',0o600);fs.writeFileSync(fd,bytes);fs.fsyncSync(fd);fs.closeSync(fd);fd=undefined;
      fs.renameSync(temporary,this.file);
      const dir=fs.openSync(this.directory,'r');try{fs.fsyncSync(dir);}finally{fs.closeSync(dir);}
    }finally{if(fd!==undefined)fs.closeSync(fd);try{fs.unlinkSync(temporary);}catch(error){if(error.code!=='ENOENT')throw error;}}
  }
}

// The only permitted network effects are the fixed public GET and fixed email
// POST. Node HTTPS ignores proxy environment variables and never follows redirects.
export async function boundedJson(url,{method='GET',headers={},body,limit=MAX_BODY,timeoutMs=10_000,request=https.request}={}){
  const target=new URL(url);
  requireValue((url===ENDPOINT&&method==='GET'&&body===undefined)
    ||(target.origin==='https://api.cloudflare.com'&&/^\/client\/v4\/accounts\/[0-9a-fA-F]{32}\/email\/sending\/send$/.test(target.pathname)
      &&!target.search&&!target.hash&&!target.username&&!target.password&&method==='POST'&&typeof body==='string'));
  return new Promise((resolve,reject)=>{
    let done=false,req,response;
    const finish=(error,value)=>{if(done)return;done=true;clearTimeout(timer);if(error){response?.destroy();req?.destroy();reject(Error('Watchdog request unavailable'));}else resolve(value);};
    const timer=setTimeout(()=>finish(true),timeoutMs);
    try{
      req=request(target,{method,headers:{accept:'application/json','accept-encoding':'identity','user-agent':'Covenant-Rental-Watchdog/1.0','cache-control':'no-cache',...headers}},res=>{
        response=res;
        if((method==='GET'?res.statusCode!==200:res.statusCode<200||res.statusCode>=300)||!/^application\/json(?:\s*;|$)/i.test(res.headers['content-type']??'')
          ||res.headers['content-encoding']&&res.headers['content-encoding']!=='identity')return finish(true);
        const length=res.headers['content-length'];
        if(length!==undefined&&(!/^\d+$/.test(length)||Number(length)>limit))return finish(true);
        const chunks=[];let size=0;
        res.on('data',chunk=>{size+=chunk.length;if(size>limit)return finish(true);chunks.push(chunk);});
        res.on('error',()=>finish(true));res.on('aborted',()=>finish(true));
        res.on('end',()=>{
          if(length!==undefined&&size!==Number(length))return finish(true);
          try{const value=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(Buffer.concat(chunks)));finish(false,value);}catch{finish(true);}
        });
      });
      req.on('error',()=>finish(true));req.end(body);
    }catch{finish(true);}
  });
}
export function cloudflareAccepted(value){
  const r=value?.result;
  return object(value)&&value.success===true&&Array.isArray(value.errors)&&value.errors.length===0&&object(r)
    &&['delivered','queued','permanent_bounces'].every(k=>Array.isArray(r[k])&&r[k].every(v=>typeof v==='string'))
    &&(!('suppressed_recipients' in r)||Array.isArray(r.suppressed_recipients)&&r.suppressed_recipients.every(v=>typeof v==='string'))
    &&!r.permanent_bounces.includes(RECIPIENT)&&!(r.suppressed_recipients??[]).includes(RECIPIENT)
    &&(r.delivered.includes(RECIPIENT)||r.queued.includes(RECIPIENT));
}
export function emailFor(event,config){
  requireValue(uuid(event.id)&&['missing','restored'].includes(event.kind)&&natural(event.observed_at_unix)&&natural(event.first_missing_at_unix));
  const subject=event.kind==='missing'?'Covenant Compute: rental-check feed unavailable':'Covenant Compute: rental-check reporting restored';
  const description=event.kind==='missing'
    ?'The external observer recorded at least 18 hours without a fresh rental-check heartbeat. The coordinator may be unavailable or its reporting may have stopped. This does not establish lost funds or a network-wide GPU outage.'
    :'The external observer received a fresh rental-check heartbeat again. Reporting is restored; this does not mean a GPU rental check passed. A current intentional pause also counts as visible reporting.';
  return {from:config.from,to:config.to,subject,text:subject+'\n\n'+description+'\n\n'
    +'Recorded observation: '+new Date(event.observed_at_unix*1000).toISOString()+'\n'
    +'First missing heartbeat: '+new Date(event.first_missing_at_unix*1000).toISOString()+'\n'
    +'Event ID: '+event.id+'\n\nhttps://opencovenant.org/compute-rental-checks\n',headers:{'X-Covenant-Event-ID':event.id}};
}
export async function deliver(event,config,transport=boundedJson){
  const value=await transport(`https://api.cloudflare.com/client/v4/accounts/${config.account}/email/sending/send`,{
    method:'POST',headers:{authorization:`Bearer ${config.token}`,'content-type':'application/json','idempotency-key':event.id},
    body:JSON.stringify(emailFor(event,config)),limit:16*1024,timeoutMs:5000});
  requireValue(cloudflareAccepted(value));
}
export async function tick({journal,config,now=Math.floor(Date.now()/1000),transport=boundedJson,send=deliver}){
  const previous=journal.read(now);let fresh=false;
  try{fresh=freshFeed(await transport(ENDPOINT),now);}catch{/* A failed read is missing visibility. */}
  const state=observe(previous,fresh,now);
  journal.write(state); // Commit observation and exact outbox event before sending.
  let delivery='none';
  if(state.outbox.length){
    let accepted=false;
    try{await send(state.outbox[0],config,transport);accepted=true;}catch{delivery='pending';}
    // Unknown delivery reuses the exact durable event ID; a failed post-send
    // disk write must also leave the durable outbox intact for reconciliation.
    if(accepted){state.outbox.shift();state.delivered_count++;journal.write(state);delivery='accepted';}
  }
  return {schema:1,observed_at_unix:now,fresh,first_missing_at_unix:state.first_missing_at_unix,pending_alerts:state.outbox.length,delivery};
}

export function isolatedEnvironment(env,data){
  requireValue(typeof data==='string'&&path.isAbsolute(data));alertConfig(env);
  return Object.fromEntries([['COVENANT_CANARY_WATCHDOG_HOME',path.join(data,'rental-watchdog')],...ALERT_KEYS.map(k=>[k,env[k]])]);
}
// Isolated from the financial ProcessGroup: a watcher failure cannot kill
// settlement workers, lock admissions or restart a paid provider qualification.
export function startWatchdog(env,data,{spawnChild=spawn,setTimer=setTimeout,clearTimer=clearTimeout,log=()=>process.stderr.write('Rental-check observer restarting\n')}={}){
  const enabled=env.COVENANT_CANARY_WATCHDOG_ENABLED;
  if(enabled===undefined||enabled==='false')return ()=>{};
  if(enabled!=='true')throw Error('Explicit watchdog switch required');
  const childEnv=isolatedEnvironment(env,data),directory=childEnv.COVENANT_CANARY_WATCHDOG_HOME;
  new Journal(directory);
  let child,timer,stopping=false;
  const launch=()=>{
    if(stopping)return;
    // util-linux is already in the image. flock releases its process-wide lock
    // on crash and prevents two supervisors from writing one observer journal.
    child=spawnChild('/usr/bin/flock',['--no-fork','-n',path.join(directory,'observer.lock'),process.execPath,fileURLToPath(import.meta.url),'--run'],{env:childEnv,stdio:'inherit'});
    let ended=false;
    const restart=()=>{if(ended)return;ended=true;if(stopping)return;log();timer=setTimer(launch,30_000);};
    child.once('exit',restart);child.once('error',restart);
  };
  launch();
  return ()=>{stopping=true;clearTimer(timer);child?.kill('SIGTERM');};
}
async function main(){
  process.umask(0o077);requireValue(process.argv.length===3&&process.argv[2]==='--run');
  const config=alertConfig(process.env),journal=new Journal(process.env.COVENANT_CANARY_WATCHDOG_HOME);
  let stopping=false;for(const signal of ['SIGINT','SIGTERM'])process.on(signal,()=>{stopping=true;});
  while(!stopping){
    try{await tick({journal,config});}catch{process.stderr.write('Rental-check observer unavailable\n');}
    if(!stopping)await new Promise(resolve=>{
      const finish=()=>{clearTimeout(timer);process.removeListener('SIGTERM',finish);process.removeListener('SIGINT',finish);resolve();};
      const timer=setTimeout(finish,PROBE_SECONDS*1000);process.once('SIGTERM',finish);process.once('SIGINT',finish);
    });
  }
}
if(process.argv[1]&&path.resolve(process.argv[1])===fileURLToPath(import.meta.url))main().catch(()=>{process.stderr.write('Rental-check observer refused startup\n');process.exitCode=1;});
