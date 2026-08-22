# Mizuki launch operating plan

The build window is 19 August–19 September 2026. Judging runs 20–30 September and the winner is selected 1 October. Registration, the X announcement, and tokenization are all hard gates by 19 September.

## Proof targets

| Proof                |  Minimum | Public evidence                                           |
| -------------------- | -------: | --------------------------------------------------------- |
| Paid jobs            |       10 | x402 settlement signatures and metrics dashboard          |
| PRs opened           |        7 | public GitHub PR URLs                                     |
| PRs merged           |        5 | GitHub merge state                                        |
| External maintainers |        3 | distinct App installations outside the internal repo list |
| Refund reliability   |     100% | refund signature for every failed settled job             |
| Unit economics       | positive | verified revenue and complete commercial-cost receipts    |

Jobs on operator-owned repositories prove integration, not demand. They do not count toward the three-external-maintainer target.

## First 72 hours

1. Deploy the API, E2B-backed coding gateway, GitHub App, durable store, refund signer, and public dashboard.
2. Run the UsePod benchmark over at least 12 historical micro/standard issues: four docs/config, four tests, four small bug fixes. Record completion, validation, review, latency, tokens, and the variable execution estimate. Keep provider billing adjustments, chain/facilitator, and infrastructure costs separate until each has a durable receipt.
3. Run a 2 USDC mainnet success canary on an internal public repository. Merge only after the full issue -> payment -> PR path is visible.
4. Force a post-payment reviewer rejection and publish the full 2 USDC refund transaction.
5. Post both canaries as short screen recordings with links to the issue, settlement, PR/refund, and live unit-economics dashboard with explicit cost coverage.

Do not start outreach until both canaries pass. Asking maintainers to be the first production test is a trust failure.

## Maintainer acquisition

Build a list of 30 public Solana and agent-tooling repositories with active maintainers and recent small issues. Exclude repositories where the only contact path is an issue; do not create promotional issues.

Contact in three cohorts of ten through public maintainer channels, ecosystem Discords, X, or existing relationships:

- Cohort A: Solana developer tools and SDK examples with docs/test issues.
- Cohort B: agent frameworks, MCP servers, and x402 tools with small reproducible bugs.
- Cohort C: hackathon teams that need maintenance on public launch repositories.

Offer the first job at the normal 2 USDC price and reimburse it only through the documented product refund path if delivery fails. Do not give free internal credits disguised as paid demand. Ask for one bounded issue, App installation, permission to show the flow, and honest feedback.

Track: contact, repository, issue candidate, App installed, quote issued, payment signature, PR, review result, merge, refund, maintainer feedback, and permission to use the clip. Follow up once after 48 hours; no repeated solicitation.

## Stream format — 15 minutes

- 0:00–0:45: “A public issue becomes a paid, validated PR or a full refund.”
- 0:45–5:30: live issue -> quote -> wallet payment -> job status -> PR. Keep a recorded external-repo run ready if the model or GitHub is slow.
- 5:30–7:30: forced-failure refund, with both settlement and refund signatures.
- 7:30–10:00: dashboard: paid jobs, external repositories, merges, refund ratio, variable execution estimate, omitted-cost categories, and gross-margin status.
- 10:00–12:00: architecture: credentials outside the model, pinned commit, sandbox, independent reviewer, repair cap.
- 12:00–13:30: why UsePod is load-bearing: both coder and reviewer consume marketplace inference; show route receipts.
- 13:30–15:00: $MIZUKI utility and roadmap, then questions.

Never spend the opening minutes on lore or token design. The product proof is the issue, payment, PR, and refund.

## Scope expansion gates

Stay on docs, tests, configuration, and small reproducible bugs until all are true:

- at least 20 paid jobs;
- delivery rate at least 80%;
- every failed settled job refunded;
- at least 10 merged PRs;
- positive aggregate gross margin after provider billing adjustments, sandbox, chain/facilitator, and infrastructure costs are durably recorded;
- no credential, workflow, or deployment boundary violations.

Features, private repositories, security-sensitive changes, and automatic issue discovery remain out of scope until those gates pass.
