#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = dirname(dirname(dirname(fileURLToPath(import.meta.url))));

const publicPaymentFiles = [
  'README.md',
  'BUILT.md',
  'ROADMAP.md',
  'docs/x402.md',
  'docs/audit-integrity.md',
  'docs/spend-authorization.md',
  'docs/production-audit-solana-agent-trust.md',
  'docs/escrow-completion.md',
  'docs/metaplex-integration.md',
  'docs/agent-registration.md',
  'docs/magicblock-integration.md',
  'docs/provenance/witness-loop-overview.md',
  'docs/multichain-value-capture.md',
  'docs/integrations/krexa.md',
  'docs/integrations/acedata.md',
  'docs/hyre-integration.md',
  'docs/capabilities.md',
  'docs/ipc-and-http-gateway.md',
  'docs/zauth.md',
  'docs/memory-drift.md',
  'docs/runtime-sandbox-security.md',
  'docs/getting-started.md',
  'docs/agent-quickstart.md',
  'landing/app/about/page.tsx',
  'landing/app/security/page.tsx',
  'landing/app/partners/page.tsx',
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
  'landing/app/docs/capabilities/page.tsx',
  'landing/app/docs/security/page.tsx',
  'landing/app/docs/audit/page.tsx',
  'landing/app/docs/agent-quickstart/page.tsx',
  'landing/app/docs/multichain/page.tsx',
  'landing/app/docs/x402/page.tsx',
  'landing/app/docs/settlement/page.tsx',
  'landing/app/blog/covenant-is-now-multichain/page.tsx',
  'landing/app/blog/covenant-payai/page.tsx',
  'landing/app/blog/page.tsx',
  'landing/app/onepager/page.tsx',
  'landing/app/agents/[asset]/page.tsx',
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
  'services/indexer/render.yaml',
  'services/indexer/src/verified.rs',
  'agent-os/crates/covenantd/src/escrow.rs',
  'agent-os/crates/covenantd/src/lib.rs',
  'agent-os/crates/covenantd/src/main.rs',
  'agent-os/crates/covenantd/src/metaplex.rs',
  'agent-os/crates/covenantd/src/reputation.rs',
  'agent-os/crates/covenant-ipc/src/lib.rs',
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
  'agent-os/crates/covenant-x402/src/client.rs',
  'agent-os/crates/covenant-x402/src/lib.rs',
  'agent-os/crates/covenant-x402/src/signer.rs',
  'agent-os/crates/covenant-x402/src/bin/xona-demo.rs',
  'agent-os/crates/covenant-hyre/src/lib.rs',
  'agent-os/crates/covenant-hyre/src/tools.rs',
  'agent-os/crates/covenant-hyre/src/x402.rs',
  'agent-os/crates/covenant-circuit/README.md',
  'agent-os/crates/covenant-circuit/src/lib.rs',
  'agent-os/crates/covenant-circuit/src/x402.rs',
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
  /(?:a |the )?valid signature authenticates Covenant as publisher/i,
  /published Covenant key/i,
  /every per-call fee and bond is chain-local USDC/i,
  /every metered call settles in the USDC/i,
  /enclave-verification results/i,
  /daemon proves the local accounting join/i,
  /settlement proves funds moved/i,
  /ecrecover authenticates (?:the |a )?(?:configured )?publisher/i,
  /signature authenticates the verdict/i,
  /Covenant-authored (?:Solana )?records/i,
  /publisher-authenticated statements/i,
  /against the published pubkey/i,
  /authenticate(?:s|d)? the publisher/i,
  /publisher and bytes with[^.]*ecrecover/i,
  /attribute bytes to a configured publisher/i,
  /recovering the published key authenticates/i,
  /ecrecover[^.]*authenticates[^.]*publisher/i,
  /Covenant-signed DCAP verification/i,
  /live DCAP check/i,
  /every generation[^.]*budget-bounded/i,
  /C2PA-style provenance/i,
  /on-chain provenance certificate[^.]*achieved by construction/i,
  /a provable record of[^.]*at what cost/i,
  /renders every dispatch as a verifiable trace/i,
  /locally hash-chained record of every step/i,
  /every call left a tamper-evident trail/i,
  /every state-changing surface in Covenant emits/i,
  /every state-changing operation emits/i,
  /the log is the ground truth/i,
  /the audit log is the system of record/i,
  /every memory write, every tool call/i,
  /state is fully reconstructible from the audit log/i,
  /Impossible: agents do not write to the audit log directly/i,
  /Later modification breaks the chain/i,
  /provenance on every privileged change/i,
  /safely share one computer/i,
  /daemon anchors receipts locally by default/i,
  /Agents can pay and charge per call/i,
  /Covenant agents pay for resources/i,
  /every privileged action is hash-chained/i,
  /audit roots are anchored on-chain where anyone can verify them/i,
  /a reckless action is refused before it reaches a wallet/i,
  /a malicious signature is rejected before it is signed/i,
  /every permitted action settles with a receipt/i,
  /every integration runs under the same signed permissions/i,
  /any compliant client or chain interoperates/i,
  /sap bridge ready/i,
  /root the SAP bridge anchors on-chain/i,
  /metaplex on-chain write confirmed/i,
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
      'explicit zero or invalid value is rejected',
      'never advertises or invokes write tools',
      'Historical AppData commitment format',
      'Inspecting a commitment',
      'Configured AppData data authority',
    ],
  ],
  [
    'README.md',
    [
      'controlled runtime dispatch',
      'local hash-chained audit',
      'recorded Covenant dispatch events',
      'does not prove that every host action passed through Covenant',
    ],
  ],
  [
    'docs/audit-integrity.md',
    [
      'Subsequent privileged mutations are classified',
      'explicit unaudited tier',
      'local tamper evidence, not public non-repudiation',
    ],
  ],
  [
    'landing/app/security/page.tsx',
    [
      'defined Covenant paths',
      'local hash-chain consistency',
      'does not mediate every host action',
      'standalone devnet reference',
    ],
  ],
  [
    'landing/app/partners/page.tsx',
    [
      'adapters for selected protocols',
      'Each adapter has its own boundary',
      'does not mean every external call',
    ],
  ],
  [
    'docs/getting-started.md',
    ['events emitted by audited', 'does not prove event completeness'],
  ],
  [
    'docs/agent-quickstart.md',
    ['verify its chain consistency', 'does not prove that every host action was mediated'],
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
    [
      'without separate operator approval',
      'not proof of human approval or W009',
      'share its filesystem',
      'outside Covenant',
      'operating-system isolation',
      'does not prove',
      'Unmatched later edits',
      'not universal proof of every privileged action',
    ],
  ],
  [
    'landing/app/guard/page.tsx',
    [
      'Covenant Evidence is a read-only evidence reader',
      'It does not approve or block payments',
      'a signature proves possession of a key',
      'bounded transfer',
    ],
  ],
  [
    'landing/app/page.tsx',
    [
      'The current release is local-first',
      'does not prove every command was mediated',
      'Settlement is currently',
    ],
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
      'production daemon does not currently make x402 payments',
      'durable prepayment reservation',
      'Attributing that key to Covenant requires',
      'Resource delivery and settlement are separate',
      'legacy compatibility names',
    ],
  ],
  [
    'landing/app/docs/x402/page.tsx',
    [
      'Delivery and settlement are separate',
      'do not assume no charge',
      'daemon-owned outbound payment is parked',
      'durable prepayment',
      'legacy compatibility names',
    ],
  ],
  [
    'docs/multichain-value-capture.md',
    [
      'configured key',
      'does not prove the underlying claim',
      'Cross-chain revenue routing into it is not implemented',
    ],
  ],
  [
    'landing/app/docs/multichain/page.tsx',
    ['configured address', 'real-world operator'],
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
    'docs/production-audit-solana-agent-trust.md',
    [
      'current witness does not prove',
      'authenticated peer can currently ask the daemon',
      'passing the daemon environment and host `HOME`',
      'Park daemon-owned Metaplex and SNS funded tools',
      'AceData and Hermes API calls as externally billed operations',
      'automatic retry remains removed',
      'violated the design rule expressed by W011',
    ],
  ],
  [
    'docs/runtime-sandbox-security.md',
    [
      'every `COVENANT_*` variable',
      'it is not a secret boundary',
      'External stdio MCP servers are launched from a cleared environment',
    ],
  ],
  [
    'docs/integrations/krexa.md',
    ['signer-authenticated heuristic', 'consumers still trust the score program'],
  ],
  [
    'docs/integrations/acedata.md',
    [
      'externally billed credential',
      'do not impose a monetary cap',
      'recorded cost is currently zero',
      'do not prove who caused the generation',
      'matching root and inclusion proof show only',
      'production daemon does not use a funding-key fallback',
      'COVENANT_ACEDATA_API_KEY',
      'not a supported daemon payment path or W009',
    ],
  ],
  [
    'docs/hyre-integration.md',
    [
      'daemon-owned payment integration is parked',
      'does not advertise',
      'LEGACY_OUTBOUND_PARKED',
      'durable reservation',
      'Requirements before re-enabling',
    ],
  ],
  [
    'docs/capabilities.md',
    [
      'Any authenticated peer can request a capability for itself',
      'not that an operator approved the action',
      'must not be treated as W009 approval',
      'this capability cannot currently authorize a payment',
      'a grant does not authorize funds to move',
      'unconditional parked boundary',
    ],
  ],
  [
    'ROADMAP.md',
    ['daemon-owned outbound payment is parked', 'durable prepayment reservation'],
  ],
  [
    'docs/ipc-and-http-gateway.md',
    [
      'recorded capability-family actions',
      'local hash-chain consistency',
      'does not prove event completeness',
      'it cannot currently drive a payment',
      'LEGACY_OUTBOUND_PARKED',
      'before config parsing, signer construction, or network I/O',
    ],
  ],
  [
    'agent-os/crates/covenantd/src/main.rs',
    ['writes_parked = true', 'sap bridge configuration loaded'],
  ],
  [
    'agent-os/crates/covenantd/src/lib.rs',
    [
      'configuration only; neither implies a reachable write path',
      'against the current local audit root',
      'consistency, not completeness or resistance',
    ],
  ],
  [
    'agent-os/crates/covenantd/src/metaplex.rs',
    [
      'production daemon exposes only read observations',
      'Production daemon dispatch never calls this method',
      'security isolation boundary',
    ],
  ],
  [
    'agent-os/crates/covenant-ipc/src/lib.rs',
    [
      'automatic SAP anchor is parked',
      'configuration state, not write reachability',
      'daemon refuses it before',
    ],
  ],
  [
    'landing/app/blog/covenant-payai/page.tsx',
    ['That inference was too', 'Those observations are not jobs or reputation'],
  ],
  ['landing/app/onepager/page.tsx', ['This document is outdated and must not be distributed']],
  [
    'services/x402-seller-base/src/attest.ts',
    [
      'not independently establish Covenant',
      'attribution or claim truth',
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
      'not an independent trust anchor',
      'A handler error is not evidence that settlement failed',
    ],
  ],
  [
    'services/indexer/src/verified.rs',
    [
      'Covenant-observed endpoint probes',
      'consumer must pin the expected key',
      'not an independent attestation authority',
      'does not establish publisher identity',
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
    'landing/app/docs/capabilities/page.tsx',
    [
      'any authenticated peer can request',
      'not operator authorization',
      'authority to spend funds',
      'Known scope-validated namespaces',
      'do not receive',
      'grant-time schema validation',
    ],
  ],
  [
    'landing/app/docs/concepts/page.tsx',
    [
      'without separate operator approval',
      'not proof of operator authorization',
      'Coverage is operation-specific',
      'Generic tool calls emit completion audit events',
      'not fully reconstructible',
    ],
  ],
  [
    'landing/app/docs/security/page.tsx',
    [
      'Do not treat the current capability grant as operator approval',
      'external controls for funds',
      'structural checks',
      'are not operator authorization',
      'the daemon cannot',
      'mediate direct host activity',
      'same-user agent can modify',
      'the local audit record',
    ],
  ],
  [
    'landing/app/docs/audit/page.tsx',
    ['Coverage is operation-specific', 'local system of record for those recorded events'],
  ],
  [
    'landing/app/docs/agent-quickstart/page.tsx',
    ['verify its chain consistency', 'does not prove that every host action was mediated'],
  ],
  [
    'landing/app/faq/page.tsx',
    ['under a trusted-local boundary', 'daemon-driven onchain receipt lifecycle is not production'],
  ],
  [
    'landing/app/blog/covenant-is-now-multichain/page.tsx',
    ['operates a Base x402-v2', 'production daemon does not currently make Base x402 payments'],
  ],
  [
    'landing/app/agents/[asset]/page.tsx',
    ['Historical AppData commitment', 'Configured data authority'],
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
      'configured EVM key signed the exact bytes',
    ],
  ],
  [
    'agent-os/crates/covenant-evm-signer/src/lib.rs',
    [
      'experimental format utility',
      'does not verify',
      'does not identify the publisher',
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
    ['Not wired to publication', 'signatures bind bytes to a key, not claims or identity'],
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
    [
      'observe.record',
      'observe.agent_records',
      'not claim verification',
      'production daemon filters and',
      'Production daemon dispatch does not supply an implementation',
    ],
  ],
  [
    'agent-os/crates/covenant-hyre/src/lib.rs',
    ['production daemon does not advertise', 'legacy daemon adapter is parked'],
  ],
  [
    'agent-os/crates/covenant-hyre/src/tools.rs',
    ['production daemon', 'does not advertise', 'authorization'],
  ],
  [
    'agent-os/crates/covenant-hyre/src/x402.rs',
    ['production daemon does not call this loop', 'refuses redirects'],
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
