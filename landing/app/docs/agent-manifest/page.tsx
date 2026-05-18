import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("agent-manifest", "Agent manifest", 'Schema and validation rules for agent.toml — the file every Covenant agent ships.');

export default function AgentManifestPage() {
  return (
    <>
      <h1>Agent manifest</h1>
      <p>
        Each Covenant agent is a subdirectory of{" "}
        <code>$COVENANT_HOME/agents/</code> containing an{" "}
        <code>agent.toml</code> manifest. The router walks{" "}
        <code>agents/</code>, picks up each subdirectory&apos;s{" "}
        <code>agent.toml</code>, and resolves{" "}
        <code>agent.entry</code> against that package directory.
        Flat <code>*.toml</code> files at the top of{" "}
        <code>agents/</code> are silently skipped. The manifest
        declares the agent&apos;s identity, runtime, package-relative
        executable path, required capabilities, resource budget,
        sandbox requirement, and optional settlement configuration.
      </p>

      <h2>Example</h2>
      <pre>
        <code>{`[agent]
id      = "research"
name    = "research"
version = "0.1.0"
runtime = "rust-bin"
entry   = "research"

[capabilities]
required = ["tool.web_search"]
optional = ["memory.write"]

[resources]
cpu_ms_per_task = 30000
memory_mb       = 512
disk_mb         = 100
network         = "outbound-https-only"

[sandbox]
required   = true
backend    = "linux-gvisor"
filesystem = "read-only-package"

[settlement]
budget_credits_per_hour = 1000
priority                = "normal"`}</code>
      </pre>

      <h2>Schema</h2>

      <h3>
        <code>[agent]</code>
      </h3>
      <table>
        <thead>
          <tr>
            <th>Field</th>
            <th>Type</th>
            <th>Required</th>
            <th>Notes</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>id</code>
            </td>
            <td>string</td>
            <td>yes</td>
            <td>
              Stable identifier. ASCII{" "}
              <code>[A-Za-z0-9_.-]+</code>; <code>@</code> is rejected
              at parse time. Used as the routing key and the audit-log{" "}
              <code>matched_agent</code> value. The daemon synthesises
              <code>&lt;id&gt;@agent</code> as the agent&apos;s{" "}
              <code>AgentId.display</code> for budget keying.
            </td>
          </tr>
          <tr>
            <td>
              <code>name</code>
            </td>
            <td>string</td>
            <td>yes</td>
            <td>Display name; appears in CLI listings.</td>
          </tr>
          <tr>
            <td>
              <code>version</code>
            </td>
            <td>string</td>
            <td>yes</td>
            <td>SemVer recommended.</td>
          </tr>
          <tr>
            <td>
              <code>runtime</code>
            </td>
            <td>enum</td>
            <td>yes</td>
            <td>
              <code>rust-bin</code>, <code>python3</code>,{" "}
              <code>node</code>, or <code>hermes</code>. The first three
              exec <code>entry</code> as a subprocess; <code>hermes</code>{" "}
              delegates to a configured Hermes HTTP endpoint and ignores{" "}
              <code>entry</code>.
            </td>
          </tr>
          <tr>
            <td>
              <code>entry</code>
            </td>
            <td>string</td>
            <td>yes</td>
            <td>
              Path to the binary (for <code>rust-bin</code>) or the
              entry script (for <code>python3</code> /{" "}
              <code>node</code>). Resolved relative to the manifest&apos;s
              parent directory unless absolute. Ignored when{" "}
              <code>runtime = &quot;hermes&quot;</code>.
            </td>
          </tr>
        </tbody>
      </table>

      <h3>
        <code>[capabilities]</code>
      </h3>
      <table>
        <thead>
          <tr>
            <th>Field</th>
            <th>Type</th>
            <th>Default</th>
            <th>Notes</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>required</code>
            </td>
            <td>list of action strings</td>
            <td>
              <code>[]</code>
            </td>
            <td>
              Every action in this list must be present in the
              issuer&apos;s active capability set or the dispatch is
              rejected.
            </td>
          </tr>
          <tr>
            <td>
              <code>optional</code>
            </td>
            <td>list of action strings</td>
            <td>
              <code>[]</code>
            </td>
            <td>
              Recorded for visibility but not enforced.
            </td>
          </tr>
        </tbody>
      </table>

      <p>
        Action strings live in reserved namespaces:{" "}
        <code>intent.</code>, <code>memory.</code>,{" "}
        <code>identity.</code>, <code>tool.</code>,{" "}
        <code>agent.</code>. The daemon validates that{" "}
        <code>required</code> and <code>optional</code> actions sit in
        one of these namespaces.
      </p>

      <h3>
        <code>[resources]</code>
      </h3>
      <table>
        <thead>
          <tr>
            <th>Field</th>
            <th>Type</th>
            <th>Default</th>
            <th>Notes</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>cpu_ms_per_task</code>
            </td>
            <td>u64 milliseconds</td>
            <td>
              <code>30000</code>
            </td>
            <td>
              CPU budget. The runtime preempts the process when the
              projection tick flags projected overshoot and kills it
              at the elapsed cap as the backstop.
            </td>
          </tr>
          <tr>
            <td>
              <code>memory_mb</code>
            </td>
            <td>u64 MiB</td>
            <td>
              <code>512</code>
            </td>
            <td>Advisory today; enforced by sandboxed runtimes.</td>
          </tr>
          <tr>
            <td>
              <code>disk_mb</code>
            </td>
            <td>u64 MiB</td>
            <td>
              <code>100</code>
            </td>
            <td>Advisory today.</td>
          </tr>
          <tr>
            <td>
              <code>network</code>
            </td>
            <td>enum</td>
            <td>
              <code>outbound-https-only</code>
            </td>
            <td>
              <code>off</code>, <code>outbound-https-only</code>, or{" "}
              <code>full</code>.
            </td>
          </tr>
        </tbody>
      </table>

      <h3>
        <code>[sandbox]</code>
      </h3>
      <table>
        <thead>
          <tr>
            <th>Field</th>
            <th>Type</th>
            <th>Default</th>
            <th>Notes</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>required</code>
            </td>
            <td>bool</td>
            <td>
              <code>false</code>
            </td>
            <td>
              When true, the manifest must name a sandbox-grade backend.
              Trusted-local subprocess execution is rejected.
            </td>
          </tr>
          <tr>
            <td>
              <code>backend</code>
            </td>
            <td>enum</td>
            <td>
              <code>trusted-local</code>
            </td>
            <td>
              <code>trusted-local</code> or <code>linux-gvisor</code>.
              The runtime crate has a gVisor runner and the daemon supports
              the <code>linux-gvisor</code> backend; live Linux CI coverage
              runs on sandbox-runtime path PRs via{" "}
              <code>gvisor-live.yml</code>. Promoting that workflow to a
              required check and broadening sandbox policy enforcement remain
              planned.
            </td>
          </tr>
          <tr>
            <td>
              <code>filesystem</code>
            </td>
            <td>enum</td>
            <td>
              <code>read-only-package</code>
            </td>
            <td>
              <code>read-only-package</code>, <code>ephemeral</code>, or{" "}
              <code>host</code>. The field is parsed now and enforced by
              sandboxed runtimes.
            </td>
          </tr>
        </tbody>
      </table>

      <h3>
        <code>[settlement]</code>
      </h3>
      <table>
        <thead>
          <tr>
            <th>Field</th>
            <th>Type</th>
            <th>Default</th>
            <th>Notes</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>budget_credits_per_hour</code>
            </td>
            <td>u64</td>
            <td>
              <code>0</code>
            </td>
            <td>
              Soft cap; tolerated as <code>0</code> until budget and
              settlement enforcement are configured for the agent.
            </td>
          </tr>
          <tr>
            <td>
              <code>priority</code>
            </td>
            <td>enum</td>
            <td>
              <code>normal</code>
            </td>
            <td>
              <code>low</code>, <code>normal</code>, <code>high</code>.
            </td>
          </tr>
        </tbody>
      </table>

      <h3>
        <code>[hermes]</code>
      </h3>
      <p>
        Optional block consulted when <code>runtime = &quot;hermes&quot;</code>;
        ignored otherwise. Hermes manages its tool allowlist
        server-side today, so both fields are documentary — they pin
        the contract the agent author expects, surface in operator
        listings, and act as the enforcement seam once Hermes exposes
        per-run controls.
      </p>
      <table>
        <thead>
          <tr>
            <th>Field</th>
            <th>Type</th>
            <th>Default</th>
            <th>Notes</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>tools_allowed</code>
            </td>
            <td>list of strings</td>
            <td>
              <code>[]</code>
            </td>
            <td>
              Tools the agent expects the run to invoke. Names match
              Hermes&apos;s tool-registry slugs (e.g.{" "}
              <code>terminal</code>, <code>read_file</code>,{" "}
              <code>web</code>). Operators can spot a manifest that
              over-asks before granting capabilities.
            </td>
          </tr>
          <tr>
            <td>
              <code>approval_policy</code>
            </td>
            <td>enum</td>
            <td>
              <code>operator-prompt</code>
            </td>
            <td>
              How the runner should handle Hermes{" "}
              <code>approval.request</code> events when no operator is
              online. <code>operator-prompt</code> blocks until an
              operator answers via the console;{" "}
              <code>auto-deny</code> short-circuits to a denied
              response; <code>auto-once</code> accepts a single
              approval and stops. Reserved — runtime enforcement lands
              once the Hermes runner learns to post{" "}
              <code>/v1/runs/&#123;id&#125;/approval</code>.
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Runtime contract</h2>
      <p>
        At dispatch, the runtime spawns the agent according to{" "}
        <code>runtime</code> and <code>entry</code>:
      </p>

      <pre>
        <code>{`runtime = "rust-bin"   →   exec entry directly
runtime = "python3"    →   exec python3 entry
runtime = "node"       →   exec node    entry
runtime = "hermes"     →   POST to a configured Hermes HTTP endpoint`}</code>
      </pre>

      <p>
        The first three runtimes communicate over stdin/stdout. The
        agent reads exactly one JSON line from stdin:
      </p>

      <pre>
        <code>{`{
  "id":         "uuid",
  "text":       "the user's intent",
  "issuer":     { "display": "user@local", "pubkey": "…" },
  "issued_at":  1714938000000,
  "priority":   "normal",
  "parent":     null
}`}</code>
      </pre>

      <p>
        And writes exactly one JSON line to stdout:
      </p>

      <pre>
        <code>{`{
  "text":    "…",
  "sources": ["…"]
}`}</code>
      </pre>

      <p>
        Stderr output is captured by the daemon&apos;s tracing subsystem
        and surfaces in operator logs. The agent process must terminate
        within <code>resources.cpu_ms_per_task</code>; the runtime
        preempts the process via <code>SIGTERM</code>/grace/<code>SIGKILL</code>{" "}
        when the periodic projection tick observes that the process is
        on track to exceed the cap, and falls back to the wall-clock
        kill at the cap if preempt did not fire. Either path produces
        a dispatch error. Successful processes with malformed stdout
        are rejected as runtime failures, not accepted as successful
        dispatches.
        The current subprocess runner is trusted-local. If{" "}
        <code>sandbox.required</code> is true, it fails closed instead of
        silently running the agent without sandbox-grade isolation.
      </p>

      <h2>Validation rules</h2>
      <p>
        The manifest parser rejects manifests that:
      </p>
      <ul>
        <li>
          omit or leave empty any of <code>agent.id</code>,{" "}
          <code>agent.name</code>, or <code>agent.version</code>;
        </li>
        <li>
          omit or leave empty <code>agent.entry</code> when{" "}
          <code>runtime</code> is <code>python3</code>,{" "}
          <code>node</code>, or <code>rust-bin</code>{" "}
          (<code>runtime = &quot;hermes&quot;</code> ignores{" "}
          <code>agent.entry</code> entirely);
        </li>
        <li>
          contain characters in <code>agent.id</code> outside
          ASCII <code>[A-Za-z0-9_.-]+</code> — <code>agent.id</code>{" "}
          flows into the daemon&apos;s synthesised{" "}
          <code>AgentId</code> display and round-trips through that
          charset filter on every JSONL replay;
        </li>
        <li>
          declare an <code>agent.entry</code> that is absolute or
          contains <code>..</code> / root / drive components for a
          subprocess runtime — entries must be relative paths inside
          the agent package directory;
        </li>
        <li>
          declare a <code>required</code> or <code>optional</code>{" "}
          capability action outside the reserved namespaces;
        </li>
        <li>
          set <code>sandbox.required = true</code> while keeping{" "}
          <code>{`backend = "trusted-local"`}</code>;
        </li>
        <li>
          fail to parse as TOML.
        </li>
      </ul>

      <p>
        Unknown top-level sections are tolerated for forward
        compatibility; subsequent releases may attach meaning to them.
      </p>

      <h2>Manifest discovery</h2>
      <p>
        On startup the daemon walks{" "}
        <code>$COVENANT_HOME/agents/</code> and loads each subdirectory
        that contains an <code>agent.toml</code>; flat{" "}
        <code>*.toml</code> files at the top of <code>agents/</code>{" "}
        and subdirectories without an <code>agent.toml</code> are
        silently skipped. The loader sorts the returned cards by{" "}
        <code>agent.id</code> so routing tie-breaking between
        equal-scoring agents is deterministic across hosts regardless
        of filesystem read order (APFS, ext4, and ntfs all return{" "}
        <code>read_dir</code> entries in different orders). Online
        registration is not supported; the daemon must be restarted
        after a new manifest is added. Existing manifests may be
        edited in place and are re-read on the next daemon start.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/concepts">Concepts</Link> — agents in
          context.
        </li>
        <li>
          <Link href="/capabilities">Capability tokens</Link> —
          what the <code>required</code> list refers to.
        </li>
        <li>
          <Link href="/security">Security model</Link> — what
          the resource budget protects.
        </li>
      </ul>
    </>
  );
}
