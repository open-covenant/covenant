#!/usr/bin/env python3
"""Check a public evidence directory offline, without buyer keys or API access."""
import argparse,base64,hashlib,json,pathlib,re,subprocess,tempfile
def read(p):return json.loads(p.read_text())
def require(ok,why):
 if not ok:raise ValueError(why)
def sha(b):return hashlib.sha256(b).hexdigest()
def safe_path(name):
 p=pathlib.PurePosixPath(name);require(not p.is_absolute() and '..' not in p.parts and bool(p.parts),'unsafe archive path');return p
def git(repo,*args):return subprocess.check_output(['git','-c','core.hooksPath=/dev/null','-C',str(repo),*args],stderr=subprocess.PIPE)
def check(folder,verifier):
 record=read(folder/'record.json');mapping=read(folder/'source-mapping.json');env=record['envelope']
 raw=(folder/'original-commit.raw').read_bytes();oid=hashlib.sha1(b'commit '+str(len(raw)).encode()+b'\0'+raw).hexdigest()
 require(oid==mapping['original_commit'],'original commit bytes differ')
 tree=raw.split(b'\n',1)[0].removeprefix(b'tree ').decode()
 require(tree==mapping['tree_hash']==record['expected']['tree_hash'],'original commit tree mismatch')
 spec=[c['value'][env['payload']['kind']] for c in env['payload']['input'] if c.get('type')=='json' and env['payload']['kind'] in c.get('value',{})]
 require(len(spec)==1,'duplicate signed source');source=spec[0]['repo']
 require(re.fullmatch('[0-9a-f]{40}',source['commit']) is not None,'invalid signed source commit')
 require(source['commit']==mapping['signed_repo_commit']==record['expected']['signed_repo_commit'],'signed source commit mismatch')
 bundle=folder/'source.bundle'
 if source['source'] in ['bundle','stored']:
  b=bundle.read_bytes()
  if source['source']=='bundle':require(b==base64.b64decode(source['bundle_b64'],validate=True),'source bundle bytes differ from envelope')
  else:require(len(b)==source['bytes'] and sha(b)==source['bundle_sha256'],'stored source digest differs')
  require(sha(b)==mapping['bundle_sha256'],'source mapping bundle digest mismatch')
  with tempfile.TemporaryDirectory(prefix='verify-public-source-') as temp:
   repo=pathlib.Path(temp)/'repo.git';subprocess.run(['git','init','--bare','--quiet',str(repo)],check=True)
   git(repo,'fetch','--no-tags',str(bundle.resolve()),source['commit'])
   require(git(repo,'rev-parse',source['commit']+'^{tree}').decode().strip()==tree,'signed snapshot has another tree')
   require(git(repo,'rev-list','--count',source['commit']).strip()==b'1','signed snapshot carries history')
   for f in mapping['source_files']:
    safe_path(f['path'])
    blob=git(repo,'show',source['commit']+':'+f['path']);require(blob==(folder/'source'/f['path']).read_bytes() and sha(blob)==f['sha256'],'copied source file differs')
 elif source['source']=='git':
  require(source['commit']==oid and source['url']==mapping['public_repo_url'],'remote source binding differs')
 else:raise ValueError('unsupported source mode')
 verified=subprocess.run([str(verifier),str(folder/'record.json')],check=True,capture_output=True,text=True)
 result=json.loads(verified.stdout)
 require(result==read(folder/'verification.json'),'published verifier summary differs from recomputation')
 # Recheck readable artifact files against the exact signed compressed package.
 import gzip,io
 outputs=[('',record['output'])]
 if record.get('agent_evidence'):
  outputs += [('checks/'+c['envelope']['payload']['job_id']+'/',c.get('output') or []) for c in record['agent_evidence']['checks']]
 for prefix,output in outputs:
  for block in output:
   e=block.get('value',{}).get('kani_execution') if block.get('type')=='json' else None
   if not e:continue
   compressed=base64.b64decode(e['artifacts_gzip_b64'],validate=True)
   require(len(compressed)<=4*1024*1024 and sha(compressed)==e['artifacts_sha256'],'compressed artifact digest differs')
   with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as f:packed=f.read(24*1024*1024+1)
   require(len(packed)<=24*1024*1024,'artifact decompression limit')
   for name,encoded in json.loads(packed).items():
    require(name in ['result.json','stdout.log','stderr.log','kani-export.json'],'unexpected artifact name')
    require(base64.b64decode(encoded,validate=True)==(folder/prefix/'kani'/name).read_bytes(),'readable raw artifact differs from signed output')
 return result
def main():
 p=argparse.ArgumentParser();p.add_argument('--archive',required=True,type=pathlib.Path);p.add_argument('--verifier',required=True,type=pathlib.Path);a=p.parse_args()
 require(not (a.archive/'INCOMPLETE-DO-NOT-PUBLISH.json').exists(),'incomplete archive')
 for name,expected in read(a.archive/'inventory.json').items():
  safe_path(name)
  data=(a.archive/name).read_bytes();require(len(data)==expected['bytes'] and sha(data)==expected['sha256'],'file inventory mismatch: '+name)
 results=[check(folder,a.verifier.resolve()) for folder in sorted((a.archive/'jobs').iterdir()) if folder.is_dir()]
 print(json.dumps({'offline_verified':True,'jobs':results,'settlement_verified':False,'inventory_completeness_verified':False},indent=2))
if __name__=='__main__':main()
