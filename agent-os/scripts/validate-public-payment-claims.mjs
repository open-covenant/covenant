#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = dirname(dirname(dirname(fileURLToPath(import.meta.url))));

const publicPaymentFiles = [
  'README.md',
  'BUILT.md',
  'docs/x402.md',
  'docs/spend-authorization.md',
  'docs/escrow-completion.md',
  'docs/metaplex-integration.md',
  'docs/agent-registration.md',
  'docs/magicblock-integration.md',
  'docs/provenance/witness-loop-overview.md',
  'docs/multichain-value-capture.md',
  'docs/integrations/krexa.md',
  'landing/app/about/page.tsx',
  'landing/app/_primitives.ts',
  'landing/app/_partners.ts',
  'landing/app/brand/page.tsx',
  'landing/app/live/page.tsx',
  'landing/app/faq/page.tsx',
  'landing/app/guard/page.tsx',
  'landing/app/page.tsx',
  'landing/app/robinhood/page.tsx',
  'landing/app/trading/page.tsx',
  'landing/app/docs/page.tsx',
  'landing/app/docs/architecture/page.tsx',
  'landing/app/docs/concepts/page.tsx',
  'landing/app/docs/primitives/page.tsx',
  'landing/app/docs/identity/page.tsx',
  'landing/app/docs/multichain/page.tsx',
  'landing/app/docs/x402/page.tsx',
  'landing/app/docs/settlement/page.tsx',
  'landing/app/blog/covenant-is-now-multichain/page.tsx',
  'landing/app/blog/covenant-payai/page.tsx',
  'landing/app/blog/page.tsx',
  'landing/app/onepager/page.tsx',
  'landing/public/.well-known/x402',
  'landing/public/.well-known/x402.json',
  'landing/public/agents/covenant-foundation.json',
  'landing/public/agents/covenant-sandbox.json',
  'landing/public/metaplex/collection.json',
  'landing/lib/verify/commitMemo.ts',
  'landing/lib/verify/settlement.ts',
  'landing/lib/verify/verifierSig.ts',
  'services/x402-seller-base/package.json',
  'services/x402-seller-base/src/attest-route.ts',
  'services/x402-seller-base/src/__tests__/attest.test.ts',
  'services/x402-seller-base/src/attest.ts',
  'services/x402-seller-base/src/server.ts',
  'agent-os/crates/covenantd/src/escrow.rs',
  'agent-os/crates/covenantd/src/reputation.rs',
  'agent-os/crates/covenant-identity/src/registration.rs',
  'agent-os/crates/covenant-identity/src/lib.rs',
  'agent-os/crates/covenant-identity/Cargo.toml',
  'agent-os/crates/covenant-audit/src/lib.rs',
  'agent-os/crates/covenant-audit/src/reputation.rs',
  'agent-os/crates/covenant-evm-signer/src/reputation.rs',
  'agent-os/crates/covenant-evm-signer/src/lib.rs',
  'agent-os/crates/covenant-evm-signer/README.md',
  'agent-os/crates/covenant-evm-signer/Cargo.toml',
  'agent-os/crates/covenant-metaplex/src/verify.rs',
  'agent-os/crates/covenant-metaplex/src/request.rs',
  'agent-os/crates/covenant-metaplex/src/tools.rs',
];

const forbidden = [
  /settlement-grounded reputation/i,
  /Covenant-Verified/i,
  /PayAI moves the money\. Covenant proves the work/i,
  /confirm a real identity/i,
  /no one can forge it/i,
  /payment held/i,
  /settled jobs/i,
  /identity passports/i,
  /identity passport/i,
  /reputation reads/i,
  /never charged/i,
  /cancels settlement/i,
  /trustless on-chain decode/i,
  /the trust layer is live/i,
  /the infrastructure builds itself/i,
  /every agent runs under/i,
  /every action leaves a receipt/i,
  /facts are signatures/i,
  /one agent, one record, provable/i,
  /a receipt for every decision/i,
  /Covenant Trust x402 seller/i,
  /verify responses without trusting/i,
  /pay only for good output/i,
  /There is no step where you trust Covenant/i,
  /spending you can walk away from/i,
  /Covenant as the trust layer/i,
  /safe for the hirer/i,
  /trust the parsed fields/i,
  /track record an agent can prove/i,
  /every decision[^.]*anchored onchain/i,
  /verifiable from the wire format alone/i,
  /neither the worker nor the operator can inflate/i,
  /discover and trust the same agent/i,
  /with no Covenant infrastructure in the trust path/i,
  /isolated minting key/i,
  /runs exactly this in the browser/i,
  /migration is a re-anchor/i,
  /authorship is a chain fact/i,
  /reputation passport/i,
  /every decision lands on-chain/i,
  /amount actually settled/i,
  /this is the release signal/i,
  /this proves, it does not pay/i,
  /proof that authorized the release/i,
  /real yield from real usage/i,
  /USDC per-call fees earned on Base[^.]*feed/i,
  /Covenant-accountable/i,
  /DAS-only verification of agent accountability/i,
  /same key[^.]*on-chain settlement/i,
  /same key signs[^.]*Solana settlement/i,
  /live TDX verification/i,
  /non-transferable identity binding/i,
  /canonical audit-derived reputation/i,
  /tamper-evident, DAS-queryable record of an agent's history/i,
];

