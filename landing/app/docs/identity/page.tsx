import Link from "next/link";

export const metadata = {
  title: "Identity and keys",
  description:
    "ed25519 identity, on-disk persistence, signing helpers, and rotation.",
};

export default function IdentityPage() {
  return (
    <>
      <h1>Identity and keys</h1>
      <p>
        Every Covenant install owns a single ed25519 keypair. The same
        key signs capability grants, signs Solana settlement
        transactions, and fronts the daemon&apos;s issuer field on
        audit events and memory records. There is no second key
        system.
      </p>

      <h2>Persistence</h2>
      <p>
        The keypair is stored as the raw 32-byte seed at{" "}
        <code>$COVENANT_HOME/identity/local.key</code>. The daemon
        writes the file with mode <code>0600</code> (owner
        read/write, no group, no other). On startup the daemon
        refuses to load a key file with broader permissions; tighten
        it with <code>chmod 0600</code> if you ever see this error.
      </p>

      <p>
        The matching public key is derivable from the seed and is
        attached to every <code>AgentId</code> the daemon emits as
        the issuer.
      </p>

      <h2>The <code>AgentId</code> shape</h2>
      <pre>
        <code>{`AgentId {
  display: "user@local",          // human-readable label
  pubkey:  [u8; 32]               // raw ed25519 pubkey
}`}</code>
      </pre>

      <p>
        The display half is set on first daemon start (default{" "}
        <code>user@&lt;hostname&gt;</code>). The pubkey half is
        derived from the seed and is the cryptographically meaningful
        identifier — display strings can collide; pubkeys cannot.
      </p>

      <h2>Signing helpers</h2>
      <p>
        The <code>covenant-identity</code> crate exposes:
      </p>
      <ul>
        <li>
          <code>LocalIdentity::generate(display)</code> — fresh
          ed25519 keypair plus display string.
        </li>
        <li>
          <code>LocalIdentity::load_or_create(path, display)</code> —
          loads from disk if present, else generates and persists.
        </li>
        <li>
          <code>LocalIdentity::sign(&amp;self, message)</code> —
          signs arbitrary bytes.
        </li>
        <li>
          <code>verify_with_pubkey(pubkey, message, signature)</code>{" "}
          — read-side verification; returns{" "}
          <code>Result&lt;(), SignatureError&gt;</code>.
        </li>
        <li>
          <code>verifying_key_from_bytes(pubkey)</code> — converts a
          32-byte pubkey to an <code>ed25519_dalek::VerifyingKey</code>.
        </li>
      </ul>

      <p>
        The signing helpers are deliberately small: they do not
        prescribe a canonical encoding for the message. The capability
        layer (<code>covenant-permissions</code>) supplies its own
        encoder and feeds the signing helpers the resulting bytes.
      </p>

      <h2>Same key, two roles</h2>
      <p>
        The same ed25519 keypair is used to:
      </p>
      <ul>
        <li>
          sign capability tokens (<code>SignedCapability</code>);
        </li>
        <li>
          sign Solana settlement transactions when the daemon flushes
          receipts on-chain;
        </li>
        <li>
          appear as the <code>issuer</code> on audit events and the{" "}
          <code>owner</code> on memory records.
        </li>
      </ul>
      <p>
        Reusing the key across roles keeps the operator&apos;s mental
        model small. The cost is that compromise of the key
        compromises all three; the benefit is that there is only one
        thing to back up, rotate, and protect.
      </p>

      <h2>Rotation</h2>
      <p>
        Rotation is deliberate and disruptive. Re-issuing the keypair
        invalidates every signed capability written under the old key
        — verifying a token after rotation will fail because the
        expected granter pubkey no longer matches the daemon&apos;s
        live key. Plan for the re-grant when you rotate.
      </p>

      <p>The procedure:</p>

      <ol>
        <li>Stop the daemon.</li>
        <li>
          Move the existing key file aside (or delete it once you are
          sure no other state references it).
        </li>
        <li>
          Start the daemon. It will generate a fresh key on first
          run.
        </li>
        <li>
          Re-grant every capability your agents need. The audit log
          will record both the rotation (implicitly, via the absence
          of the old issuer in subsequent events) and the new
          grants.
        </li>
        <li>
          Update any external systems that bound to the old pubkey
          (Solana program authority records, third-party
          integrations).
        </li>
      </ol>

      <h2>Keys at scale</h2>
      <p>
        Today Covenant runs one keypair per machine. A future shape
        will allow multiple subordinate keys for delegated agents
        (each subordinate signed by the root), so an agent compromise
        does not require a full root rotation. Until then, treat the
        single keypair the way you would treat your shell&apos;s SSH
        key.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/capabilities">Capability tokens</Link> —
          everything that depends on the signing helpers.
        </li>
        <li>
          <Link href="/settlement">Settlement</Link> — the
          on-chain side that signs with the same key.
        </li>
        <li>
          <Link href="/security">Security model</Link> — the
          file-permissions, threat-model context.
        </li>
      </ul>
    </>
  );
}
