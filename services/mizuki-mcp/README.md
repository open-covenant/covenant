# mizuki-mcp

Mizuki takes one authorized issue from a public GitHub repository and returns a pull request that passes that repository's own checks, or refunds the quoted amount. This package exposes it to any MCP client.

Payment is an exact USDC transfer on Solana mainnet with a sponsored fee payer, so a caller needs USDC but not SOL. There is no account to create.

## Use it

```json
{
  "mcpServers": {
    "mizuki": { "command": "npx", "args": ["-y", "mizuki-mcp"] }
  }
}
```

Quoting an issue and reading bounties, treasury, and capability records work with no configuration. Set `MIZUKI_API_TOKEN` to also list your linked repositories, run a preflight, or check whether a quote already reserved a job.

## Tools

| Tool                                                | Purpose                                                     |
| --------------------------------------------------- | ----------------------------------------------------------- |
| `mizuki_quote`                                      | Fixed price and x402 payment requirements for an issue      |
| `mizuki_submit`                                     | Submit a quoted job with a wallet-signed payment            |
| `mizuki_status`                                     | Delivery, pull request, validation, or refund state         |
| `mizuki_preflight`                                  | Repository, authorization, and scope checks without quoting |
| `mizuki_payment_status`                             | Whether a quote and idempotency key already reserved a job  |
| `mizuki_repositories`                               | Repositories linked to the authenticated maintainer         |
| `mizuki_repository_readiness`                       | Current readiness for one linked repository                 |
| `mizuki_repository_issues`                          | Bounded maintenance candidates for a linked repository      |
| `mizuki_bounties` / `mizuki_bounty`                 | Public bounties opened after eligible refunds               |
| `mizuki_treasury`                                   | Refund reserve status                                       |
| `mizuki_capabilities` / `mizuki_capability_handoff` | Proposed capability changes and their required evidence     |

`mizuki_payment_status` never requests a signature or submits a payment, so it is safe to call when a submission's outcome is unknown.

## Configuration

| Variable                | Default                                      |
| ----------------------- | -------------------------------------------- |
| `MIZUKI_API_URL`        | `https://mizuki.opencovenant.org/api/mizuki` |
| `MIZUKI_API_TOKEN`      | unset — public tools still work              |
| `MIZUKI_MCP_TIMEOUT_MS` | SDK default                                  |

Apache-2.0. Source: [open-covenant/covenant](https://github.com/open-covenant/covenant/tree/main/services/mizuki-mcp).
