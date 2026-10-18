"""Compare accepted Node signing semantics and shared SQLite recovery."""
import asyncio
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

import pytest
from solders.keypair import Keypair

from covenant_compute_x402 import CovenantX402Client, CovenantError
from covenant_compute_x402.journal import Journal
from covenant_compute_x402.proof import stable, strict_json, sha256


JS = Path(os.environ.get("COVENANT_JS_CLIENT_PATH", Path(__file__).parents[2] / "compute-x402-client"))
NODE = os.environ.get("NODE_BIN") or shutil.which("node") or "/Users/dennisgoslar/.local/opt/node24/bin/node"
pytestmark = pytest.mark.skipif(not Path(NODE).exists() or not (JS / "journal.mjs").exists(), reason="Node24 and accepted JS client required for cross-language tests")


def node(code, *args, input=None):
    return subprocess.run([NODE, "--input-type=module", "-e", code, *map(str, args)], input=input, text=True, capture_output=True, check=True).stdout.strip()


def test_canonical_request_digest_and_number_vectors():
    sources = [
        '{"temperature":1.0,"messages":[{"role":"user","content":"日本語 🟨 \\n"}],"model":"covenant","max_tokens":256}',
        '{"z":-0.0,"a":[1e-7,1e-6,1e20,1e21,333333333.33333329,4.50,2e-3,1e30]}',
        '{"\\u20ac":"euro","\\r":"cr","\\ufb33":"hebrew","1":"one","\\ud83d\\ude00":"emoji","\\u0080":"ctrl","\\u00f6":"o"}',
        '{"x":9007199254740993,"y":-9007199254740993,"z":1.2345678901234567}',
    ]
    output = json.loads(node(f"import{{stable,strictJson,sha256}}from {json.dumps((JS/'proof.mjs').as_uri())};let raw='';for await(const c of process.stdin)raw+=c;console.log(JSON.stringify(JSON.parse(raw).map(s=>{{const c=stable(strictJson(s));return[c,sha256(c)]}})));", input=json.dumps(sources)))
    assert [[stable(strict_json(s)), sha256(stable(strict_json(s)))] for s in sources] == output


