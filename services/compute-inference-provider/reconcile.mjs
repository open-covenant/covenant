import { configFromEnv } from './config.mjs';
import { Ledger } from './ledger.mjs';

// Local operator command only. It never contacts the provider or submits a request.
process.umask(0o077);
let ledger;
try{
 const args=process.argv.slice(2);
 const config=configFromEnv();if(!config.enabled)throw new Error('disabled');
 ledger=new Ledger(config);
 if(args.length===2&&args[0]==='--inspect')process.stdout.write(`${JSON.stringify(ledger.inspect(args[1]))}\n`);
 else {
  const names=['--abandon-full-reservation','--id','--request-sha256','--reserved-micro-usdc','--audit-sha256'];
  if(args.length!==9||args[0]!==names[0]||names.slice(1).some((name,i)=>args[1+i*2]!==name)||!/^\d+$/.test(args[6])||!/^[a-f0-9]{64}$/.test(args[8]))throw new Error('invalid arguments');
  const result=ledger.abandonUnknown({id:args[2],requestDigest:args[4],reservedMicroUsdc:Number(args[6]),auditReference:`sha256:${args[8]}`});
  process.stdout.write(`${JSON.stringify(result)}\n`);
 }
}catch{process.stderr.write('Provider reconciliation refused: verify local policy, exact unknown record, full reservation and audit SHA-256.\n');process.exitCode=1;}
finally{ledger?.close();}
