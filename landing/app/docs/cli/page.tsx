import Link from "next/link";

export const metadata = {
  title: "Command-line interface",
  description:
    "Every covenant subcommand, with arguments and exit codes.",
};

export default function CliPage() {
  return (
    <>
      <h1>Command-line interface</h1>
      <p>
        The <code>covenant</code> CLI talks to a running daemon over the
        Unix socket at <code>$COVENANT_HOME/sock</code>. Every subcommand
        is a single round-trip; the CLI does no caching and holds no
        state of its own.
      </p>

      <h2>Synopsis</h2>
      <pre>
        <code>{`covenant <subcommand> [args]

  intent <text>                      Submit an intent and print the result.
  ping                               Check the daemon is responsive.

  memory recent [--tier T] [-n N]    List recent memory records.
  memory search <query>
        [--tier T] [-n N]            Cosine-similarity search via embeddings.
  memory purge [--tier T]
        (--before-ms M
         | --older-than-ms D)        Delete records older than the threshold.

  capabilities recent [-n N]         List recent capability tokens.
  capabilities grant <action>        Sign and persist a new capability.
  capabilities revoke <signature-b58>
                                     Tombstone a previously granted token.

  receipts recent [-n N]             List recent settlement receipts.

  verify [--window N]                Cross-check audit log vs other state.

  ignore check <text>                Report whether text matches the
                                     .covenantignore rules.

  tools list                         List registered tools.
  tools call <name> [--args <json>]  Invoke a registered tool.
`}</code>
      </pre>

      <h2>Conventions</h2>
      <ul>
        <li>
          <code>--tier T</code> accepts <code>working</code>,{" "}
          <code>episodic</code>, or <code>longterm</code> (also{" "}
          <code>long-term</code>, <code>long_term</code>).
        </li>
        <li>
          <code>-n N</code> sets the result count. Defaults to 10.
        </li>
        <li>
          Time values are Unix milliseconds.{" "}
          <code>--before-ms</code> is an absolute epoch;{" "}
          <code>--older-than-ms</code> is a relative offset (now minus
          duration).
        </li>
        <li>
          Daemon errors print to stderr and exit non-zero.
        </li>
      </ul>

      <h2>Exit codes</h2>
      <table>
        <thead>
          <tr>
            <th>Code</th>
            <th>Meaning</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>0</code>
            </td>
            <td>Success.</td>
          </tr>
          <tr>
            <td>
              <code>1</code>
            </td>
            <td>
              The daemon returned an error response, or a downstream
              call (e.g. socket connect) failed.
            </td>
          </tr>
          <tr>
            <td>
              <code>2</code>
            </td>
            <td>
              Usage error — bad subcommand, missing argument, malformed
              flag value.
            </td>
          </tr>
        </tbody>
      </table>

      <p>
        <code>covenant verify</code> is the one exception: a non-zero exit
        signals drift between state files even when the call itself
        succeeded.
      </p>

      <h2>Examples</h2>

      <h3>Submit an intent</h3>
      <pre>
        <code>{`$ covenant intent "summarise recent work on agent memory"
phase 0 echo (no agent matched): summarise recent work on agent memory`}</code>
      </pre>

      <h3>Inspect recent memory</h3>
      <pre>
        <code>{`$ covenant memory recent -n 3
[1714938191234] working: phase 0 echo (no agent matched): summarise...
[1714938018993] working: phase 0 echo (no agent matched): index the...
[1714937883112] working: phase 0 echo (no agent matched): list any open...`}</code>
      </pre>

      <h3>Semantic search across all tiers</h3>
      <pre>
        <code>{`$ covenant memory search "agent memory" -n 5
# (records ordered by cosine similarity, descending)`}</code>
      </pre>

      <h3>Grant and revoke a capability</h3>
      <pre>
        <code>{`$ covenant capabilities grant tool.web_search
granted tool.web_search to user@local
signature: 4qXP...8tF1

$ covenant capabilities revoke 4qXP...8tF1
revoked 4qXP...8tF1 (removed=true)`}</code>
      </pre>

      <h3>Verify state</h3>
      <pre>
        <code>{`$ covenant verify --window 100
window: 100
checks:
  memory ↔ audit       PASS  0 memory orphan(s), 0 audit orphan(s)
  capability ↔ audit   PASS  0 capabilit(ies) without matching grant audit event
  memory ↔ receipts    PASS  20 memory record(s) vs 20 receipt(s); diff = 0
orphans_total: 0`}</code>
      </pre>

      <h3>Invoke a tool</h3>
      <pre>
        <code>{`$ covenant capabilities grant tool.call.echo
$ covenant tools call echo --args '{"text":"hello"}'
hello`}</code>
      </pre>

      <h2>Environment</h2>
      <table>
        <thead>
          <tr>
            <th>Variable</th>
            <th>Purpose</th>
            <th>Default</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>COVENANT_HOME</code>
            </td>
            <td>
              Root of all on-disk state — socket, identity, memory,
              receipts, audit, capabilities, agents.
            </td>
            <td>
              <code>$HOME/.covenant</code>
            </td>
          </tr>
          <tr>
            <td>
              <code>COVENANT_HTTP_PORT</code>
            </td>
            <td>
              Port the daemon binds for the HTTP gateway. The CLI
              itself does not use HTTP.
            </td>
            <td>
              <code>8421</code>
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/http-api">HTTP API</Link> — same surface
          over HTTP, suitable for browser-facing UIs.
        </li>
        <li>
          <Link href="/ipc">Local IPC</Link> — the wire protocol
          underneath the CLI.
        </li>
        <li>
          <Link href="/capabilities">Capability tokens</Link> —
          what <code>capabilities grant</code>/<code>revoke</code>{" "}
          actually mints.
        </li>
      </ul>
    </>
  );
}