const required = new Map([
  [
    'docs/escrow-completion.md',
    [
      'not a release authorization',
      'supplied by the caller',
      'must not be used by itself to release funds',
      'PINNED_COVENANT_PUBKEY',
    ],
  ],
  [
    'docs/metaplex-integration.md',
    [
      'does not establish the real-world operator',
      'process separation, not a security isolation boundary',
      'does not create ERC-8004 interoperability',
      'page does not perform this recomputation',
    ],
  ],
  [
    'docs/agent-registration.md',
    [
      'It does not make those claims true',
      'A valid self-signature authenticates',
      'A consumer must verify those values',
    ],
  ],
  [
    'docs/magicblock-integration.md',
    [
      'caller-supplied receipt hash',
      'does not prove that the bytes describe work',
      'not independently reproduce the DCAP verification',
      'does not prove that the runtime mediated the prompts',
    ],
  ],
  [
    'landing/app/about/page.tsx',
    ['outside Covenant', 'operating-system isolation', 'does not prove'],
  ],
  [
    'landing/app/guard/page.tsx',
    [
      'Covenant Evidence is a read-only evidence reader',
      'It does not approve or block payments',
      'a signature proves authorship, not truth',
    ],
  ],
  [
    'landing/app/page.tsx',
    ['The current release is local-first', 'does not prove every command was mediated'],
  ],
  [
    'landing/app/robinhood/page.tsx',
    [
      'These controls cover funds held by this escrow contract',
      'the attestor remains trusted for the semantic judgment',
      'They do not establish the semantic quality',
    ],
  ],
  [
    'landing/app/trading/page.tsx',
    [
      'No brokerage account is attached',
      'do not prove that a live venue was mediated',
      'not a production trading boundary',
    ],
  ],
  [
    'docs/x402.md',
    [
      'The seller speaks x402 v2',
      'legacy v1 payment envelope',
      'A valid signature authenticates Covenant as publisher',
      'Resource delivery and settlement are separate',
    ],
  ],
  [
    'landing/app/docs/x402/page.tsx',
    ['Delivery and settlement are separate', 'do not assume no charge'],
  ],
  [
    'docs/multichain-value-capture.md',
    [
      'authenticates the configured publisher',
      'not the underlying claim',
      'Cross-chain revenue routing into it is not implemented',
    ],
  ],
  [
    'landing/app/docs/multichain/page.tsx',
    ['authenticates the publisher', 'not the claim or real-world operator'],
  ],
  [
    'docs/spend-authorization.md',
    [
      'advisory preflight, not signer enforcement',
      'The wallet remains the enforcement boundary',
      'The guarantee is process-local',
      'multiple daemon processes must not share the same files',
    ],
  ],
  [
    'docs/integrations/krexa.md',
    ['signer-authenticated heuristic', 'consumers still trust the score program'],
  ],
  [
    'landing/app/blog/covenant-payai/page.tsx',
    ['That inference was too', 'Those observations are not jobs or reputation'],
  ],
  ['landing/app/onepager/page.tsx', ['This document is outdated and must not be distributed']],
  [
    'services/x402-seller-base/src/attest.ts',
    [
      'it does not establish that the claim',
      'expectedPubkeyB58',
      'att.pubkey_b58 !== expectedPubkeyB58',
    ],
  ],
  [
    'services/x402-seller-base/src/attest-route.ts',
    ['Resource status and payment', 'settlement are separate outcomes'],
  ],
  [
    'services/x402-seller-base/src/server.ts',
    [
      'statement over caller-supplied data',
      'the signature does not establish claim truth',
      'A handler error is not evidence that settlement failed',
    ],
  ],
  [
    'agent-os/crates/covenantd/src/escrow.rs',
    ['caller-supplied escrow context', 'not a release authorization'],
  ],
  [
    'agent-os/crates/covenantd/src/reputation.rs',
    ['not independent reputation', 'does not verify payouts onchain'],
  ],
  [
    'landing/app/_partners.ts',
    [
      'the external wallet remains the signing boundary',
      'they are not reputation truth',
      'no brokerage account, live order path, or venue-enforced gate is attached',
    ],
  ],
  [
    'agent-os/crates/covenant-identity/src/registration.rs',
    [
      'its signature authenticates the authoring key, not the claims or operator',
      'No trust model is advertised by default',
      'DEFAULT_SUPPORTED_TRUST: &[&str] = &[]',
    ],
  ],
  [
    'agent-os/crates/covenant-identity/src/lib.rs',
    [
      'Payment funding keys are separate',
      'signs local protocol statements, not payment',
    ],
  ],
  [
    'landing/app/_primitives.ts',
    ['Solana and EVM payment funding keys are separate'],
  ],
  [
    'landing/app/docs/identity/page.tsx',
    [
      'payment funding keys are',
      'not derived from this identity',
      'separate receipt and',
    ],
  ],
  [
    'agent-os/crates/covenant-identity/Cargo.toml',
    ['Payment funding keys are separate'],
  ],
  [
    'agent-os/crates/covenant-audit/src/lib.rs',
    [
      'does not query the chain or',
      'not sufficient authorization',
      'proposed atomic amount',
      'whether the wallet later signs or settles',
    ],
  ],
  [
    'agent-os/crates/covenant-audit/src/reputation.rs',
    [
      'Experimental event-classification heuristic',
      'does not establish event completeness',
      'counted without inspecting its status',
    ],
  ],
  [
    'agent-os/crates/covenant-evm-signer/src/reputation.rs',
    [
      'not wired into',
      'does not verify an anchor',
      'authenticates only the',
    ],
  ],
  [
    'agent-os/crates/covenant-evm-signer/src/lib.rs',
    [
      'experimental format utility',
      'does not verify',
      'authenticates only the publisher',
      'opaque caller-supplied reference',
    ],
  ],
  [
    'agent-os/crates/covenant-evm-signer/README.md',
    [
      'caller-supplied score',
      'does not fetch that account',
      'not wired to score publication',
      'Base mainnet (8453) to `version()` `1.0.1`',
    ],
  ],
  [
    'agent-os/crates/covenant-evm-signer/Cargo.toml',
    ['Not wired to publication', 'authenticate bytes, not claims'],
  ],
  [
    'agent-os/crates/covenant-metaplex/src/verify.rs',
    [
      'Structural observations over configured DAS-provider responses',
      'does not authenticate',
      'matches_expected_envelope',
      'has_matching_record',
    ],
  ],
  [
    'agent-os/crates/covenant-metaplex/src/request.rs',
    ['does not create generic-wallet', 'do not prove identity'],
  ],
  [
    'agent-os/crates/covenant-metaplex/src/tools.rs',
    ['observe.record', 'observe.agent_records', 'not claim verification'],
  ],
  [
    'docs/provenance/witness-loop-overview.md',
    [
      'do not currently query or decode RPC state',
      'Self-published verifier statement',
      'stays yellow',
    ],
  ],
  [
    'landing/public/metaplex/collection.json',
    ['does not prove log completeness', 'real-world operator'],
  ],
  [
    'landing/lib/verify/commitMemo.ts',
    ['never a chain-verified green', 'has not queried or decoded'],
  ],
  [
    'landing/lib/verify/settlement.ts',
    ['never chain-verified', 'has not fetched or decoded'],
  ],
  [
    'landing/lib/verify/verifierSig.ts',
    [
      'covenant.witness-verdict.v2',
      'not an externally pinned trust root',
      'legacy or malformed',
    ],
  ],
]);

const errors = [];
for (const path of publicPaymentFiles) {
  const text = readFileSync(join(repoRoot, path), 'utf8');
  const normalized = text.replace(/\s+/g, ' ');
  for (const pattern of forbidden) {
    if (pattern.test(normalized)) {
      errors.push(`${path}: public payment copy contains forbidden overclaim ${pattern}`);
    }
  }
  for (const phrase of required.get(path) ?? []) {
    if (!normalized.includes(phrase)) {
      errors.push(`${path}: required honesty boundary is missing: ${JSON.stringify(phrase)}`);
    }
  }
}

if (errors.length > 0) {
  console.error('public payment-claim validation failed:');
  for (const error of errors) console.error(`- ${error}`);
  process.exit(1);
}

console.log('public payment-claim validation passed');
