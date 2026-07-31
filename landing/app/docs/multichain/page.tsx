import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "multichain",
  "Multi-chain signed evidence",
  "Selected Covenant registrations and signed statements are readable on Base while $CVNT stays on Solana. ecrecover verifies a configured signing address, not publisher identity or claim truth.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function MultichainPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Multi-chain signed evidence</h1>
      <p>
        Selected Covenant registrations, schemas, and signed statements are
        readable on Base. <strong>$CVNT stays a single Solana mint.</strong>{" "}
        What crosses is data signed under configured keys, not independent proof
        of publisher identity or that an underlying identity, score, delivery,
        or bond claim is true.
      </p>

      <h2>The problem: going multi-chain usually means moving the token</h2>
      <p>
        Crypto has lost billions to bridge hacks, and almost every one traces to
        the same decision: a project expands to a new chain by bridging or
        wrapping its token, and the wrapper becomes the honeypot. The value sits
        on the bridge, and the bridge is what gets drained.
      </p>
      <p>
        Signed evidence does not require moving the protocol token. A signature
        can bind bytes to a configured key without bridging value; it cannot
        identify the publisher or turn a claim into a fact.
      </p>

      <h2>The design: project signed evidence, not the token</h2>
      <p>
        Covenant treats configured Solana identity and audit-root records as the
        canonical references for this projection. Base holds signed statements,
        never the token. Registrations, audit-root statements, caller-supplied
        score projections, provenance commitments, and bond-receipt claims can
        each be encoded as signed bytes. The score utility is not wired to
        publication and does not verify its supplied Solana reference or
        derivation. Consumers still decide what the publisher and claim mean for
        their own policy.
      </p>
      <ul>
        <li>
          <strong>Only signed data crosses.</strong> Never the token, never a
          wrapped representation, never custody.
        </li>
        <li>
          <strong>Solana stays canonical.</strong> Other chains carry
          projections; the source of truth does not move.
        </li>
        <li>
          <strong>Value stays chain-local.</strong> The Solana/Base evidence
          surfaces described here use local USDC, never $CVNT. Other integrations
          can use their own settlement assets.
        </li>
      </ul>

      <h2>The keystone: an issuer key an EVM can authenticate</h2>
      <p>
        Verifying a Solana ed25519 signature on an EVM costs about 2 million
        gas, enough to make cross-chain verification pointless. So Covenant
        gives each identity a second key on the secp256k1 curve, which an EVM
        checks with a plain <code>ecrecover</code> at around 3 thousand gas.
        Covenant records associate that issuer with the configured Solana
        identity.
      </p>
      <p>
        Selected cross-chain artifacts are signed by that issuer key. A consumer
        recovers the signer with one <code>ecrecover</code> and checks it
        against the configured address recorded for this projection. That proves
        the configured address signed the bytes, not the claim, publisher
        identity, or real-world operator.
      </p>

      <h2>Live on Base mainnet</h2>
      <p>
        The listed contracts, registration, schema, and signed record are
        deployed on Base mainnet. Their addresses and signatures are checkable;
        deployment and internal review do not establish the truth of a signed
        claim or production use of the unexercised paths.
      </p>
      <ul>
        <li>
          <strong>ERC-8004 registration.</strong> A registry entry points to the
          configured Covenant and Solana identifiers. The entry does not prove
          who operates an agent.
        </li>
        <li>
          <strong>Issuer key.</strong> Covenant&apos;s configured secp256k1
          publisher key, associated by Covenant records with a Solana address.
          Selected signed statements recover to this key.
        </li>
        <li>
          <strong>Score schema.</strong> A registered EAS schema for an score
          projection. Registration defines a wire shape; no onchain score has
          been written, the utility accepts caller-supplied fields, and the
          schema does not prove reputation.
        </li>
        <li>
          <strong>Bond verifier.</strong> A contract that checks a USDC bond
          statement against a configured address with one <code>ecrecover</code>.
          No bridge, no light client, no Solana read on the path.
        </li>
        <li>
          <strong>Provenance record.</strong> A signed audit-root statement
          whose EAS digest recovers to the configured issuer key.
        </li>
      </ul>

      <h2>Payments: chain-local USDC, never the token</h2>
      <p>
        Covenant runs x402 paid endpoints on Solana mainnet, returning real USDC
        payment challenges per call. The same rail extends to Base:{" "}
        <code>covenant-x402</code> signs EIP-3009 USDC authorizations so an agent
        needs only USDC, not the gas token and not $CVNT, settling gaslessly
        through Coinbase&apos;s x402 facilitator. The Base seller is live,
        returning EIP-3009 USDC challenges on Base mainnet; the token boundary
        holds either way, $CVNT never crosses.
      </p>

      <h2>A name that resolves to the configured identifier</h2>
      <p>
        <strong>
          opencovenant.eth resolves to the configured Covenant identifier.
        </strong>{" "}
        An ENS lookup returns the same Solana address referenced by the ERC-8004
        entry. This is consistent pointer data, not proof of a real-world
        identity. Per-agent names extend it:{" "}
        <code>&lt;agent&gt;.agents.opencovenant.eth</code> resolves to each
        configured Solana address through a CCIP-Read gateway, so an ENS-aware
        tool can resolve the published pointer. The name does not prove who
        controls that address or operates the agent.
      </p>

      <h2>The invariant: $CVNT never leaves Solana</h2>
      <p>
        $CVNT is one mint, one market. It is never bridged, wrapped, or minted
        on any other chain, and no per-call fee is intended to be denominated in
        it. A repository guard catches known mint literals and named bridge
        patterns; it is a tripwire, not proof that every future integration
        preserves the invariant. Multi-chain expands the evidence and payment
        surfaces without requiring a second $CVNT mint.
      </p>

      <h2>Verifiable on Base mainnet</h2>
      <p>Address sheet, Base mainnet (chain 8453):</p>
      <pre>
        <code>{`issuer identity (attestor)   0x186953d5b4A290f8f53b8377cb38EDA75D664211
bond receipt verifier        0xBee387DD4A2fF215d6f997E5DA464C92285BCb6e
score schema UID (EAS)       0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39
EAS registry / predeploy     0x4200...0020 (schemas) . 0x4200...0021 (attestations)
relayer (no authority)       0x5fA1d0C0bfFE257a20027C523093F941834f5D66
$CVNT mint (Solana only)     2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump`}</code>
      </pre>
      <ul>
        <li>
          <strong>The bond verifier trusts the issuer.</strong>{" "}
          <code>TRUSTED_ATTESTOR()</code> reads back the issuer address, and{" "}
          <code>USDC()</code> reads back Base&apos;s native Circle USDC. Anyone
          with an RPC can call them.
        </li>
        <li>
          <strong>The score schema is registered.</strong>{" "}
          <code>getSchema(uid)</code> on the EAS registry returns the Covenant
          schema.
        </li>
        <li>
          <strong>The provenance record recovers to the issuer.</strong>{" "}
          Recompute the EAS digest and run <code>ecrecover</code>; it returns the
          issuer address, not ours to assert.
        </li>
        <li>
          <strong>The relayer is not the statement issuer.</strong> It pays gas
          and submits transactions. If it lacks the issuer key, compromising it
          cannot forge that issuer&apos;s signature, though it can censor or
          submit other transactions available to its account.
        </li>
        <li>
          <strong>$CVNT is not on Base.</strong> There is no such token to find,
          by construction.
        </li>
      </ul>

      <h2>What comes next</h2>
      <ul>
        <li>
          <strong>Cross-chain enforcement.</strong> Bonds slashable on Solana
          from an EVM-proven event, with an objective fault definition and a
          challenge window.
        </li>
        <li>
          <strong>More L2s.</strong> OP-Stack EAS predeploys make each new L2
          nearly free. Base is first.
        </li>
      </ul>

      <p>
        <strong>A key check proves possession of a key, not publisher identity or
        a claim.</strong> The token and its market stay on Solana; selected signed
        evidence can be consumed elsewhere under local policy only after the
        consumer establishes its own trusted key mapping.
      </p>
    </>
  );
}
