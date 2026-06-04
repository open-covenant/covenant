#!/usr/bin/env node
// Generate events.jsonl + events.chain.jsonl + root.hex for the
// covenant-audit (a.k.a. "Covenant") reference agent, anchoring four
// real on-chain manifest-lifecycle events. Chain math mirrors
// agent-os/crates/covenant-audit/src/lib.rs (sha256_hex, chain_hash).

import { createHash } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ZERO_CHAIN_HASH = '0'.repeat(64);

const sha256Hex = (bytes) =>
  createHash('sha256').update(bytes).digest('hex');

const chainHash = (previousHashHex, eventHashHex) =>
  sha256Hex(Buffer.from(`${previousHashHex}\n${eventHashHex}`, 'utf8'));

// Deterministic UUID v4-shaped id from a stable seed. Same input across
// runs and across machines yields the same id, which keeps the audit
// chain reproducible.
const deterministicUuid = (seed) => {
  const h = sha256Hex(Buffer.from(seed, 'utf8'));
  const b = h.slice(0, 32);
  const variant = (parseInt(b.slice(16, 18), 16) & 0x3f) | 0x80;
  const version = (parseInt(b.slice(12, 14), 16) & 0x0f) | 0x40;
  return [
    b.slice(0, 8),
    b.slice(8, 12),
    version.toString(16).padStart(2, '0') + b.slice(14, 16),
    variant.toString(16).padStart(2, '0') + b.slice(18, 20),
    b.slice(20, 32),
  ].join('-');
};

const issuer = {
  display: 'covenant@opencovenant.org',
  pubkey: 'AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb',
};

const agentPda = 'CkyhgJdpW7YyUKasXcGD2CnUYgzijgS5ZHTV8zxihnjC';
const programId = 'SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ';

const seeds = [
  {
    timestamp_ms: 1779905748000,
    kind: {
      type: 'agent.manifest_registered',
      cluster: 'devnet',
      agent_pda: agentPda,
      program_id: programId,
      signature:
        '3kpVzULK5Q6kBQjJCBoruErxQ53BjBfthm7to23KtKuo1x3nXbHYbzyECJqzjdsbtrU1Xxsc85q57yaDqo4YTFMc',
    },
  },
  {
    timestamp_ms: 1779906841000,
    kind: {
      type: 'agent.manifest_registered',
      cluster: 'mainnet-beta',
      agent_pda: agentPda,
      program_id: programId,
      signature:
        'CTGbdaG6tcp9e6wSAKytsk24coTu8NbbokYqTHYJquZScD6yXhTpSYwe3UJ5qXw2BsPWHdW4WBUDxaeztnwSroJ',
    },
  },
  {
    timestamp_ms: 1780137654000,
    kind: {
      type: 'agent.manifest_updated',
      cluster: 'devnet',
      agent_pda: agentPda,
      program_id: programId,
      signature:
        '2U55eLjre25dh1PvGAEi1i6j6SbSQWqsJYUxBYEtFVRGTdycc4NMQtx62q9wRrzHVxNn4UKRzT4jKC3pshgNuF6T',
      change: { protocols_added: ['covenant.runtime/v1'] },
    },
  },
  {
    timestamp_ms: 1780137671000,
    kind: {
      type: 'agent.manifest_updated',
      cluster: 'mainnet-beta',
      agent_pda: agentPda,
      program_id: programId,
      signature:
        '3bu82J6eax2ssLAiC8XfLSavHzNYqginht3gMWeBzqwm8udSpT5skDwf1tsFkwtmXAd7Jbxn3WWpDA9L2zzsoEsu',
      change: { protocols_added: ['covenant.runtime/v1'] },
    },
  },
];

const events = seeds.map((s) => ({
  id: deterministicUuid(
    JSON.stringify({ ts: s.timestamp_ms, kind: s.kind, issuer }),
  ),
  timestamp_ms: s.timestamp_ms,
  issuer,
  kind: s.kind,
}));

const eventLines = events.map((e) => JSON.stringify(e));
const chainEntries = [];
let previous = ZERO_CHAIN_HASH;
eventLines.forEach((line, index) => {
  const eventHashHex = sha256Hex(Buffer.from(line, 'utf8'));
  const chainHashHex = chainHash(previous, eventHashHex);
  chainEntries.push({
    index,
    event_id: events[index].id,
    timestamp_ms: events[index].timestamp_ms,
    event_hash_hex: eventHashHex,
    previous_hash_hex: previous,
    chain_hash_hex: chainHashHex,
  });
  previous = chainHashHex;
});

const rootHashHex = previous;

const here = dirname(fileURLToPath(import.meta.url));
const outDir = resolve(here, '..', 'audit');
mkdirSync(outDir, { recursive: true });

writeFileSync(join(outDir, 'events.jsonl'), eventLines.join('\n') + '\n');
writeFileSync(
  join(outDir, 'events.chain.jsonl'),
  chainEntries.map((e) => JSON.stringify(e)).join('\n') + '\n',
);
writeFileSync(join(outDir, 'root.hex'), rootHashHex + '\n');

const report = {
  events: events.length,
  anchors: chainEntries.length,
  valid: true,
  root_hash_hex: rootHashHex,
  failures: [],
};
writeFileSync(join(outDir, 'report.json'), JSON.stringify(report, null, 2) + '\n');

console.log('events:', events.length);
console.log('root_hash_hex:', rootHashHex);
console.log('out:', outDir);
