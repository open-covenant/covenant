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


@pytest.mark.parametrize("pending,legacy,tariff,batch", [(True, False, False, False), (False, False, False, False), (True, True, False, False), (True, False, True, False), (False, False, True, False), (True, False, True, True), (False, False, True, True), (True, True, True, True), (False, True, True, True)])
async def test_python_resumes_real_node_journal_without_resigning(pending, legacy, tariff, batch):
    code = f"""
      import{{fixture,input}}from {json.dumps((JS/'test/fixtures/mock-gateway.mjs').as_uri())};
      import{{CovenantX402Client}}from {json.dumps((JS/'index.mjs').as_uri())};
      import{{x402Client,x402HTTPClient}}from '@x402/core/client';
      import{{ExactSvmScheme}}from '@x402/svm/exact/client';
      import{{getTransactionEncoder}}from '@solana/kit';
      import{{VersionedTransaction}}from '@solana/web3.js';
      const f=await fixture({{pendingFirst:{str(pending).lower()},expectedUnits:{60000 if legacy else 20000},legacyMemo:{str(legacy).lower()}}});
      if({str(batch).lower()}){{f.config.maxPaymentMicroUsdc=10000;f.config.maxTotalMicroUsdc=20000;}}
      const c=new CovenantX402Client(f.config);let r=input();
      if({str(tariff).lower()})r.pricing='model_tariff_v1';
      if({str(batch).lower()}){{const{{requestId,...child}}=r;r={{requestId,version:1,requests:[child,{{...child,max_tokens:128}}]}};}}
      if({str(legacy).lower()}){{
        // A v2 quote paid at 60k CU by a client from before v3 memos.
        const payer=f.walletProvider.getAddress();
        const signer={{address:payer,modifyAndSignTransactions:async txs=>Promise.all(txs.map(async tx=>{{
          const unsigned=VersionedTransaction.deserialize(getTransactionEncoder().encode(tx));
          const limit=Buffer.from(unsigned.message.compiledInstructions[0].data);limit.writeUInt32LE(60000,1);unsigned.message.compiledInstructions[0].data=limit;
          const message=Buffer.from(unsigned.message.serialize());const signed=await f.walletProvider.signTransaction(unsigned);
          return {{...tx,messageBytes:Uint8Array.from(message),signatures:{{...tx.signatures,[payer]:Uint8Array.from(signed.signatures[1])}}}};
        }}))}};
        c.http=new x402HTTPClient(new x402Client().register('solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp',new ExactSvmScheme(signer,{{rpcUrl:f.config.rpcUrl}})));
      }}
      const result=await c.{'purchaseBatch' if batch else 'purchase'}(r);c.close();
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
            result = await (client.purchase_batch(**req) if batch else client.purchase(**req))
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


@pytest.mark.parametrize("tariff,batch,largest", [(False, False, False), (True, False, False), (False, True, False), (True, True, False), (True, True, True)])
async def test_node_reads_python_completed_journal_without_network(tmp_path, tariff, batch, largest):
    from fixture import Fixture, request
    from batch_fixture import BatchFixture, batch_request
    f, req = (BatchFixture(tmp_path), batch_request()) if batch else (Fixture(tmp_path), request())
    if largest:
        req = batch_request(32)
        for child in req["requests"]:
            child.update(model="\\"*128, max_tokens=8192)
    if tariff and not batch:
        req["pricing"] = "model_tariff_v1"
    if batch and not tariff:
        for child in req["requests"]:
            child.pop("pricing", None)
    async with f.client() as client:
        if largest:
            with pytest.raises(CovenantError, match="fresh_batch_size_exceeded"):
                await client.purchase_batch(**req)
            assert not f.calls
            await f.seed_historical_authorization(client, req)
        result = await (client.purchase_batch(**req) if batch else client.purchase(**req))
        if largest:
            from fixture import encode
            row = client.journal.get(req["request_id"])
            assert len(f.payments[0]) < 65_536
            assert len(encode(strict_json(row["quote"]))) < 65_536
    config = {"gatewayUrl": f.config["gateway_url"], "rpcUrl": f.config["rpc_url"], "payTo": f.config["pay_to"], "gatewayBuyer": f.config["gateway_buyer"], "operators": f.config["operators"], "journalPath": f.config["journal_path"], "maxPaymentMicroUsdc": f.config["max_payment_micro_usdc"], "maxTotalMicroUsdc": f.config["max_total_micro_usdc"], "payer": f.wallet.address}
    js_request = {**req}; js_request["requestId"] = js_request.pop("request_id")
    code = f"""import{{CovenantX402Client,SOLANA_GENESIS}}from {json.dumps((JS/'index.mjs').as_uri())};
    const config=JSON.parse(process.argv[1]),request=JSON.parse(process.argv[2]);
    const walletProvider={{getNetwork:()=>({{protocolFamily:'svm',chainId:SOLANA_GENESIS}}),getAddress:()=>config.payer,signTransaction:()=>{{throw Error('must_not_sign')}}}};
    const c=new CovenantX402Client({{...config,walletProvider,fetchImpl:()=>{{throw Error('must_not_fetch')}}}});
    try{{console.log(JSON.stringify(await c.{'purchaseBatch' if batch else 'purchase'}(request)))}}finally{{c.close()}};"""
    assert json.loads(node(code, json.dumps(config), json.dumps(js_request))) == result


@pytest.mark.parametrize("legacy_memo", [False, True])
async def test_python_tariff_and_legacy_authorizations_accepted_by_one_real_facilitator(tmp_path, legacy_memo):
    from fixture import Fixture, request
    from covenant_compute_x402.payment import decode64
    bridge = JS.parents[1] / "services/compute-x402-facilitator/bridge.mjs"
    if not bridge.exists():
        pytest.skip("facilitator source required")
    f = Fixture(tmp_path, price=lambda body: 12000 + 9 * body["max_tokens"] if body.get("pricing") else 50000,
                legacy_memo=legacy_memo)
    requests = [dict(request(), model="claude-haiku-4.5", max_tokens=256, pricing="model_tariff_v1"),
                dict(request(), model="claude-haiku-4.5", max_tokens=2048, pricing="model_tariff_v1"),
                dict(request(), model="claude-haiku-4.5", max_tokens=256)]
    amounts = [14304, 30432, 50000]
    async with f.client(max_payment_micro_usdc=50000, max_total_micro_usdc=100000) as client:
        for req, amount in zip(requests, amounts):
            assert (await client.purchase(**req))["amountMicroUsdc"] == amount
    payloads = [json.loads(decode64(payment)) for payment in f.payments]
    code = f"""
      import assert from 'node:assert/strict';
      import{{createBridge,Rejected}}from {json.dumps(bridge.as_uri())};
      import{{createKeyPairSignerFromBytes}}from '@solana/kit';
      import{{toFacilitatorSvmSigner}}from '@x402/svm';
      const payments=JSON.parse(process.argv[1]),fee=Uint8Array.from(JSON.parse(process.argv[2]));
      const first=payments[0],now=Date.now();
      const tariff={{schema_version:1,revision:1,model:'claude-haiku-4.5',max_output_tokens:2048,
        valid_from_ms:now-60000,valid_until_ms:now+3600000}};
      // The deployment policy is independent of the amount in any payment.
      const config={{network:'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp',
        asset:'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',payTo:first.accepted.payTo,
        feePayer:first.accepted.extra.feePayer,resourceUrl:'https://compute.example/v1/chat/completions',
        priceAtomic:'50000',secret:'unfunded-test-secret-with-more-than-32-chars',tariffRows:[{{
          native:{{...tariff,id:'fixture-native',base_micro_usdc:10000,output_cap_micro_usdc_per_million:7500000}},
          retail:{{...tariff,id:'fixture-retail',base_micro_usdc:12000,output_cap_micro_usdc_per_million:9000000}},
        }}]}};
      const signer=toFacilitatorSvmSigner(await createKeyPairSignerFromBytes(fee));
      let sponsorSignatures=0,simulations=0;
      const sign=signer.signTransaction.bind(signer);
      signer.signTransaction=async(...args)=>{{sponsorSignatures++;return sign(...args)}};
      signer.simulateTransaction=async()=>{{simulations++}};
      const rpcMethods=[];
      const bridge=createBridge({{signer,config,rpc:async method=>{{
        rpcMethods.push(method);assert.equal(method,'getGenesisHash','broadcast_forbidden');
        return '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d';
      }}}});
      const verify=(p,terms=p.accepted)=>bridge.verify({{paymentPayload:p,paymentRequirements:terms,resourceUrl:config.resourceUrl}});
      const verified=[];
      for(const p of payments){{
        const result=await verify(p);
        verified.push({{payer:result.payer,authorizationId:result.authorization_id}});
      }}
      assert.equal(sponsorSignatures,3);assert.equal(simulations,3);
      const nonmember=structuredClone(first);nonmember.accepted.amount='14305';
      const changedAccepted=structuredClone(first);changedAccepted.accepted.amount='30432';
      // The original signature remains valid, but its transfer is 14304 rather
      // than the independently allowed 30432 now claimed by these terms.
      const changedTransfer=structuredClone(first);changedTransfer.accepted.amount='30432';
      const rejections=[];
      for(const [p,terms] of [[nonmember,nonmember.accepted],[changedAccepted,first.accepted],[changedTransfer,changedTransfer.accepted]]){{
        await assert.rejects(verify(p,terms),error=>{{assert(error instanceof Rejected);rejections.push(error.message);return true}});
        assert.equal(sponsorSignatures,3,'rejection must precede sponsor signing');
        assert.equal(simulations,3,'static rejection must precede simulation');
      }}
      console.log(JSON.stringify({{verified,rejections,sponsorSignatures,simulations,rpcMethods}}));
    """
    output = subprocess.run([NODE, "--input-type=module", "-e", code, json.dumps(payloads), json.dumps(list(bytes(f.fee)))], cwd=JS, capture_output=True, text=True, check=True)
    result = json.loads(output.stdout)
    assert [payload["accepted"]["amount"] for payload in payloads] == list(map(str, amounts))
    assert [row["payer"] for row in result["verified"]] == [f.wallet.address] * 3
    assert [row["authorizationId"] for row in result["verified"]] == [sha256(decode64(p["payload"]["transaction"])) for p in payloads]
    assert result["rejections"][:2] == ["payment amount is not configured for admission", "payment payload does not match quote"]
    assert len(result["rejections"]) == 3
    assert result["sponsorSignatures"] == result["simulations"] == f.wallet.signatures == 3
    assert result["rpcMethods"] and set(result["rpcMethods"]) == {"getGenesisHash"}
