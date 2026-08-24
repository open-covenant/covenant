# Launch plan through 19 September 2026

Traction is the release gate. Internal dogfooding is preparation, not evidence. The public target is at least 10 paid jobs, 7 PRs, 5 merges, 3 external maintainers, 100% successful refunds, and positive gross margin.

## Hard gates

| Gate                        | Minimum by 12 Sep | Launch minimum by 19 Sep | Stop condition                                                             |
| --------------------------- | ----------------: | -----------------------: | -------------------------------------------------------------------------- |
| Paid external jobs          |                10 |                       10 | Fewer than 7 by 7 Sep triggers daily founder-led onboarding.               |
| PRs opened                  |                 7 |                        7 | Below 70% job-to-PR conversion freezes scope expansion.                    |
| PRs merged                  |                 5 |                        5 | Fewer than 3 by 7 Sep shifts all outreach to merge-ready Micro issues.     |
| External maintainers        |                 3 |                        3 | Fewer than 2 by 31 Aug ends internal demo work until outreach recovers.    |
| Full payment refund success |              100% |                     100% | Any missed, short, wrong-recipient, or duplicate refund stops intake.      |
| Gross margin                |          Positive |                 Positive | Negative trailing margin pauses Standard jobs and re-benchmarks the route. |

No features, security-sensitive work, private repositories, or larger scopes enter the queue before every launch minimum is met.

## Calendar

### 22–24 August: core freeze and rehearsal

- Freeze paid-engine, refund/bounty, and signer interfaces except for correctness fixes.
- Complete mainnet route benchmarks on representative Micro and Standard issues; publish route, latency, token, and cost receipts.
- Run signer restart, RPC outage, database restore, duplicate webhook, and duplicate refund-request drills without external transfers.
- Prepare a list of 30 public repositories with active maintainers, reproducible Micro issues, recent merges, and permissive contribution rules.
- Book the first two external maintainers and stream slots.

Exit: all core tests green, a fresh complete production-readiness report, no non-terminal signer operations, and canary participants confirmed.

### 25–26 August: public canaries

- 25 Aug: run and publish the $2 successful-job canary.
- 26 Aug: run and publish the forced full refund-to-bounty canary.
- Post the evidence thread and dashboard after each, including measured variable execution estimates, omitted commercial costs, gross-margin status, and failure details.

Exit: one external paid merge, one 100% finalized refund, one rescue bounty, and no duplicate financial side effects. Any failure consumes the next day as a core-only incident day.

### 27–31 August: first external cohort

- Send 10 targeted maintainer messages per day, each naming one existing Micro issue and offering the fixed $2 quote with automatic full refund.
- Hold two 20-minute onboarding rooms daily for GitHub App installation and issue acceptance criteria.
- Stream two real jobs: one normal PR and one bounty claim/release.
- Ask every participant for one public one-sentence outcome and one maintainer referral.

Gate by 31 Aug: 4 paid jobs, 3 PRs, 1 merge, 2 external maintainers, 100% refund success, positive cumulative margin.

### 1–7 September: repeatability

- Continue 8 targeted maintainer messages per weekday and 3 follow-ups per prior participant.
- Run two scheduled 15-minute streams and one open maintainer office hour.
- Publish the live unit-economics dashboard and route-quality report every day.
- Accept only issue classes that cleared the week-one route benchmark.

Gate by 7 Sep: 7 paid jobs, 5 PRs, 3 merges, 3 external maintainers. Missing any gate triggers Micro-only pricing and daily hands-on onboarding until recovered.

### 8–12 September: reach launch minimum

- Close remaining paid jobs with external repositories first.
- Stream one full issue-to-PR transaction and one reserve/refund drill from recorded evidence.
- Freeze token narrative changes; lead every public artifact with completed maintenance work and customer protection.

Gate by 12 Sep: 10 paid jobs, 7 PRs, 5 merges, 3 external maintainers, all refunds successful, positive gross margin.

### 13–16 September: reliability buffer

- No scope expansion. Resolve review feedback, merge eligible PRs, reconcile ledger and treasury, and retest recovery.
- Publish an anonymized route failure matrix and the exact percentage of each quoted USDC payment refunded.
- Prepare a five-minute recorded fallback demo from real evidence in case live infrastructure fails.

### 17–19 September: evidence freeze and submission

- 17 Sep: freeze metrics and evidence links; verify every claim against GitHub and finalized chain records.
- 18 Sep: full 15-minute rehearsal, signer recovery drill, and stream setup check.
- 19 Sep: run the final stream. Do not risk a new financial flow in the final hour; use the prepared real-job evidence if no safe live issue is queued.

## Outreach list and message

Prioritize maintainers of active TypeScript, Rust, and small Solana tooling repositories with a recent, well-scoped bug or test issue. Exclude repositories that prohibit bots, require contributor license workflows not yet supported, or contain sensitive infrastructure.

Track repository, maintainer handle, issue URL, why it is Micro scope, contact channel, first contact, follow-up, App installed, quote paid, PR, merge, refund, and testimonial. Never store personal email addresses in the repository.

Suggested message:

> I am testing Mizuki on real public maintenance work before 19 September. It can take this specific Micro issue for a fixed $2 USDC quote, open a scoped PR, and automatically return the full quoted USDC payment if it cannot deliver. Solana network and wallet fees are separate. You keep normal review and merge control; it never opens unsolicited PRs. Would you install the repository-scoped GitHub App and try this issue on a short live session?

Follow up once after 48 hours with the public canary receipt. Do not mass-message, scrape private contacts, or offer a testimonial incentive.

## Fifteen-minute stream

| Time        | Content                                                                                                                                                                       |
| ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0:00–1:00   | Name the public issue, external maintainer, acceptance criteria, fixed price, and refund guarantee.                                                                           |
| 1:00–6:00   | Show issue → quote → wallet payment → job receipt → PR using a live safe job or an uncut recording of a real job.                                                             |
| 6:00–8:00   | Show checks, independent review, maintainer merge control, and finalized chain receipt.                                                                                       |
| 8:00–10:00  | Show the public full-refund-to-bounty evidence, including contributor escrow release.                                                                                         |
| 10:00–12:00 | Show paid jobs, PRs, merges, external maintainers, refund rate, independently verified refund capacity, variable execution estimates, omitted costs, and gross-margin status. |
| 12:00–14:00 | Explain failure-to-capability and the signed updater boundary.                                                                                                                |
| 14:00–15:00 | Give one install link, one eligible-scope sentence, and the next public job time.                                                                                             |

Background, model branding, and token mechanics stay after completed work, refund protection, and unit economics.
