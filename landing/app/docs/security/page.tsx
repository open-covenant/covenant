import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("security", "Security model", 'Trust boundaries, threat model, defaults, and operator responsibilities.');

export default function SecurityPage() {
  return (
    <>
      <h1>Security model</h1>
      <p>
        Covenant is a local-first daemon. The model assumes the operator
        owns the host and trusts their own user account; it does not defend
        against an adversary that has already obtained shell access as the
        operator. Within that boundary, Covenant provides hard guarantees
        over Covenant-mediated actions: ed25519-signed capability tokens,
        an append-only audit log, and enforcement at dispatch. Trusted-local
        subprocess execution is not process isolation. The runtime crate has
        an initial Linux gVisor runner, daemon-selectable backend
        configuration, and opt-in live Linux coverage; repeatable CI
        provisioning and broader policy enforcement are still required before
        production sandbox claims.
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
            <td>Daemon → trusted-local agent</td>
            <td>Partial</td>
            <td>
              Capability checks at dispatch, wall-clock budget,
              hard-preempt on projected overshoot, JSON-line
              stdin/stdout protocol, and daemon-owned attribution.
              This is not a sandbox against hostile code.
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
              Servers execute with the operator&apos;s privileges. The
              daemon gates <em>invocation</em> via{" "}
              <code>tool.call.&lt;name&gt;</code> but cannot constrain
              the behavior of a malicious server post-invocation.
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
          <tr>
            <td>Sandbox-required agent → host</td>
            <td>Partial</td>
            <td>
              Manifests can require <code>linux-gvisor</code>, the
              trusted-local runner fails closed instead of downgrading, the
              daemon supports the <code>linux-gvisor</code> backend, and
              opt-in live Linux coverage runs on sandbox-runtime path PRs
              via <code>gvisor-live.yml</code>. Promoting that workflow to a
              required check and broadening sandbox policy enforcement
              remain planned.
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
          A registered agent exceeding its CPU budget. A periodic
          projection tick walks an in-memory subprocess tracker and
          preempts via <code>SIGTERM</code>/grace/<code>SIGKILL</code>{" "}
          when <code>project_overshoot</code> flags an in-flight
          process. The wall-clock kill at the agent-declared{" "}
          <code>cpu_ms_per_task</code> is the backstop. Either path
          emits <code>BudgetPreempted</code> or{" "}
          <code>BudgetPreemptFailed</code> in the audit log, and the
          dispatch returns an error. A daemon crash between the
          projection decision and the signal dispatch can leave an
          orphan subprocess; recovery on restart is a documented gap.
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
          Host filesystem, environment, or network access by a malicious
          trusted-local subprocess. The current subprocess runner is for
          first-party or otherwise trusted local automation only.
        </li>
        <li>
          Network-level mitm to the on-chain RPC. Use a trusted RPC
          provider and verify TLS; the daemon does not pin
          certificates.
        </li>
      </ul>

      <h2>Defaults</h2>

      <ul>
        <li>
          Identity key written at mode <code>0600</code>; the daemon
          refuses to start if the key file is world-readable.
        </li>
        <li>
          HTTP gateway bound to <code>127.0.0.1</code>. Remote access
          requires deliberate configuration and an authentication proxy
          in front of the gateway.
        </li>
        <li>
          Default agent network policy is{" "}
          <code>outbound-https-only</code>;{" "}
          <code>full</code> network access requires explicit opt-in per
          agent.
        </li>
        <li>
          Agents declaring <code>sandbox.required</code> fail closed on the
          trusted-local runner. Covenant must not silently downgrade a
          sandbox-required agent to subprocess execution.
        </li>
        <li>
          The default <code>.covenantignore</code> seeds rules for
          common credential filenames; intents whose text references
          paths such as <code>id_rsa</code> are short-circuited and
          never persisted to memory.
        </li>
      </ul>

      <h2>Operator responsibilities</h2>

      <ul>
        <li>
          <code>$COVENANT_HOME</code> contains identity material,
          capability tokens, and the full audit log. Restrict access,
          back it up, and exclude it from version control.
        </li>
        <li>
          Review external MCP servers prior to configuration. Prefer the
          server with the narrowest scope appropriate to the task.
        </li>
        <li>
          Treat capability grants as deliberate authorization decisions.
          Each grant is recorded in the audit log; new grants should be
          reviewed and justified rather than issued ad hoc.
        </li>
        <li>
          Inspect the audit log on a regular cadence.{" "}
          <code>covenant verify</code> provides drift detection across
          the cross-references but does not replace direct inspection
          of <code>events.jsonl</code>.
        </li>
        <li>
          Rotate the identity key on a schedule consistent with the
          deployment&apos;s security requirements. Re-issuing the keypair
          invalidates every capability signed under the prior key; plan
          the corresponding re-grant in advance of rotation.
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
        <li>
          <Link href="/gvisor-live-runner">Linux gVisor runner</Link> —
          repeatable setup for the opt-in sandbox live test.
        </li>
      </ul>
    </>
  );
}
