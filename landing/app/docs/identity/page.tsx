import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("identity", "Identity and keys", 'ed25519 identity, on-disk persistence, signing helpers, and rotation.');

export default function IdentityPage() {
  return (
    <>
      <h1>Identity and keys</h1>
      <p>
        Every Covenant install holds a single ed25519 keypair. The same
        key signs capability grants, signs Solana settlement transactions,
        and appears as the issuer on audit events and memory records.
        Covenant does not maintain a secondary key system.
      </p>

      <h2>Persistence</h2>
      <p>
        The keypair is stored as the raw 32-byte seed at{" "}
        <code>$COVENANT_HOME/identity/local.key</code> with mode{" "}
        <code>0600</code> (owner read/write only). The daemon refuses to
        start if the key file has broader permissions; restrict the file
        with <code>chmod 0600</code> to recover.
      </p>

      <p>
        The corresponding public key is derived from the seed and attached
        to every <code>AgentId</code> the daemon emits as issuer.
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
        <code>user@local</code>, kept generic so logs and commits do
        not leak the operator's hostname). The public key is derived
        from the seed and is the cryptographically meaningful identifier;
        display strings may collide, public keys cannot.
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
          <code>Result&lt;(), IdentityError&gt;</code>.
        </li>
        <li>
          <code>verifying_key_from_bytes(pubkey)</code> — converts a
          32-byte pubkey to an <code>ed25519_dalek::VerifyingKey</code>.
        </li>
      </ul>

      <p>
        The signing helpers are deliberately minimal and do not prescribe
        a canonical message encoding. The capability layer
        (<code>covenant-permissions</code>) supplies its own encoder and
        passes the resulting bytes to the signing helpers.
      </p>

      <h2>Roles</h2>
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
          appear as the <code>issuer</code> on audit events and as the{" "}
          <code>owner</code> on memory records.
        </li>
      </ul>
      <p>
        A single key across all three roles reduces the operator&apos;s
        key-management surface to one artifact. The trade-off is that
        compromise of the key affects all three roles; the deployment
        benefit is one artifact to back up, rotate, and protect.
      </p>

      <h2>Rotation</h2>
      <p>
        Rotation is deliberate and disruptive. Re-issuing the keypair
        invalidates every capability signed under the prior key:
        verification fails because the expected granter public key no
        longer matches the daemon&apos;s active key. Plan the
        corresponding re-grant in advance of rotation.
      </p>

      <p>Procedure:</p>

      <ol>
        <li>Stop the daemon.</li>
        <li>
          Move the existing key file aside, or delete it once no other
          state references the corresponding public key.
        </li>
        <li>
          Start the daemon. A new keypair is generated on first run.
        </li>
        <li>
          Re-grant the capabilities required by registered agents. The
          audit log records the rotation implicitly through the change
          of issuer on subsequent events, and explicitly through the new
          grant events.
        </li>
        <li>
          Update any external systems bound to the prior public key
          (Solana program authority records, third-party integrations).
        </li>
      </ol>

      <h2>Subordinate keys</h2>
      <p>
        The current implementation provisions one keypair per host. A
        later milestone introduces subordinate keys for delegated agents,
        each signed by the root key, so that compromise of an individual
        subordinate does not require a full root rotation. Until that
        capability ships, the single keypair carries the same protection
        requirements as a host SSH key.
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
