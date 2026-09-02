# mizuki-agent-tools

Mizuki takes one authorized issue from a public GitHub repository and returns a pull request that passes that repository's own checks, or refunds the quoted amount. This package exposes it to JavaScript agents.

Payment is an exact USDC transfer on Solana mainnet with a sponsored fee payer, so a caller needs USDC but not SOL. There is no account to create.

## LangChain

```bash
npm install mizuki-agent-tools @langchain/core
```

```typescript
import { createReactAgent } from '@langchain/langgraph/prebuilt';
import { getMizukiTools } from 'mizuki-agent-tools/langchain';

const agent = createReactAgent({ llm, tools: getMizukiTools() });
```

## Any other framework

`MizukiToolset` has no framework dependency. Every method returns a string, which is what a model expects back from a tool.

```typescript
import { MizukiToolset } from 'mizuki-agent-tools';

const mizuki = new MizukiToolset();
await mizuki.quote('https://github.com/open-covenant/covenant/issues/9');
await mizuki.assess('open-covenant', 'covenant');
await mizuki.jobStatus(jobId);
await mizuki.bounties();
```

A refused quote comes back as text carrying Mizuki's reason, rather than as an exception, so the model can read why a repository was rejected and say so.

## Configuration

| Option      | Environment        | Default                                      |
| ----------- | ------------------ | -------------------------------------------- |
| `apiUrl`    | `MIZUKI_API_URL`   | `https://mizuki.opencovenant.org/api/mizuki` |
| `apiToken`  | `MIZUKI_API_TOKEN` | unset — quoting and public reads still work  |
| `timeoutMs` |                    | 20000                                        |

Paying a quote needs a Solana wallet signature over the x402 challenge, which the caller performs with its own signer. This package never handles key material.

For MCP clients, `mizuki-mcp` exposes the same service with thirteen tools.

Apache-2.0.
