import Link from "next/link";

export const metadata = {
  title: "MCP integration",
  description:
    "Tool trait, native tools, registry, and the stdio JSON-RPC transport for external MCP servers.",
};

export default function McpPage() {
  return (
    <>
      <h1>MCP integration</h1>
      <p>
        Covenant integrates with the Model Context Protocol (MCP) — the
        protocol agents use to discover and call external tools. The
        same <code>Tool</code> trait backs native Rust implementations
        and external MCP servers reached over JSON-RPC, so the daemon
        and any client can list and invoke tools through a single
        surface.
      </p>

      <h2>Wire shapes</h2>
      <p>
        Covenant&apos;s MCP types follow the public MCP shapes verbatim
        so a tool catalogue is portable between Covenant and any other
        MCP-aware host.
      </p>

      <pre>
        <code>{`ToolSpec {
  name:        "echo",
  description: "Returns the provided text argument verbatim.",
  inputSchema: {
    "type": "object",
    "properties": { "text": { "type": "string" } },
    "required": ["text"],
    "additionalProperties": false
  }
}

ToolCallResult {
  content: [ Content::Text { text: "…" }, Content::Json { value: {…} } ],
  isError: false
}`}</code>
      </pre>

      <h2>Native tools</h2>
      <p>
        Native tools are Rust types implementing the{" "}
        <code>Tool</code> trait and registered with{" "}
        <code>ToolRegistry</code> at daemon startup. The registry
        exposes <code>list_specs</code> for catalogue queries and{" "}
        <code>call(name, arguments)</code> for invocation.
      </p>

      <p>
        Two native tools ship with the daemon out of the box:
      </p>

      <ul>
        <li>
          <code>echo</code> — returns its <code>text</code> argument
          verbatim. Validates the schema (rejects missing or
          non-string <code>text</code>); useful as a smoke test for
          the registry plumbing and as a reference for argument
          validation.
        </li>
        <li>
          <code>clock</code> — no-arg, returns{" "}
          <code>{"{ \"epoch_ms\": <u64> }"}</code>. Useful for
          confirming the daemon is alive without granting any
          capability.
        </li>
      </ul>

      <h2>Calling a tool</h2>
      <pre>
        <code>{`# Grant the per-tool capability first.
covenant capabilities grant tool.call.echo

# Then call the tool with JSON arguments.
covenant tools call echo --args '{"text":"hi"}'
# → hi`}</code>
      </pre>

      <p>
        Every <code>tools/call</code> requires the capability{" "}
        <code>tool.call.&lt;name&gt;</code> in the issuer&apos;s active
        set. Capability checks are audited via the same{" "}
        <code>CapabilityCheck</code> event used by agent dispatch; the
        audit row carries an <code>agent_id</code> of{" "}
        <code>tool:&lt;name&gt;</code> so operators can distinguish
        tool calls from agent dispatches at a glance.
      </p>

      <h2>External MCP servers</h2>
      <p>
        External MCP servers extend the catalogue without modifying
        the daemon. Configure them in <code>~/.covenant/secrets.toml</code>:
      </p>

      <pre>
        <code>{`[[mcp.server]]
name    = "filesystem"
command = "npx"
args    = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/notes"]
env     = { LOG_LEVEL = "info" }`}</code>
      </pre>

      <p>
        On startup the daemon spawns each configured server, performs
        the MCP handshake (<code>initialize</code> →{" "}
        <code>notifications/initialized</code> →{" "}
        <code>tools/list</code>), and merges the discovered tools into
        the same <code>ToolRegistry</code> the native tools live in.
        Per-server failures are fail-soft: a server that fails to
        start or returns an error during handshake is logged and
        skipped without preventing the daemon from coming up.
      </p>

      <h3>Transport</h3>
      <p>
        Covenant speaks line-delimited JSON-RPC 2.0 over the spawned
        process&apos;s stdin/stdout. Each request carries a stable id;
        the daemon correlates responses by id and ignores anything
        unrecognised. The child process is started with{" "}
        <code>kill_on_drop(true)</code>, so an unclean daemon exit
        also stops the child — there are no orphan tool processes.
      </p>

      <h2>Security</h2>
      <p>
        External MCP servers run as the operator. They have access to
        whatever the daemon&apos;s user account has access to. The
        capability gate (<code>tool.call.&lt;name&gt;</code>) governs
        who can <em>invoke</em> a tool through the daemon, but it
        cannot govern what the tool itself does on the operator&apos;s
        machine once invoked. Vet servers before configuring them and
        prefer the most narrowly-scoped server for the job.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/capabilities">Capability tokens</Link> —
          the gate on every <code>tools/call</code>.
        </li>
        <li>
          <Link href="/audit">Audit log</Link> — where every
          capability check and every tool call lands.
        </li>
        <li>
          <Link href="/cli">CLI</Link> — <code>tools list</code>{" "}
          and <code>tools call</code>.
        </li>
      </ul>
    </>
  );
}
