# GitHub Marketplace listing: Mizuki the Mech

Copy for the listing form at
`https://github.com/settings/apps/mizuki-the-mech-core/marketplace`.
The App is already public at `https://github.com/apps/mizuki-the-mech-core`.

Free listings have no minimum installation count. The plan below is free **on
GitHub**, because GitHub never bills for Mizuki: a job is paid per call in USDC
on Solana, directly to the service.

## Name

Mizuki the Mech

## Short description

_(one line, shown in search results)_

Fixed-price maintenance for your open issues. You get a pull request that passes your checks, or your money back.

## Categories

Primary: **Code review** — secondary: **Continuous integration**

## Long description

Mizuki takes one issue you have authorized and returns a pull request that passes your repository's own checks.

The price is fixed before anything is paid, and pinned to the commit it was quoted against. If Mizuki cannot deliver a change that passes, the payment is refunded in full. There is no subscription, no seat count, and no prepaid balance.

You stay in control of what it may touch. Mizuki only reads issues carrying a label you apply, so nothing is picked up that you did not hand it. It opens a pull request like any other contributor, and your review and merge rules apply unchanged.

**How it works**

1. Install the App on a public repository and label an issue `mizuki:authorized`.
2. Ask for a quote. Mizuki reads the issue and returns a fixed price with the work it will attempt.
3. Pay the quote in USDC on Solana. A sponsored fee payer covers the network fee, so you need only the stablecoin.
4. Mizuki opens a pull request and runs your repository's validation command against it. If it does not pass, you are refunded.

**Pricing**

Two classes, quoted per issue: Micro at 2 USDC for changes up to three files, and Standard at 10 USDC for up to ten. The quote names the class before you pay.

**For agents**

Mizuki is callable without a browser. `npx -y mizuki-mcp` exposes it to any MCP client, `mizuki-agent-tools` covers LangChain, and the service is listed in Coinbase's x402 Bazaar so an agent can discover and pay for it unattended.

## Pricing plan

| Field       | Value                                                                                                                                                             |
| ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Plan name   | Free                                                                                                                                                              |
| Price       | $0 on GitHub                                                                                                                                                      |
| Description | Installing and authorizing issues is free. Each job is quoted and paid per issue in USDC on Solana, direct to the service. Unsuccessful work is refunded in full. |

## Required links

| Field             | URL                                                 |
| ----------------- | --------------------------------------------------- |
| Homepage          | https://mizuki.opencovenant.org                     |
| Privacy policy    | https://mizuki.opencovenant.org/privacy             |
| Terms of service  | https://mizuki.opencovenant.org/terms               |
| Support           | https://mizuki.opencovenant.org/support             |
| Security          | https://mizuki.opencovenant.org/security            |
| Status / evidence | https://mizuki.opencovenant.org/api/mizuki/v1/proof |

## Assets

| Asset        | Source                                             | Requirement             |
| ------------ | -------------------------------------------------- | ----------------------- |
| Logo         | `apps/mizuki-web/public/mizuki-icon-512.png`       | 200x200 minimum, square |
| Feature card | `feature-card.png` in this directory               | 1280x640                |
| Screenshots  | the job room mid-run, and a delivered pull request | 1280x800 or larger      |

## Before submitting

- Accept the GitHub Marketplace Developer Agreement.
- Confirm publisher contact details on the organization.
- A free listing does not require publisher verification. That is only needed to charge through GitHub, which Mizuki does not do.
