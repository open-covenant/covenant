#!/usr/bin/env python3
"""Print commands for an archived direct check. Never execute or submit them."""
import argparse,base64,hashlib,json,pathlib,shlex

def require(ok,why):
 if not ok:raise ValueError(why)
def sha(b):return hashlib.sha256(b).hexdigest()
def quote(args):return shlex.join([str(x) for x in args])
def main():
 p=argparse.ArgumentParser();p.add_argument('--job',type=pathlib.Path,required=True);p.add_argument('--cli',required=True);p.add_argument('--source-dir',required=True);a=p.parse_args()
 folder=a.job.resolve();record=json.loads((folder/'record.json').read_text());env=record['envelope']['payload']
 require(env['kind']=='agent_check','native check record required')
 specs=[b['value']['agent_check'] for b in env['input'] if b.get('type')=='json' and 'agent_check' in b.get('value',{})];require(len(specs)==1,'ambiguous check spec');s=specs[0]
 require(s['task_job_id']==env['job_id'] and not s.get('patch_b64'),'only unchanged-source native check supported')
 require(not s.get('hidden') and not s.get('reproduction'),'private inputs cannot be reproduced from this public template')
 acceptance=s['acceptance'];k=acceptance.get('kani');require(k is not None,'typed Kani plan required');require(not acceptance.get('commands'),'arbitrary shell commands are not part of this Kani template')
 require(k['check_price_micro_usdc']==env['price_micro_usdc'],'check price mismatch')
 source=s['repo'];bundle=(folder/'source.bundle').read_bytes()
 if source['source']=='bundle':require(bundle==base64.b64decode(source['bundle_b64'],validate=True),'signed bundle differs')
 elif source['source']=='stored':require(sha(bundle)==source['bundle_sha256'] and len(bundle)==source['bytes'],'stored bundle differs')
 else:raise ValueError('template requires archived source.bundle')
 plan=folder/'kani/plan.json';require(json.loads(plan.read_text())==k['plan'],'readable plan differs from signed order')
 cmd=[a.cli,'--json','check','--repo',a.source_dir,'--commit',source['commit'],'--kani-plan',str(plan),'--check-image',acceptance['image'],'--memory-mib',k['memory_mib'],'--cpus',k['cpus'],'--scratch-mib',k['scratch_mib'],'--check-timeout',acceptance['timeout_secs'],'--deadline-ms',env['deadline_ms'],'--price',env['price_micro_usdc'],'--check-price',k['check_price_micro_usdc']]
 for setup in acceptance.get('setup',[]):cmd+=['--setup',setup]
 print('# Template only: this program has not run any command or contacted Covenant.')
 print('# First verify the archive offline; then reconstruct its public source locally:')
 print(quote(['git','init',a.source_dir]))
 print(quote(['git','-C',a.source_dir,'fetch','--no-tags',str(folder/'source.bundle'),source['commit']]))
 print(quote(['git','-C',a.source_dir,'checkout','--detach',source['commit']]))
 print('# Preview only; spends nothing:')
 print(quote(cmd+['--dry-run']))
 print('# OPT-IN PAID RERUN: checker execution costs '+str(env['price_micro_usdc'])+' microUSDC even if acceptance fails.')
 print('# Replace YOUR_FRESH_RERUN_KEY once; reuse that same key when recovering this intended purchase.')
 print('# '+quote(cmd+['--idempotency-key','YOUR_FRESH_RERUN_KEY']))
if __name__=='__main__':main()
