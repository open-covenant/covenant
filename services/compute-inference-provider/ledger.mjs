import { DatabaseSync } from 'node:sqlite';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

export const ceilingCost=(tokens,price)=>Number((BigInt(tokens)*BigInt(price)+999_999n)/1_000_000n);
export class Refusal extends Error { constructor(code,status=503){super(code);this.code=code;this.status=status;} }
export class Ledger {
 constructor(config){
  this.config=config;fs.mkdirSync(path.dirname(config.dbPath),{recursive:true,mode:0o700});
  if(fs.existsSync(config.dbPath)&&!fs.lstatSync(config.dbPath).isFile())throw new Error('unsafe database file');
  this.db=new DatabaseSync(config.dbPath);fs.chmodSync(config.dbPath,0o600);
  this.db.exec('PRAGMA busy_timeout=5000;');
  // Concurrent fresh connections can deadlock upgrading journal mode; SQLite skips its busy handler then.
  // Retry only this idempotent startup PRAGMA. No reservation, dispatch or monetary transition is retried.
  const deadline=performance.now()+5000,wait=new Int32Array(new SharedArrayBuffer(4));
  for(;;){try{this.db.exec('PRAGMA journal_mode=WAL;');break;}catch(error){
   const remaining=deadline-performance.now();
   if(![5,6].includes(error.errcode&255)||remaining<=0)throw error;
   Atomics.wait(wait,0,0,Math.min(25,remaining));
  }}
  this.db.exec('PRAGMA synchronous=FULL;');
  this.db.exec(`CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
   CREATE TABLE IF NOT EXISTS calls(id TEXT PRIMARY KEY,digest TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('reserved','unknown','complete')),
   reserved INTEGER NOT NULL,accounted INTEGER NOT NULL,created_ms INTEGER NOT NULL,finished_ms INTEGER,input_price INTEGER NOT NULL,output_price INTEGER NOT NULL,
   max_input_tokens INTEGER NOT NULL,max_output_tokens INTEGER NOT NULL,input_tokens INTEGER,output_tokens INTEGER,response TEXT,error_code TEXT);
   CREATE INDEX IF NOT EXISTS calls_state ON calls(state);
   CREATE TABLE IF NOT EXISTS abandoned_losses(id TEXT PRIMARY KEY REFERENCES calls(id),request_digest TEXT NOT NULL,
    accounted INTEGER NOT NULL,audit_reference TEXT NOT NULL,abandoned_ms INTEGER NOT NULL);
   CREATE TABLE IF NOT EXISTS failure_diagnostics(id TEXT PRIMARY KEY REFERENCES calls(id),metadata_json TEXT NOT NULL);
   CREATE TRIGGER IF NOT EXISTS abandoned_losses_no_update BEFORE UPDATE ON abandoned_losses BEGIN SELECT RAISE(ABORT,'loss record is immutable'); END;
   CREATE TRIGGER IF NOT EXISTS abandoned_losses_no_delete BEFORE DELETE ON abandoned_losses BEGIN SELECT RAISE(ABORT,'loss record is immutable'); END;`);
  // The same balance must not acquire a fresh budget by swapping a token in place.
  const identity=crypto.createHash('sha256').update(config.token).digest('hex');
  this.db.exec('BEGIN IMMEDIATE');try{const row=this.db.prepare("SELECT value FROM metadata WHERE key='provider_identity'").get();
   if(row&&row.value!==identity)throw new Error('provider identity changed');
   this.db.prepare("INSERT OR IGNORE INTO metadata VALUES('provider_identity',?)").run(identity);
   this.db.exec('COMMIT');}catch(e){this.db.exec('ROLLBACK');throw e;}
 }
 totals(now=Date.now()){
  const r=this.db.prepare(`SELECT COALESCE(SUM(accounted),0) lifetime,
   COALESCE(SUM(CASE WHEN state!='complete' OR finished_ms>=? THEN accounted ELSE 0 END),0) daily,
   COALESCE(SUM(CASE WHEN state!='complete' AND NOT EXISTS(SELECT 1 FROM abandoned_losses WHERE abandoned_losses.id=calls.id) THEN 1 ELSE 0 END),0) unresolved FROM calls`).get(now-86_400_000);
  return r;
 }
 reserve(id,digest,maxOutputTokens,now=Date.now()){
  const c=this.config,reserved=ceilingCost(c.maxInputTokens,c.inputPrice)+ceilingCost(maxOutputTokens,c.outputPrice);
  this.db.exec('BEGIN IMMEDIATE');try{
   const previous=this.db.prepare('SELECT * FROM calls WHERE id=?').get(id);
   if(previous){if(previous.digest!==digest)throw new Refusal('idempotency_conflict',409);if(this.db.prepare('SELECT 1 FROM abandoned_losses WHERE id=?').get(id))throw new Refusal('provider_request_abandoned',409);if(previous.state!=='complete')throw new Refusal('provider_outcome_unknown');this.db.exec('COMMIT');return {cached:previous.response};}
   const totals=this.totals(now);
   if(totals.unresolved)throw new Refusal('provider_busy_or_unresolved',429);
   if(totals.lifetime+reserved>c.lifetimeBudget||totals.daily+reserved>c.dailyBudget)throw new Refusal('provider_budget_exhausted',429);
   this.db.prepare(`INSERT INTO calls(id,digest,state,reserved,accounted,created_ms,input_price,output_price,max_input_tokens,max_output_tokens)
    VALUES(?,?,'reserved',?,?,?,?,?,?,?)`).run(id,digest,reserved,reserved,now,c.inputPrice,c.outputPrice,c.maxInputTokens,maxOutputTokens);
   this.db.exec('COMMIT');return {reserved};
  }catch(e){this.db.exec('ROLLBACK');throw e;}
 }
 complete(id,{inputTokens,outputTokens,response},now=Date.now()){
  this.db.exec('BEGIN IMMEDIATE');try{
   const row=this.db.prepare('SELECT * FROM calls WHERE id=?').get(id);
   if(!row||row.state!=='reserved')throw new Error('reservation missing');
   if(inputTokens>row.max_input_tokens||outputTokens>row.max_output_tokens)throw new Refusal('provider_usage_exceeds_cap');
   const cost=ceilingCost(inputTokens,row.input_price)+ceilingCost(outputTokens,row.output_price);
   if(cost>row.reserved)throw new Refusal('provider_usage_exceeds_cap');
   this.db.prepare("UPDATE calls SET state='complete',accounted=?,finished_ms=?,input_tokens=?,output_tokens=?,response=? WHERE id=? AND state='reserved'")
    .run(cost,now,inputTokens,outputTokens,response,id);
   this.db.exec('COMMIT');return cost;
  }catch(e){this.db.exec('ROLLBACK');throw e;}
 }
 unknown(id,code,diagnostics){
  this.db.exec('BEGIN IMMEDIATE');try{
   const result=this.db.prepare("UPDATE calls SET state='unknown',error_code=? WHERE id=? AND state='reserved'").run(code,id);
   if(result.changes&&diagnostics)this.db.prepare('INSERT OR IGNORE INTO failure_diagnostics VALUES(?,?)').run(id,JSON.stringify(diagnostics));
   this.db.exec('COMMIT');
  }catch(e){this.db.exec('ROLLBACK');throw e;}
 }
 inspect(id){
  const row=this.db.prepare('SELECT id,digest,state,reserved,accounted,created_ms,finished_ms,error_code FROM calls WHERE id=?').get(id);
  if(!row)throw new Refusal('provider_record_missing',404);
  const loss=this.db.prepare('SELECT audit_reference,abandoned_ms FROM abandoned_losses WHERE id=?').get(id);
  const diagnostics=this.db.prepare('SELECT metadata_json FROM failure_diagnostics WHERE id=?').get(id);
  return {...row,...(loss?{abandoned_loss:loss}:{}),...(diagnostics?{diagnostics:JSON.parse(diagnostics.metadata_json)}:{})};
 }
 abandonUnknown({id,requestDigest,reservedMicroUsdc,auditReference},now=Date.now()){
  if(typeof id!=='string'||!id.length||!/^[a-f0-9]{64}$/.test(requestDigest)||!Number.isSafeInteger(reservedMicroUsdc)||reservedMicroUsdc<1||
    typeof auditReference!=='string'||!/^sha256:[a-f0-9]{64}$/.test(auditReference)||!Number.isSafeInteger(now)||now<0)throw new Refusal('invalid_loss_reconciliation',400);
  this.db.exec('BEGIN IMMEDIATE');try{
   const row=this.db.prepare('SELECT * FROM calls WHERE id=?').get(id);
   if(!row||row.state!=='unknown'||row.digest!==requestDigest||row.reserved!==reservedMicroUsdc||row.accounted!==row.reserved)throw new Refusal('loss_reconciliation_mismatch',409);
   const previous=this.db.prepare('SELECT * FROM abandoned_losses WHERE id=?').get(id);
   if(previous&&previous.audit_reference!==auditReference)throw new Refusal('loss_reconciliation_conflict',409);
   if(!previous)this.db.prepare('INSERT INTO abandoned_losses VALUES(?,?,?,?,?)').run(id,row.digest,row.reserved,auditReference,now);
   // The unknown call is intentionally unchanged: full cost stays in both budgets forever.
   this.db.exec('COMMIT');return {state:'abandoned_full_loss',accounted_micro_usdc:row.reserved,audit_reference:auditReference};
  }catch(e){this.db.exec('ROLLBACK');throw e;}
 }
 close(){this.db.close();}
}