def test_node_python_concurrent_budget_one_winner(tmp_path):
    file = tmp_path / "shared.sqlite"
    Journal(file, "shared", 1000).close()
    js = f"import{{Journal}}from {json.dumps((JS/'journal.mjs').as_uri())};const j=new Journal(process.argv[1],'shared',1000);try{{let r=j.claim('node','body','digest');j.reserve('node',r.owner,{{}},1000);console.log('reserved')}}catch(e){{if(e.message!=='cumulative_spend_cap_exceeded')throw e;console.log('refused')}}finally{{j.close()}}"
    py = "from covenant_compute_x402.journal import Journal\nfrom covenant_compute_x402 import CovenantError\nimport sys\nj=Journal(sys.argv[1],'shared',1000)\ntry:\n r=j.claim('python','body','digest');j.reserve('python',r['owner'],{},1000);print('reserved')\nexcept CovenantError as e:\n assert str(e)=='cumulative_spend_cap_exceeded';print('refused')\nfinally:j.close()"
    env = {**os.environ, "PYTHONPATH": os.pathsep.join(sys.path)}
    processes = [subprocess.Popen([NODE, "--input-type=module", "-e", js, str(file)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True), subprocess.Popen([sys.executable, "-c", py, str(file)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)]
    outputs = []
    for process in processes:
        stdout, stderr = process.communicate(timeout=15)
        assert process.returncode == 0, stderr
        outputs.append(stdout.strip())
    assert sorted(outputs) == ["refused", "reserved"]


@pytest.mark.parametrize("pending,legacy", [(True, False), (False, False), (True, True)])
async def test_python_resumes_real_node_journal_without_resigning(pending, legacy):
    code = f"""
      import{{fixture,input}}from {json.dumps((JS/'test/fixtures/mock-gateway.mjs').as_uri())};
      import{{CovenantX402Client}}from {json.dumps((JS/'index.mjs').as_uri())};
      import{{x402Client,x402HTTPClient}}from '@x402/core/client';
      import{{ExactSvmScheme}}from '@x402/svm/exact/client';
      import{{getTransactionEncoder}}from '@solana/kit';
      import{{VersionedTransaction}}from '@solana/web3.js';
      const f=await fixture({{pendingFirst:{str(pending).lower()},expectedUnits:{20000 if legacy else 60000}}});const c=new CovenantX402Client(f.config),r=input();
      if({str(legacy).lower()}){{
        const signer={{address:f.walletProvider.getAddress(),signTransactions:async txs=>Promise.all(txs.map(async tx=>{{
          const signed=await f.walletProvider.signTransaction(VersionedTransaction.deserialize(getTransactionEncoder().encode(tx)));
          return {{[f.walletProvider.getAddress()]:signed.signatures[1]}};
        }}))}};
        c.http=new x402HTTPClient(new x402Client().register('solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp',new ExactSvmScheme(signer,{{rpcUrl:f.config.rpcUrl}})));
      }}
      const result=await c.purchase(r);c.close();
      console.log(JSON.stringify({{config:f.config,key:f.walletKeypair,request:r,result}}));
      for await(const chunk of process.stdin){{console.log(JSON.stringify(f.counts()));break;}}
      await f.cleanup();
    """
    process = subprocess.Popen([NODE, "--input-type=module", "-e", code], cwd=JS, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        first = await asyncio.wait_for(asyncio.to_thread(process.stdout.readline), 20)
        assert first, process.stderr.read()
        data = json.loads(first)
        c = data["config"]
        # Keys here are freshly generated unfunded fixture keys, never live wallet material.
        key = Keypair.from_bytes(bytes(data["key"]))
        class NeverSign:
            address = str(key.pubkey())
            def sign_transaction(self, _):
                raise AssertionError("existing Node authorization must not be signed again")
        config = dict(gateway_url=c["gatewayUrl"], rpc_url=c["rpcUrl"], pay_to=c["payTo"], gateway_buyer=c["gatewayBuyer"], operators=c["operators"], wallet=NeverSign(), journal_path=c["journalPath"], max_payment_micro_usdc=c["maxPaymentMicroUsdc"], max_total_micro_usdc=c["maxTotalMicroUsdc"], allow_localhost=True)
        req = dict(data["request"]); req["request_id"] = req.pop("requestId")
        async with CovenantX402Client(**config) as client:
            before = client.journal.get(req["request_id"])
            result = await client.purchase(**req)
            after = client.journal.get(req["request_id"])
            assert result["status"] == "completed"
            assert before["payment"] == after["payment"] and before["amount"] == after["amount"]
            if not pending:
                assert result == data["result"]
        process.stdin.write("counts\n"); process.stdin.flush()
        counts = json.loads(await asyncio.wait_for(asyncio.to_thread(process.stdout.readline), 10))
        assert counts["signatures"] == counts["quotes"] == 1
        assert counts["sends"] == (2 if pending else 1)
        if pending:
            assert counts["payments"][0] == counts["payments"][1]
    finally:
        if process.poll() is None:
            process.stdin.close()
            await asyncio.to_thread(process.wait, 10)
        assert process.returncode == 0, process.stderr.read()


async def test_node_reads_python_completed_journal_without_network(tmp_path):
    from fixture import Fixture, request
    f, req = Fixture(tmp_path), request()
    async with f.client() as client:
        result = await client.purchase(**req)
    config = {"gatewayUrl": f.config["gateway_url"], "rpcUrl": f.config["rpc_url"], "payTo": f.config["pay_to"], "gatewayBuyer": f.config["gateway_buyer"], "operators": f.config["operators"], "journalPath": f.config["journal_path"], "maxPaymentMicroUsdc": 1000, "maxTotalMicroUsdc": 2000, "payer": f.wallet.address}
    js_request = {**req}; js_request["requestId"] = js_request.pop("request_id")
    code = f"""import{{CovenantX402Client,SOLANA_GENESIS}}from {json.dumps((JS/'index.mjs').as_uri())};
    const config=JSON.parse(process.argv[1]),request=JSON.parse(process.argv[2]);
    const walletProvider={{getNetwork:()=>({{protocolFamily:'svm',chainId:SOLANA_GENESIS}}),getAddress:()=>config.payer,signTransaction:()=>{{throw Error('must_not_sign')}}}};
    const c=new CovenantX402Client({{...config,walletProvider,fetchImpl:()=>{{throw Error('must_not_fetch')}}}});
    try{{console.log(JSON.stringify(await c.purchase(request)))}}finally{{c.close()}};"""
    assert json.loads(node(code, json.dumps(config), json.dumps(js_request))) == result


async def test_python_authorization_accepted_by_real_facilitator_without_broadcast(tmp_path):
    from fixture import Fixture, request
    from covenant_compute_x402.payment import decode64
    f = Fixture(tmp_path)
    async with f.client() as client:
        await client.purchase(**request())
    bridge = JS.parents[1] / "services/compute-x402-facilitator/bridge.mjs"
    if not bridge.exists():
        pytest.skip("facilitator source required")
    payload = json.loads(decode64(f.payments[0]))
    code = f"""
      import{{createBridge}}from {json.dumps(bridge.as_uri())};
      import{{createKeyPairSignerFromBytes}}from '@solana/kit';
      import{{toFacilitatorSvmSigner}}from '@x402/svm';
      const p=JSON.parse(process.argv[1]),fee=Uint8Array.from(JSON.parse(process.argv[2]));
      const signer=toFacilitatorSvmSigner(await createKeyPairSignerFromBytes(fee));
      signer.simulateTransaction=async()=>{{}};
      const bridge=createBridge({{signer,config:{{network:p.accepted.network,asset:p.accepted.asset,payTo:p.accepted.payTo,feePayer:p.accepted.extra.feePayer,resourceUrl:p.resource.url,priceAtomic:p.accepted.amount,secret:'unfunded-test-secret-with-more-than-32-chars'}},rpc:async(method)=>{{if(method!=='getGenesisHash')throw Error('broadcast_forbidden');return '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d'}}}});
      const verified=await bridge.verify({{paymentPayload:p,paymentRequirements:p.accepted,resourceUrl:p.resource.url}});
      console.log(JSON.stringify({{payer:verified.payer,authorizationId:verified.authorization_id}}));
    """
    output = subprocess.run([NODE, "--input-type=module", "-e", code, json.dumps(payload), json.dumps(list(bytes(f.fee)))], cwd=JS, capture_output=True, text=True, check=True)
    verified = json.loads(output.stdout)
    assert verified["payer"] == f.wallet.address
    assert verified["authorizationId"] == sha256(decode64(payload["payload"]["transaction"]))
