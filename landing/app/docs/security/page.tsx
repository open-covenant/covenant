import Link from "next/link";

export const metadata = {
  title: "Security model",
  description:
    "Trust boundaries, threat model, defaults, and operator responsibilities.",
};

export default function SecurityPage() {
  return (
    <>
      <h1>Security model</h1>
      <p>
        Covenant is a local-first daemon. It assumes the operator owns
        the machine and trusts their own user account; it does not
        defend against an attacker who already has the operator&apos;s
        shell. Within that assumption it offers strong guarantees
        about what agents and tools can do — capability tokens with
        ed25519 signatures, append-only audit, hard enforcement at
        dispatch.
      </p>

      <h2>Trust boundaries</h2>

      <table>
        <thead>
          <tr>
            <th>Boundary</th>
            <th>Defended</th>
            <th>Mechanism</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>Operator → daemon</td>
            <td>Yes (within the OS user model)</td>
            <td>
              File permissions on{" "}
              <code>$COVENANT_HOME</code>, identity key at{" "}
              <code>0600</code>, Unix-socket access controlled by the
              filesystem.
            </td>
          </tr>
          <tr>
            <td>Daemon → agent</td>
            <td>Yes</td>
            <td>
              Capability checks at dispatch, wall-clock budget,
              JSON-line stdin/stdout protocol with no out-of-band
              channel.
            </td>
          </tr>
          <tr>
            <td>Agent → daemon</td>
            <td>Partial</td>
            <td>
              Agents only see what the daemon hands them on stdin.
              They cannot mutate state directly; everything goes
              through the daemon.
            </td>
          </tr>
          <tr>
            <td>External MCP server</td>
            <td>No</td>
            <td>
              Servers run with the operator&apos;s privileges. The
              daemon gates <em>invocation</em> via{" "}
              <code>tool.call.&lt;name&gt;</code>; it cannot govern
              what a malicious server does once invoked.
            </td>
          </tr>
          <tr>
            <td>Network → daemon</td>
            <td>Yes (via loopback)</td>
            <td>
              The HTTP gateway binds <code>127.0.0.1</code> only.
              Anything beyond that requires explicit operator
              configuration.
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Threat model</h2>

      <h3>What Covenant defends against</h3>
      <ul>
        <li>
          A registered agent attempting an action it does not have
          capabilities for. Hard rejection at dispatch; audit row
          recorded.
        </li>
        <li>
          A registered agent exceeding its CPU budget. The runtime
          kills the process; the dispatch returns an error.
        </li>
        <li>
          A registered agent injecting forged audit entries.
          Impossible — agents do not write to the audit log directly.
        </li>
        <li>
          Out-of-band edits to capability files producing tokens
          without a matching grant audit event.{" "}
          <code>covenant verify</code> reports the mismatch.
        </li>
        <li>
          A registered agent fabricating a result for a different
          agent&apos;s intent. Each <code>AgentResult</code> is
          captured by the runtime with a known matched agent;
          attribution is set by the daemon, not the agent.
        </li>
      </ul>

      <h3>What Covenant does not defend against</h3>
      <ul>
        <li>
          An attacker with the operator&apos;s shell. They can read
          the identity key, sign capabilities, edit files, restart
          the daemon. Outside the scope of the daemon.
        </li>
        <li>
          A malicious external MCP server granted{" "}
          <code>tool.call.&lt;name&gt;</code>. The capability gates
          who can <em>invoke</em> the tool; the tool itself runs as
          the operator and can do anything the operator can do.
        </li>
        <li>
          Side-channel resource consumption. Memory budgets are
          advisory until a sandboxed runtime (gVisor, Firecracker)
          enforces them.
        </li>
        <li>
          Network-level mitm to the on-chain RPC. Use a trusted RPC
          provider and verify TLS; the daemon does not pin
          certificates.
        </li>
      </ul>

      <h2>Defaults that matter</h2>

      <ul>
        <li>
          Identity key written at mode <code>0600</code>; refusal to
          start if the key file is world-readable.
        </li>
        <li>
          HTTP gateway bound to <code>127.0.0.1</code>. Override
          deliberately if you need remote access — and gate the
          surface behind your own auth proxy when you do.
        </li>
        <li>
          Default agent network policy is{" "}
          <code>outbound-https-only</code>; opt into{" "}
          <code>full</code> only when the agent needs it.
        </li>
        <li>
          The default <code>.covenantignore</code> seeds rules for
          common credential filenames so that intents whose text
          mentions e.g. <code>id_rsa</code> are short-circuited and
          never written to memory.
        </li>
      </ul>

      <h2>Operator responsibilities</h2>

      <ul>
        <li>
          Treat <code>$COVENANT_HOME</code> as you would treat your
          shell&apos;s dotfiles. Back it up; restrict access; do not
          check it into version control.
        </li>
        <li>
          Vet every external MCP server before configuring it.
          Prefer the most narrowly-scoped server for the job.
        </li>
        <li>
          Audit capability grants. If an agent suddenly needs a new
          action, that should be a deliberate decision, not a
          drive-by grant.
        </li>
        <li>
          Read the audit log periodically. The cross-references{" "}
          <code>covenant verify</code> checks are a smoke test, not
          a substitute for occasionally tailing{" "}
          <code>events.jsonl</code> by hand.
        </li>
        <li>
          Rotate the identity key on a schedule that matches your
          security posture. Re-issuing the keypair invalidates every
          signed capability written under the old key — plan for the
          re-grant.
        </li>
      </ul>

      <h2>Reporting a vulnerability</h2>
      <p>
        Use GitHub&apos;s private advisory flow at{" "}
        <a
          href="https://github.com/open-covenant/covenant/security/advisories/new"
          target="_blank"
          rel="noopener noreferrer"
        >
          github.com/open-covenant/covenant/security/advisories/new
        </a>
        , or email <code>security@opencovenant.org</code>. Do not open
        a public issue for anything that could compromise keys,
        capability tokens, audit-log integrity, or on-chain funds.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/identity">Identity and keys</Link> — the
          ed25519 keypair behind every capability.
        </li>
        <li>
          <Link href="/capabilities">Capability tokens</Link> —
          how dispatch is gated.
        </li>
        <li>
          <Link href="/audit">Audit log</Link> — the system&apos;s
          ground truth and how to read it.
        </li>
      </ul>
    </>
  );
}
