# Where Mizuki is listed, and what is still open

## Listed

| Directory                | Entry                              | How it got there                                                                               |
| ------------------------ | ---------------------------------- | ---------------------------------------------------------------------------------------------- |
| MCP registry (Anthropic) | `org.opencovenant/mizuki`          | `mcp-publisher`, DNS-verified namespace                                                        |
| npm                      | `mizuki-mcp`, `mizuki-agent-tools` | `npm publish`                                                                                  |
| x402 Bazaar (Coinbase)   | the assess endpoint                | indexed after a payment settled through CDP's facilitator ([[reference_x402_bazaar_indexing]]) |

## Prepared, needs an account to finish

These directories rank on a public repository and a README, which is why the
server now lives at `github.com/mizuki0x/mizuki-mcp` rather than only inside
this monorepo. Each still needs a signed-in submission.

| Directory | Where                                   | What to paste                                                     |
| --------- | --------------------------------------- | ----------------------------------------------------------------- |
| Glama     | `glama.ai/mcp/servers` → Add Server     | repo `https://github.com/mizuki0x/mizuki-mcp`                     |
| Smithery  | `smithery.ai` → connect the GitHub repo | the repo carries `smithery.yaml`, so it needs no further input    |
| mcp.so    | `mcp.so/submit`                         | repo URL and npm package `mizuki-mcp`                             |
| PulseMCP  | `pulsemcp.com` → submit                 | repo URL, npm package, homepage `https://mizuki.opencovenant.org` |

Glama also has an API at `glama.ai/api/mcp/v1/servers`, which needs a key from
`glama.ai/settings/api`. With that key the submission can be scripted instead.

Shared values for any of these forms:

- **Name:** Mizuki
- **Package:** `mizuki-mcp` on npm, run with `npx -y mizuki-mcp`
- **Repository:** https://github.com/mizuki0x/mizuki-mcp
- **Homepage:** https://mizuki.opencovenant.org
- **Description:** Fixed-price maintenance for public GitHub issues, paid in USDC on Solana.
- **License:** Apache-2.0
- **Tools:** 13, listed in `smithery.yaml`

## GitHub Marketplace

The App is already public at `https://github.com/apps/mizuki-the-mech-core`
with one installation. It is not on Marketplace, which is a separate listing
and the thing maintainers actually browse.

A **free** listing has no minimum installation count; the 100-install rule
applies only to apps that charge through GitHub, which Mizuki does not. Copy and
assets are in `marketplace/`.
