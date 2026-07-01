// Covenant verified-ER registry. Publishes "this MagicBlock ER is
// Covenant-verified" as a Solana Attestation Service attestation keyed to the
// validator identity that getRoutes returns, and resolves it the way an agent
// would: one account read from the validator pubkey, no router change, no indexer.
//
// The data comes from covenant-tee: the ER's TDX enclave is DCAP-verified, then
// the result (TCB status + mr_td measurement) is attested on chain under the
// Covenant credential. An agent trusts it by checking the attestation's signer
// is an authorized signer of that credential.
//
//   SAS_RPC=https://api.devnet.solana.com node er-registry.mjs            # attest + resolve mainnet-tee
//   node er-registry.mjs resolve <validatorPubkey>                        # agent-side lookup only
import {
  createSolanaRpc, createKeyPairSignerFromBytes, address, lamports, pipe,
  createTransactionMessage, setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash, appendTransactionMessageInstructions,
  signTransactionMessageWithSigners, getBase64EncodedWireTransaction,
} from "@solana/kit";
import {
  deriveCredentialPda, deriveSchemaPda, deriveAttestationPda,
  getCreateCredentialInstruction, getCreateSchemaInstruction, getCreateAttestationInstruction,
  fetchCredential, fetchSchema, fetchMaybeCredential, fetchMaybeSchema, fetchMaybeAttestation,
  serializeAttestationData, deserializeAttestationData,
} from "sas-lib";
import fs from "node:fs";
import os from "node:os";

const CRED_NAME = "covenant";
const SCHEMA_NAME = "er-verified";
const SCHEMA_VERSION = 1;
const LAYOUT = Uint8Array.from([10, 12, 13, 8, 12]); // bool, String, Vec<u8>, i64, String
const FIELDS = ["verified", "status", "mr_td", "verified_at", "endpoint"];
const SCHEMA_DESC = "Covenant-verified MagicBlock ephemeral rollup (TDX enclave, DCAP attested)";

export const rpcFor = (url) => createSolanaRpc(url);
export const signerFromFile = (path) =>
  createKeyPairSignerFromBytes(Uint8Array.from(JSON.parse(fs.readFileSync(path, "utf8"))));

async function send(rpc, signer, ixs) {
  const { value: bh } = await rpc.getLatestBlockhash().send();
  const msg = pipe(
    createTransactionMessage({ version: 0 }),
    (m) => setTransactionMessageFeePayerSigner(signer, m),
    (m) => setTransactionMessageLifetimeUsingBlockhash(bh, m),
    (m) => appendTransactionMessageInstructions(ixs, m),
  );
  const signed = await signTransactionMessageWithSigners(msg);
  const wire = getBase64EncodedWireTransaction(signed);
  const sig = await rpc.sendTransaction(wire, { encoding: "base64", preflightCommitment: "confirmed" }).send();
  for (let i = 0; i < 40; i++) {
    const st = (await rpc.getSignatureStatuses([sig]).send()).value[0];
    if (st?.confirmationStatus === "confirmed" || st?.confirmationStatus === "finalized") {
      if (st.err) throw new Error("tx failed: " + JSON.stringify(st.err));
      return sig;
    }
    await new Promise((r) => setTimeout(r, 1500));
  }
  throw new Error("confirm timeout " + sig);
}

async function issuerPdas(authority) {
  const [credential] = await deriveCredentialPda({ authority: address(authority), name: CRED_NAME });
  const [schema] = await deriveSchemaPda({ credential, name: SCHEMA_NAME, version: SCHEMA_VERSION });
  return { credential, schema };
}

export async function ensureIssuer(rpc, authority) {
  const { credential, schema } = await issuerPdas(authority.address);
  if (!(await fetchMaybeCredential(rpc, credential)).exists) {
    await send(rpc, authority, [getCreateCredentialInstruction({
      payer: authority, credential, authority, name: CRED_NAME, signers: [authority.address],
    })]);
  }
  if (!(await fetchMaybeSchema(rpc, schema)).exists) {
    await send(rpc, authority, [getCreateSchemaInstruction({
      payer: authority, authority, credential, schema,
      name: SCHEMA_NAME, description: SCHEMA_DESC, layout: LAYOUT, fieldNames: FIELDS,
    })]);
  }
  return { credential, schema };
}

