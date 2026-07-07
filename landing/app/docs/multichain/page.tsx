import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "multichain",
  "Multi-chain trust",
  "Covenant's trust layer is live on Base while $CVNT stays a single Solana mint. Only signed data crosses chains, verifiable with a plain ecrecover.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function MultichainPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Multi-chain trust</h1>
      <p>
        Covenant&apos;s trust layer is live on Base. <strong>$CVNT stays a single
        Solana mint.</strong> What crosses to Base is proof, not value: signed
        statements about an agent that any EVM contract can check for a fraction
        of a cent.
      </p>

      <h2>The problem: going multi-chain usually means moving the token</h2>
      <p>
        Crypto has lost billions to bridge hacks, and almost every one traces to
        the same decision: a project expands to a new chain by bridging or
        wrapping its token, and the wrapper becomes the honeypot. The value sits
        on the bridge, and the bridge is what gets drained.
      </p>
      <p>
        An agent trust layer does not need to move value across chains. It needs
        to move facts: who an agent is, what it has done, what it has staked.
        Facts are signatures. Signatures do not need a bridge.
      </p>

      <h2>The design: bridge the trust, not the token</h2>
      <p>
        Covenant keeps one canonical identity and one hash-chained audit history
        authoritative on Solana. Base holds verifiable projections of them, never
        the source of truth and never the token. An agent&apos;s identity,
        reputation, provenance, and bonds each cross as a signed statement a
        contract verifies on arrival.
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
          <strong>Value stays chain-local.</strong> Per-call payment is USDC on
          the chain of the call, never $CVNT.
        </li>
      </ul>

      <h2>The keystone: one identity an EVM can check for ~3k gas</h2>
      <p>
        Verifying a Solana ed25519 signature on an EVM costs about 2 million gas,
        enough to make cross-chain verification pointless. So Covenant gives each
        identity a second key on the secp256k1 curve, which an EVM checks with a
        plain <code>ecrecover</code> at around 3 thousand gas, and binds it
        bidirectionally to the canonical ed25519 identity, anchored on Solana.
      </p>
      <p>
        Every cross-chain artifact is signed by that issuer key. A consumer
        recovers the signer with one <code>ecrecover</code> and checks it against
        Covenant&apos;s published issuer address. One agent, one record, provable
        on both chains, for a fraction of a cent.
      </p>

      <h2>Live on Base mainnet</h2>
      <p>
        The trust primitives are live on Base mainnet, the agent is registered
        and discoverable, and one real Covenant record already verifies against
        Base&apos;s own attestation stack. The entire surface went through an
        internal adversarial security audit and hardening before any of it
        shipped.
      </p>
      <ul>
        <li>
          <strong>ERC-8004 identity.</strong> The agent is registered in the
          ERC-8004 Identity Registry, so EVM tooling discovers a Covenant
          identity whose record points back to Solana.
        </li>
        <li>
          <strong>Issuer identity.</strong> Covenant&apos;s secp256k1 Base
          identity, bound to the Solana identity. The key every attestation
          recovers to.
        </li>
        <li>
          <strong>Reputation schema.</strong> An EAS schema for
          non-transferable, audit-derived reputation, bound to a Solana anchor so
          it cannot be laundered onto a sellable token.
        </li>
        <li>
          <strong>Bond verifier.</strong> A contract that authenticates a
          Covenant USDC bond receipt with one <code>ecrecover</code>. No bridge,
          no light client, no Solana read on the path.
        </li>
        <li>
          <strong>Provenance record.</strong> A real audit-root attestation that
          verifies under Base&apos;s EAS domain and recovers to the issuer key.
        </li>
      </ul>

      <h2>Payments: chain-local USDC, never the token</h2>
      <p>
        Covenant already runs x402 paid endpoints on Solana mainnet, returning
        real USDC payment challenges per call. The same model carries to Base:
        per-call value is chain-local USDC, signed through an EIP-3009
        authorization so an agent needs only USDC, not the gas token and not
        $CVNT. Base-native paid endpoints are the next step; the payment rail and
        the token boundary are already settled.
      </p>

      <h2>A name that resolves to the identity</h2>
      <p>
        <strong>opencovenant.eth resolves to the Covenant identity.</strong> An
        ENS lookup of the name on Ethereum returns the same Solana identity the
        ERC-8004 record points at, so the name, the onchain registration, and the
        canonical identity all agree. Per-agent names extend it:{" "}
        <code>&lt;agent&gt;.agents.opencovenant.eth</code> resolves to each
        agent&apos;s Solana identity through a CCIP-Read gateway, so any
        ENS-aware tool can look up a Covenant agent by name. The name is a
        pointer; the identity stays authoritative on Solana.
      </p>

      <h2>The invariant: $CVNT never leaves Solana</h2>
      <p>
        $CVNT is one mint, one market. It is never bridged, wrapped, or minted on
        any other chain, and no per-call fee is ever denominated in it. This is
        enforced in code, checked on every build. Multi-chain grows the surface
        that consumes Covenant&apos;s trust without ever fragmenting the token or
        the trust root.
      </p>

      <h2>Verify it yourself</h2>
      <p>Address sheet, Base mainnet (chain 8453):</p>
      <pre>
        <code>{`issuer identity (attestor)   0x186953d5b4A290f8f53b8377cb38EDA75D664211
bond receipt verifier        0xBee387DD4A2fF215d6f997E5DA464C92285BCb6e
reputation schema UID (EAS)  0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39
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
          <strong>The reputation schema is registered.</strong>{" "}
          <code>getSchema(uid)</code> on the EAS registry returns the Covenant
          schema.
        </li>
        <li>
          <strong>The provenance record recovers to the issuer.</strong>{" "}
          Recompute the EAS digest and run <code>ecrecover</code>; it returns the
          issuer address, not ours to assert.
        </li>
        <li>
          <strong>The relayer holds no trust.</strong> It pays gas and submits
          transactions. It signs no attestation, so compromising it forges
          nothing. The trust key never sends a transaction.
        </li>
        <li>
          <strong>$CVNT is not on Base.</strong> There is no such token to find,
          by construction.
        </li>
      </ul>

      <h2>What comes next</h2>
      <ul>
        <li>
          <strong>Base x402 endpoints.</strong> Base-native paid endpoints over
          the EIP-3009 rail already built.
        </li>
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
        <strong>Not trust by claim. Trust checked against the key.</strong> The
        token and its market stay whole on Solana; the proof travels everywhere
        else.
      </p>
    </>
  );
}
