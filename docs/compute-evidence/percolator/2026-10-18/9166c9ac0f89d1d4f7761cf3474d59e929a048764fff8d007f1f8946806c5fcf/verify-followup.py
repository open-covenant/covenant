#!/usr/bin/env python3
"""Verify the original paid patch and later local delta without running source."""
import argparse,base64,hashlib,json,pathlib,subprocess,tempfile

def read(p):return json.loads(p.read_text())
def require(ok,why):
 if not ok:raise ValueError(why)
def sha(raw):return hashlib.sha256(raw).hexdigest()
def git(repo,*args,input=None):
 return subprocess.check_output(['git','-c','core.hooksPath=/dev/null','-c','core.autocrlf=false','-C',str(repo),*args],input=input,stderr=subprocess.PIPE)
def check_files(repo,tree,expected,copies):
 actual={}
 for line in git(repo,'ls-tree','-r',tree,'--','deploy/kani-check').decode().splitlines():
  mode,kind,oid,name=line.replace('\t',' ',1).split(' ',3)
  require(kind=='blob' and mode in ['100644','100755'],'unexpected component object')
  blob=git(repo,'cat-file','blob',oid)
  require((copies/name).read_bytes()==blob,'source copy differs: '+name)
  actual[name]={'git_mode':mode,'sha256':sha(blob)}
 want={f['path']:{k:f[k] for k in ['git_mode','sha256']} for f in expected}
 require(actual==want,'component file inventory differs')

def main():
 p=argparse.ArgumentParser();p.add_argument('--archive',type=pathlib.Path,required=True);p.add_argument('--checkout',type=pathlib.Path);a=p.parse_args()
 root=a.archive.resolve();job=root/'jobs/kani-runner-paid-authorship';local=root/'local-integration'
 record=read(job/'record.json');mapping=read(job/'source-mapping.json');provenance=read(local/'provenance.json')
 results=[b['value']['agent_task_result'] for b in record['output'] if b.get('type')=='json' and 'agent_task_result' in b.get('value',{})]
 require(len(results)==1,'ambiguous authored result');patch=(job/'authored.patch').read_bytes();delta=(local/'follow-up.patch').read_bytes()
 require(patch==base64.b64decode(results[0]['patch_b64'],validate=True),'patch differs from signed output')
 require(sha(patch)==results[0]['patch_sha256']==provenance['signed_patch_sha256'],'paid patch hash differs')
 require(len(delta)==provenance['followup_patch_bytes'] and sha(delta)==provenance['followup_patch_sha256'],'local delta differs')
 with tempfile.TemporaryDirectory(prefix='verify-runner-followup-') as temp:
  repo=pathlib.Path(temp);git(repo,'init','--quiet');git(repo,'fetch','--no-tags',str(job/'source.bundle'),mapping['signed_repo_commit'])
  require(git(repo,'rev-parse',mapping['signed_repo_commit']+'^{tree}').decode().strip()==mapping['tree_hash'],'snapshot tree differs')
  git(repo,'read-tree',mapping['signed_repo_commit']);git(repo,'apply','--cached','--check','-',input=patch);git(repo,'apply','--cached','-',input=patch)
  authored=git(repo,'write-tree').decode().strip();require(authored==provenance['authored_complete_tree'],'paid tree differs')
  require(git(repo,'rev-parse',authored+':deploy/kani-check').decode().strip()==provenance['authored_runner_tree'],'paid component differs')
  check_files(repo,authored,provenance['paid_authored_files'],job/'authored-source')
  git(repo,'apply','--cached','--check','-',input=delta);git(repo,'apply','--cached','-',input=delta)
  integrated=git(repo,'write-tree').decode().strip();component=git(repo,'rev-parse',integrated+':deploy/kani-check').decode().strip()
  require(component==provenance['integrated_runner_tree'],'integrated component differs')
  check_files(repo,integrated,provenance['local_integrated_files'],local/'integrated-source')
  git(repo,'apply','--cached','--reverse','-',input=delta);require(git(repo,'write-tree').decode().strip()==authored,'reverse delta differs')
 if a.checkout:
  pin=provenance.get('canonical_covenant_commit',provenance['integrated_covenant_commit'])
  expected_tree=provenance['integrated_covenant_complete_tree']
  if provenance.get('canonical_covenant_commit'):
   equivalence=read(root/provenance['source_equivalence'])
   require(equivalence['canonical_merge_commit']==pin,'canonical mapping differs')
   expected_tree=equivalence['canonical_complete_tree']
  require(git(a.checkout,'rev-parse',pin+'^{tree}').decode().strip()==expected_tree,'checkout commit tree differs')
  require(git(a.checkout,'rev-parse',pin+':deploy/kani-check').decode().strip()==component,'checkout component differs')
 print(json.dumps({'paid_patch_matches_signed_output':True,'paid_patch_applies_to_signed_source':True,'integrated_component_tree':component,'independent_checkout_compared':a.checkout is not None,'archived_code_executed':False},indent=2))
if __name__=='__main__':main()
