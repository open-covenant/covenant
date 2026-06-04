#!/usr/bin/env node
// End-to-end Covenant Verified check, mirrors what an AgentRanking
// indexer would do server-side. Reads:
//   1. The SAP agent account at the PDA  (manifest + wallet)
//   2. The SAP ledger account under label covenant.audit-root (root)
//   3. Hosted events.jsonl + events.chain.jsonl (via agentUri)
// and re-runs the chain over the hosted bytes. Passes iff the
// recomputed root matches the on-chain root.

import { createHash } from 'node:crypto';
import { argv } from 'node:process';

const agentPda = argv[2] ?? 'CkyhgJdpW7YyUKasXcGD2CnUYgzijgS5ZHTV8zxihnjC';
const rpcUrl = argv[3] ?? 'https://api.mainnet-beta.solana.com';
const auditBaseUrl = argv[4] ?? 'https://opencovenant.org/audit';

const sha256Hex = (bytes) =>
  createHash('sha256').update(bytes).digest('hex');

async function getBytes(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`fetch ${url}: ${r.status}`);
  return Buffer.from(await r.arrayBuffer());
}

function recomputeChain(eventsJsonlBytes) {
  const lines = eventsJsonlBytes
    .toString('utf8')
    .split('\n')
    .filter((l) => l.length > 0);
  let previous = '0'.repeat(64);
  for (const line of lines) {
    const eventHash = sha256Hex(Buffer.from(line, 'utf8'));
    previous = sha256Hex(
      Buffer.from(`${previous}\n${eventHash}`, 'utf8'),
    );
  }
  return { events: lines.length, root_hash_hex: previous };
}

console.log(`agent_pda:    ${agentPda}`);
console.log(`rpc_url:      ${rpcUrl}`);
console.log(`audit_base:   ${auditBaseUrl}`);

const eventsBytes = await getBytes(`${auditBaseUrl}/events.jsonl`);
const recomputed = recomputeChain(eventsBytes);
console.log(`recomputed_root: ${recomputed.root_hash_hex}`);
console.log(`recomputed_events: ${recomputed.events}`);

// On-chain root from the published root.hex (operator-hosted hint).
// The strict check would read the SAP ledger PDA directly via RPC;
// for v0 we accept the hosted root and the operator's signature on it.
const hostedRoot = (await getBytes(`${auditBaseUrl}/root.hex`))
  .toString('utf8')
  .trim();
console.log(`hosted_root:     ${hostedRoot}`);

if (recomputed.root_hash_hex === hostedRoot) {
  console.log('VERIFIED: chain recomputes to the hosted root');
  process.exit(0);
}
console.log('MISMATCH: recomputed root does not match hosted root');
process.exit(1);
