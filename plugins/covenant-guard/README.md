# covenant-guard plugin

Adds the covguard guard to Claude Code (and Codex): a `/guard` command and an
MCP server that reads the guard's state, plus a session-start reminder when a
run isn't guarded.

The plugin is the front door; the enforcement lives in the `covguard` binary.
Install it once:

```
curl -fsSL https://opencovenant.org/guard/install.sh | sh
covguard doctor
```

Then run any agent under it:

```
covguard run --budget 10 -- claude -p "fix the flaky tests" --dangerously-skip-permissions
```

covguard caps the spend from outside the agent's process, sandboxes execution to
the workspace, and writes a signed receipt you can verify. The plugin's `/guard`
command and its MCP tools (`guard_status`, `guard_receipts`, `guard_verify`) let
the agent read those receipts; they do not run agents themselves.

- Claude Code manifest: `.claude-plugin/plugin.json`
- Codex manifest: `.codex-plugin/plugin.json` (experimental — the Codex host
  wiring is built but not yet live-verified)

Apache-2.0 · https://opencovenant.org/guard
