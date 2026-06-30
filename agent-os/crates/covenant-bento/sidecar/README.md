# bento-guard sidecar

A one-shot Node process the Covenant daemon spawns to run Bento's `protect()`
through the real `@bentoguard/sdk`. The daemon pipes a screen request as JSON
on stdin and reads a normalized `AnalysisResult` on stdout. The agent key never
reaches the daemon: the daemon forwards a keypair file path, and only this
process reads the key from it.

`bs58` is used directly (to load a Solana keypair JSON file); `@solana/web3.js`
and `tweetnacl` are the SDK's peer dependencies, per Bento's quickstart.

## Setup

```
cd crates/covenant-bento/sidecar
npm install            # package-lock.json is committed; this process holds the key
```

The script already ships executable. Register an agent at `app.bentoguard.xyz`,
put its key in a file (a base58 string, or a Solana keypair JSON array), then
point the daemon at the script:

```
COVENANT_BENTO_ENABLED=1
COVENANT_BENTO_PROTECT_ENABLED=1
COVENANT_BENTO_GUARD_BINARY=/abs/path/to/bento-guard.mjs
COVENANT_BENTO_KEYPAIR_PATH=/abs/path/to/agent-key
```

The daemon forwards the keypair path (as `AGENT_WALLET_KEYPAIR_PATH`) and `PATH`
into the sidecar's otherwise-cleared environment. It never reads, holds, or logs
the key value.

## Contract

Input on stdin: `{ "intent": string, "agentAddress"?: string, "timeoutMs"?: number }`.

Output on stdout: `{ "recommendation": "ALLOW"|"BLOCKED"|"ESCALATED", "riskScore": int 0-100,
"reasoning": string, "actionId"?, "approveUrl"?, "blockUrl"?, "reviewUrl"?, "timestamp"? }`.

Any error exits non-zero (with the key redacted from stderr), which the daemon
treats as fail-closed: the action is blocked.

`protect()` needs a Bento-registered agent (self-serve at `app.bentoguard.xyz`).
The full path is live-validated on devnet through the daemon and the real SDK: a
benign intent returns ALLOW with a moderate risk score, a malicious one returns
BLOCKED with a high score and a real reasoning string.
