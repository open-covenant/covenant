import Link from "next/link";

export const metadata = {
  title: "Capability tokens",
  description:
    "Shape, canonical encoding, signing, verification, and revocation of Covenant capability tokens.",
};

export default function CapabilitiesPage() {
  return (
    <>
      <h1>Capability tokens</h1>
      <p>
        A capability token is a typed, signed authorization. It names an
        action, optionally scopes it to a JSON predicate, and is signed by
        the granter with their ed25519 key. Covenant enforces agent and
        tool authorization through capability tokens.
      </p>

      <h2>Shape</h2>
      <p>
        A token is a <code>SignedCapability</code> — a{" "}
        <code>Capability</code> wrapped with a 64-byte ed25519 signature
        encoded in Base58.
      </p>

      <pre>
        <code>{`SignedCapability {
  capability: Capability {
    subject:    AgentId,        // who this token authorises
    action:     "tool.web_search",
    scope:      JSON,           // free-form predicate; e.g. { "host": "example.com" }
    granted_by: AgentId,        // who issued the token
    expires_at: u64 | null,     // unix milliseconds; null = never
  },
  signature: [u8; 64],          // ed25519 over canonical_message(capability)
}`}</code>
      </pre>

      <h2>Action namespaces</h2>
      <p>
        Action strings live in a fixed set of reserved namespaces. The
        manifest parser and the daemon both validate that actions sit in
        one of these:
      </p>
      <ul>
        <li>
          <code>intent.</code> — actions over intent dispatch.
        </li>
        <li>
          <code>memory.</code> — actions over memory tiers.
        </li>
        <li>
          <code>identity.</code> — actions over the local identity.
        </li>
        <li>
          <code>tool.</code> — actions over the tool surface, including{" "}
          <code>tool.call.&lt;name&gt;</code> for individual tool
          dispatch.
        </li>
        <li>
          <code>agent.</code> — actions over agent registration and
          execution.
        </li>
      </ul>

      <p>
        Within a namespace, the action string is up to the operator.
        Examples in active use:{" "}
        <code>tool.web_search</code>, <code>tool.call.echo</code>,{" "}
        <code>memory.write</code>, <code>memory.read.longterm</code>.
      </p>

      <h2>Canonical encoding</h2>
      <p>
        Tokens are signed over a deterministic byte encoding, so any
        verifier produces the same bytes for the same capability. The
        encoder is explicit and length-prefixed and is located in a
        single function (<code>canonical_message</code>); it can be
        replaced without affecting the wire format.
      </p>

      <pre>
        <code>{`subject_pubkey       [32 bytes]
action_len_be        [4 bytes, u32 big-endian]
action               [action_len bytes]
scope_len_be         [4 bytes, u32 big-endian]
scope_json_bytes     [scope_len bytes — UTF-8 JSON]
granted_by_pubkey    [32 bytes]
expires_tag          [1 byte: 0 = none, 1 = present]
expires_at_be        [8 bytes, u64 big-endian; zero if expires_tag = 0]`}</code>
      </pre>

      <p>
        Concatenating these fields in this order produces the message
        the granter signs and the verifier reconstructs.
      </p>

      <h2>Granting a token</h2>
      <p>
        Granting is a daemon operation: the daemon constructs a{" "}
        <code>Capability</code>, encodes it canonically, signs it with
        the local identity key, and persists the signed capability to{" "}
        <code>$COVENANT_HOME/capabilities/granted.jsonl</code>. The CLI
        and HTTP gateway expose grant as{" "}
        <code>covenant capabilities grant</code> and{" "}
        <code>POST /capabilities/grant</code>.
      </p>

      <pre>
        <code>{`covenant capabilities grant tool.web_search
covenant capabilities grant tool.web_search --scope '{"host":"example.com"}'
covenant capabilities grant tool.web_search --expires-in 86400`}</code>
      </pre>

      <p>
        The grant audit event is written alongside the token, and the
        daemon returns the Base58 signature so the operator can revoke
        the token later.
      </p>

      <h2>Verification</h2>
      <p>
        Two helpers cover verification:
      </p>

      <ul>
        <li>
          <code>verify(signed)</code> — reconstructs the canonical
          message and verifies the signature against{" "}
          <code>capability.granted_by.pubkey</code>. Returns{" "}
          <code>BadSignature</code> on mismatch.
        </li>
        <li>
          <code>verify_with_clock(signed, now_ms)</code> — same as
          <code>verify</code>, plus rejects tokens whose{" "}
          <code>expires_at</code> is in the past relative to{" "}
          <code>now_ms</code>.
        </li>
      </ul>

      <p>
        At dispatch the daemon enumerates the issuer&apos;s active
        capabilities, discards any that fail{" "}
        <code>verify_with_clock</code>, computes the set of{" "}
        <code>action</code>s, and verifies that every required action
        from the matched agent&apos;s manifest is present. Missing
        actions are recorded in the audit event and the dispatch is
        rejected.
      </p>

      <h2>Revocation</h2>
      <p>
        Revocations are tombstones written to{" "}
        <code>$COVENANT_HOME/capabilities/revoked.jsonl</code>. Each
        tombstone references a token by its Base58 signature. The
        active set is the granted set with revocations subtracted —
        tokens can be re-granted after revocation, but the prior
        signature is permanently dead.
      </p>

      <pre>
        <code>{`covenant capabilities revoke 4qXP…8tF1
# → revoked 4qXP…8tF1 (removed=true)`}</code>
      </pre>

      <h2>Storage layout</h2>
      <table>
        <thead>
          <tr>
            <th>Path</th>
            <th>Format</th>
            <th>Append-only?</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>capabilities/granted.jsonl</code>
            </td>
            <td>
              One <code>SignedCapability</code> per line.
            </td>
            <td>Yes.</td>
          </tr>
          <tr>
            <td>
              <code>capabilities/revoked.jsonl</code>
            </td>
            <td>One revocation tombstone per line.</td>
            <td>Yes.</td>
          </tr>
        </tbody>
      </table>

      <h2>Security properties</h2>
      <ul>
        <li>
          <strong>Independently verifiable.</strong> Anyone with the
          granter&apos;s public key and the canonical encoder can verify
          a token without contacting the daemon.
        </li>
        <li>
          <strong>Tamper-evident.</strong> Mutating any field in the
          capability invalidates the signature.
        </li>
        <li>
          <strong>Time-bound.</strong> Tokens may carry an{" "}
          <code>expires_at</code>; verifiers check the wall clock.
        </li>
        <li>
          <strong>Revocable.</strong> The active set is{" "}
          <em>granted ⊝ revoked</em>; the daemon enforces this on
          every dispatch.
        </li>
        <li>
          <strong>Append-only on disk.</strong> Both grants and
          revocations are append-only logs. The audit log records
          every grant and every revocation; <code>covenant verify</code>{" "}
          flags any capability without a matching grant audit event.
        </li>
      </ul>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/identity">Identity and keys</Link> — the
          ed25519 keypair behind every signature.
        </li>
        <li>
          <Link href="/audit">Audit log</Link> — where grants,
          revocations, and capability checks are recorded.
        </li>
        <li>
          <Link href="/security">Security model</Link> — the
          assumptions the capability layer rests on.
        </li>
      </ul>
    </>
  );
}
