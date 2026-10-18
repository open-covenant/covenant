"""One explicitly reviewed public hidden contract; all other overlays are refused.

This module checks disclosure scope and byte commitments, not signatures. The
Rust full-chain verifier authenticates every envelope/receipt and parent binding.
"""
import base64,hashlib,json
POLICY={
 'policy_id':'percolator-pr239-buyer-acceptance-v1',
 'original_commit':'81fabc86869d0a4db7da3fabc7eab5430b1f02c5',
 'path':'.covenant-pr239/acceptance.py',
 'bytes':6998,
 'content_sha256':'1b67e2b98d9286c697066994cb72563b62a54b77eecf3ff16ed51122bb572c18',
 'command':'python3 .covenant-pr239/acceptance.py',
 'hidden_bundle_sha256':'8558d8bb1f3725cee29fc71757797199882d6c577523f430c24f15f3830e07cb',
}
def require(ok,why):
 if not ok:raise ValueError(why)
def sha(raw):return hashlib.sha256(raw).hexdigest()
def policy_ok(policy):
 require(policy==POLICY,'Unknown or altered hidden disclosure allowlist')
def spec(envelope):
 p=envelope['payload'];kind=p['kind'];require(kind in ['agent_task','agent_check'],'Unsupported hidden disclosure job kind')
 matches=[b['value'][kind] for b in p['input'] if b.get('type')=='json' and isinstance(b.get('value'),dict) and kind in b['value']]
 require(len(matches)==1,'Expected one signed task/check input');return kind,matches[0]
def validate_spec(kind,s,policy=None):
 require(s.get('reproduction') is None,'Reproduction overlays are not authorized for public disclosure')
 commitment=s.get('acceptance',{}).get('hidden_sha256');hidden=s.get('hidden')
 if policy is None:
  require(commitment is None and hidden is None,'Private hidden inputs or source overlays are not publishable without the exact per-archive allowlist')
  return None
 policy_ok(policy)
 require(s.get('acceptance',{}).get('skill','code.change')=='code.change','Only the reviewed code.change buyer contract may be disclosed')
 require(s.get('acceptance',{}).get('kani') is None,'Hidden command disclosure is for the authorship contract, not native Kani checks')
 require(commitment==POLICY['hidden_bundle_sha256'],'Signed hidden commitment differs from the reviewed bundle')
 if kind=='agent_task':
  require(hidden is None,'Parent task unexpectedly embeds hidden files');return None
 require(kind=='agent_check' and isinstance(hidden,dict),'Signed checker hidden bundle is missing')
 require(set(hidden)=={'files','commands'},'Unexpected hidden bundle fields')
 require(hidden['commands']==[POLICY['command']],'Unknown or additional hidden command')
 require(isinstance(hidden['files'],list) and len(hidden['files'])==1,'Hidden bundle must contain exactly the reviewed file')
 file=hidden['files'][0];require(isinstance(file,dict) and set(file)=={'path','content_b64'},'Unexpected hidden file fields')
 require(file['path']==POLICY['path'],'Unknown hidden file path')
 raw=base64.b64decode(file['content_b64'],validate=True)
 require(base64.b64encode(raw).decode()==file['content_b64'],'Noncanonical hidden file encoding')
 require(len(raw)==POLICY['bytes'] and sha(raw)==POLICY['content_sha256'],'Hidden file bytes differ from reviewed content')
 # Match Rust HiddenChecks::digest: declaration-order serde JSON, no spaces.
 canonical={'files':[{'path':file['path'],'content_b64':file['content_b64']}],'commands':hidden['commands']}
 digest=sha(json.dumps(canonical,separators=(',',':'),ensure_ascii=False).encode())
 require(digest==commitment==POLICY['hidden_bundle_sha256'],'Signed hidden bundle digest mismatch')
 return raw

def validate_record(record,original_commit):
 policy=record.get('hidden_disclosure');kind,parent=spec(record['envelope']);validate_spec(kind,parent,policy)
 evidence=record.get('agent_evidence')
 if policy is not None:
  policy_ok(policy)
  require(kind=='agent_task' and record['expected']['role']=='authorship','Hidden disclosure requires a full authorship chain')
  require(original_commit==POLICY['original_commit'],'Disclosure applies only to the reviewed original source commit')
  require(isinstance(evidence,dict) and bool(evidence.get('checks')),'All linked signed checker evidence is required for hidden disclosure')
 revealed=None
 if evidence:
  parent_kind,parent_spec=spec(evidence['task']['envelope']);require(parent_kind=='agent_task','Invalid evidence parent');validate_spec(parent_kind,parent_spec,policy)
  for child in evidence['checks']:
   child_kind,child_spec=spec(child['envelope']);require(child_kind=='agent_check','Non-check child in disclosure inventory');raw=validate_spec(child_kind,child_spec,policy)
   if raw is not None:
    if revealed is not None:require(raw==revealed,'Child hidden contracts differ')
    revealed=raw
 if policy is not None:require(revealed is not None,'Reviewed contract bytes missing from signed child evidence')
 return revealed