export async function attestEr(rpc, authority, validator, v) {
  const { credential, schema } = await ensureIssuer(rpc, authority);
  const nonce = address(validator);
  const [attestation] = await deriveAttestationPda({ credential, schema, nonce });
  if ((await fetchMaybeAttestation(rpc, attestation)).exists) return { attestation, sig: null, existed: true };
  const schemaAcct = await fetchSchema(rpc, schema);
  const data = serializeAttestationData(schemaAcct.data, {
    verified: v.verified, status: v.status, mr_td: Array.from(v.mrTd),
    verified_at: BigInt(v.verifiedAt), endpoint: v.endpoint,
  });
  const sig = await send(rpc, authority, [getCreateAttestationInstruction({
    payer: authority, authority, credential, schema, attestation,
    nonce, data, expiry: BigInt(v.expiry ?? 0),
  })]);
  return { attestation, sig, existed: false };
}

export async function resolveEr(rpc, issuerAuthority, validator) {
  const { credential, schema } = await issuerPdas(issuerAuthority);
  const [attestation] = await deriveAttestationPda({ credential, schema, nonce: address(validator) });
  const acct = await fetchMaybeAttestation(rpc, attestation);
  if (!acct.exists) return { verified: false, reason: "no attestation for this validator" };
  const cred = await fetchCredential(rpc, credential);
  const trustedSigner = cred.data.authorizedSigners.includes(acct.data.signer);
  const notExpired = acct.data.expiry === 0n || acct.data.expiry > BigInt(Math.floor(Date.now() / 1000));
  const d = deserializeAttestationData(await fetchSchema(rpc, schema).then((s) => s.data), Uint8Array.from(acct.data.data));
  return { verified: Boolean(trustedSigner && notExpired && d.verified), trustedSigner, notExpired, attestation, signer: acct.data.signer, data: d };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const KEY = [process.env.COVENANT_ATTESTER_KEY, `${os.homedir()}/.config/solana/covenant-agent.json`, `${os.homedir()}/.config/solana/id.json`].find((p) => p && fs.existsSync(p));
  const RPC_URL = process.env.SAS_RPC || "https://api.devnet.solana.com";
  const rpc = rpcFor(RPC_URL);
  const issuer = await signerFromFile(KEY);

  if (process.argv[2] === "resolve") {
    const r = await resolveEr(rpc, issuer.address, process.argv[3]);
    console.log(JSON.stringify(r, (_, x) => (typeof x === "bigint" ? Number(x) : x), 2));
    process.exit(r.verified ? 0 : 1);
  }

  const TEE = process.env.TEE || "https://mainnet-tee.magicblock.app";
  console.log("issuer (Covenant):", issuer.address);
  console.log("SAS RPC          :", RPC_URL, "| TEE:", TEE);
  let bal = (await rpc.getBalance(issuer.address).send()).value;
  if (bal < 50_000_000n && /devnet|localhost|127\.0\.0\.1/.test(RPC_URL)) {
    console.log("airdropping devnet SOL...");
    try {
      await rpc.requestAirdrop(issuer.address, lamports(1_000_000_000n)).send();
      for (let i = 0; i < 20 && bal < 50_000_000n; i++) { await new Promise((r) => setTimeout(r, 1500)); bal = (await rpc.getBalance(issuer.address).send()).value; }
    } catch (e) { console.log("airdrop failed:", e.message); }
  }
  console.log("issuer balance   :", (Number(bal) / 1e9).toFixed(4), "SOL");

  const { attest, signerFromKeypairFile } = await import("./covenant-tee.mjs");
  const att = await attest({ teeUrl: TEE, signer: signerFromKeypairFile(KEY) });
  const validator = att.er.validator;
  const mrTd = Buffer.from(String(att.enclave.mr_td).replace(/^0x/, ""), "hex");
  console.log(`\nenclave verified : ${validator} | TCB ${att.enclave.status} | mr_td ${String(att.enclave.mr_td).slice(0, 18)}...`);

  const { attestation, sig, existed } = await attestEr(rpc, issuer, validator, {
    verified: att.enclave.status === "UpToDate", status: att.enclave.status, mrTd,
    verifiedAt: att.verified_at, endpoint: new URL(TEE).host,
  });
  console.log(`SAS attestation  : ${attestation}${existed ? " (already published)" : "\n             tx  : " + sig}`);

  const r = await resolveEr(rpc, issuer.address, validator);
  console.log(`\nagent resolve(${validator.slice(0, 8)}...):`);
  console.log("  verified       :", r.verified, "| trusted signer:", r.trustedSigner, "| not expired:", r.notExpired);
  console.log("  data           :", JSON.stringify({ ...r.data, mr_td: `<${r.data.mr_td.length} bytes>`, verified_at: Number(r.data.verified_at) }));
  console.log(r.verified ? "\nCOVENANT-VERIFIED ER (on-chain via SAS) ✓" : "\nNOT VERIFIED");
}
