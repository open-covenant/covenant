# Mizuki

Mizuki sells small, bounded software-maintenance jobs. A maintainer submits a public GitHub issue, receives a fixed USDC quote, pays over x402, and gets either a validated pull request or a refund of the quoted USDC payment. Network and wallet fees are separate.

Mizuki is a delivery agent, not a verification wrapper and not a trading bot. The paid result is the code change.

## Contract

- Micro: 2 USDC, at most 3 files, variable execution estimate cap of $0.80.
- Standard: 10 USDC, at most 10 files, variable execution estimate cap of $4.00.
- Public repositories only.
- The GitHub App must already be installed. Mizuki never opens unsolicited PRs.
- Feature, enhancement, and security-labeled issues are rejected. Explicit new-capability requests are out of scope.
- No auth, secrets, cryptography, custody, payments, deployment, workflow, generated, vendored, or lockfile work.
- The quoted commit and issue text are pinned. Repository or issue drift invalidates the quote before payment.
- One independent review and at most one repair pass.
- Any failure after settlement enters the refund path. Refund calls are idempotent and retryable.

## Flow

```text
GitHub issue -> scoped quote -> x402 USDC settlement -> isolated checkout
             -> register full refund liability -> UsePod coding route
             -> repository checks -> clean-context independent review
             -> scope + authorization recheck -> GitHub App PR -> merge receipt
                                              \-> failure -> full refund
                                                           -> funded rescue bounty
                                                           -> public capability proposal
                                                              -> externally signed upgrade evidence
```

The model runs in the existing Covenant coding gateway. It receives an ephemeral checkout and no GitHub or wallet credentials. Mizuki publishes accepted files with the GitHub Git Data API. Production must use E2B or another hardened sandbox; repository runs fail closed on the gateway's local provider unless `ALLOW_LOCAL_REPOSITORY_RUNS=1` is deliberately set for trusted development. Configure and verify `E2B_EGRESS_ALLOW` before accepting public work.

## Run locally

```sh
pnpm install
pnpm --filter @covenant/coding-gateway build
pnpm --filter @covenant/mizuki build
MIZUKI_ADMIN_TOKEN=replace-with-at-least-32-characters MIZUKI_PAYMENT_MODE=mock MIZUKI_REQUIRE_GITHUB_APP=0 pnpm --filter @covenant/mizuki start
```

The in-memory development store also starts closed. Open it deliberately through `POST /v1/admin/admission` before submitting a mock job.

Copy `.env.example` into the deployment secret manager. Do not commit a populated env file.

The coding gateway needs `CODER_BACKEND=usepod`, a pinned `CODER_MODEL`, `USEPOD_API_KEY`, `E2B_API_KEY`, authenticated access, and persistent ledger/run-store paths. Mizuki needs a dedicated Postgres database, GitHub App credentials, the x402 treasury, a distinct reviewer route, the private policy-signer link, and a dedicated refund-liability authority key.

Before changing the production coding route or model, run `pnpm --filter @covenant/mizuki benchmark -- cases.json` against a file of `{name, repositoryUrl, baseSha, prompt, validationCommands, maxCostUsd}` cases. `maxCostUsd` defaults to the Micro ceiling of `0.8` and cannot exceed the Standard ceiling of `4`. Each case uses a unique idempotency key and passes that explicit all-in cap to the gateway. Keep the JSON report, including provider and cost receipts, as the route evidence. The benchmark exits nonzero if any route fails.

## HTTP and MCP

- `GET /v1/account` returns the signed-in GitHub account. `POST /v1/auth/logout` clears the session.
- `GET /v1/account/repositories`, `/v1/account/jobs`, `/v1/account/billing`, and `/v1/account/bounties` return only records linked to the signed-in maintainer account.
- `POST /v1/account/repositories` with `{"owner":"owner","repo":"repo"}` verifies current maintainer access and explicitly links one public repository to the account.
- `GET /v1/repositories/:owner/:repo/issues` is read-only and lists bounded maintenance candidates for a linked repository.
- `POST /v1/preflights` with `{"github_issue_url":"https://github.com/owner/repo/issues/1"}` requires a signed-in maintainer and an explicitly connected repository, then checks both App installations, maintainer authority, attributable authorization evidence, scope, current repository metadata, and validation commands without creating a quote or accepting payment.
- `POST /v1/quotes` with `{"github_issue_url":"https://github.com/owner/repo/issues/1"}` creates a public quote without linking it to a Workbench account.
- `POST /v1/account/quotes` uses the same input but requires a signed-in maintainer and an explicitly connected repository. It durably links the quote to that account before returning a payment challenge, enabling safe payment-status recovery.
- `POST /v1/jobs` with `{"quote_id":"..."}`, `Idempotency-Key`, and the x402 v2 `PAYMENT-SIGNATURE` header.
- `GET /v1/account/quotes/:quoteId/payment-status` with the original `Idempotency-Key` safely distinguishes an existing job reservation from an unpaid quote. It never requests or submits a payment signature.
- `GET /v1/jobs/:id` for PR, validation, or refund status.
- `GET /v1/metrics` and `GET /metrics` for the public unit-economics dashboard.
- `GET /v1/admission` for the public paid-intake and new-claim switch status. Both switches start closed on a fresh database.
- `GET /v1/admin/jobs`, `POST /v1/admin/refunds/:jobId`, and `POST /v1/admin/settlements/:jobId` require the admin bearer token. Payment authorization is durably reserved before facilitator settlement; the last endpoint reconciles an indeterminate settlement after a process or network failure.
- `GET /v1/admin/admission` and `POST /v1/admin/admission` require the admin bearer token. Updates accept `intakeEnabled`, `claimsEnabled`, and a 10-500 character `reason`; paid authorization and settlement read the durable switch inside the same serial gate.
- `POST /v1/admin/bounties/:bountyId/disputes/:disputeId/resolve` requires the admin bearer token, an idempotency key, a release/refund decision, and normalized public evidence. Retryable signer failures remain pending. A dispute cannot pretend to freeze a release that has already begun.

Run `pnpm --filter @covenant/mizuki mcp` to expose quote, submission, status, repository readiness, issue preflight, and payment-recovery tools over stdio. A wallet-capable host creates the x402 signature; Mizuki never asks an MCP client for a private key. Every MCP API request has a bounded timeout, configurable from 1,000 to 60,000 milliseconds with `MIZUKI_MCP_TIMEOUT_MS`.

Repository, issue, and payment-recovery tools fail closed unless `MIZUKI_SESSION` contains a valid signed Workbench session supplied through the MCP host’s secret storage. They reuse the same authenticated maintainer, GitHub App, repository-link, and quote-account checks as Workbench. They do not accept a GitHub token as a substitute and cannot connect a new repository; complete that explicit authorization in Workbench first.

Expensive public mutations use bounded per-source token buckets and return `429` with `Retry-After`. Production on Render must set `MIZUKI_TRUSTED_PROXY_HOPS=1` and share `MIZUKI_WEB_PROXY_SECRET` only with the same-origin web proxy. The setting enables Render-specific edge trust; it is not a generic proxy-chain depth. Direct ingress validates Cloudflare's overwritten `CF-Connecting-IP` value and ignores `X-Forwarded-For`, which Cloudflare appends to caller-controlled values. Missing or malformed edge identity falls back to the direct socket. The authenticated web proxy context carries the same validated address without trusting browser-supplied Mizuki headers. Activity streams have global, per-source, and idle-lifetime caps.

## GitHub App

Grant only:

- Repository contents: read and write.
- Issues: read.
- Pull requests: read and write.
- Checks: read.
- Metadata: read.

Subscribe only to Pull request events. Maintenance-only scope, exact issue text, installation, authorization-label provenance, and the human label actor's current repository permission are checked at quote, payment, and immediately before publication. Short-lived installation tokens request exactly one repository and the permission map above. Mizuki rejects all-repository selection, permission drift, a different repository, an invalid lifetime, a suspended installation, or App identity drift before using a token.

## External policy-signer contract

Before any paid work starts, Mizuki sends only the job ID and finalized settlement signature to a separately deployed signer. A short-lived, action-bound Ed25519 request proves that the live API authorized the registration. The signer independently derives payer, recipient, mint, decimals, amount, slot, and block time from two finalized RPC views, then atomically reserves enough treasury capacity for the entire principal.

Failure uses a fresh refund-execution authorization tied to that registered liability. The signer reconstructs the exact full-principal transfer and persists signed bytes before broadcast. Retry uses the same economic idempotency key and can never authorize a second transfer.

Successful work releases protected reserve only after the signer independently verifies a public merged PR created after settlement. Refund and discharge lock the same liability row, so only one can begin. A retryable failure stays public as `refund_pending`; it is never reported as successful. The API has no Solana custody key and cannot change signer limits, accepted programs, recipients, or evidence rules.

## Public accounting contract

`/v1/treasury` and `/v1/metrics` call the bounded-fresh service readiness probe before publishing protection evidence. Refund custody is verified only when the complete readiness report is current, signer atomics are coherent, finalized custody covers signer liabilities, and signer liabilities exactly reconcile with the application job records. Missing, stale, or mismatched evidence returns `unavailable` or `degraded`; application ledger rows can never make protection verified.

Recorded USD net flow and the 70/30 waterfall are published separately as an `application_ledger` allocation model with `custodyVerified: false`. Its values are plans, not wallet balances or spend authority. SOL rescue escrow and platform-reported creator fees stay outside that model. Customer payments are settled receipts; revenue is recognized only after the corresponding refund liability is independently discharged. Gross margin remains unverified while any commercial cost category is omitted.

## Provenance and isolation

Mizuki uses independent package names, APIs, schemas, signer operations, escrow instructions, public copy, and telemetry. Earlier internal prototypes informed general design principles such as strict payment boundaries and independent review, but no legacy payment proof, simulated wallet tool, public identifier, or on-chain instruction name is accepted by this system.

The production path is deliberately narrow: official x402 v2 exact SVM settlement, deterministic refund of the quoted USDC principal, and separately funded contributor escrow. Network and wallet fees are outside the quoted principal. Every transfer is bound to independently verified chain or GitHub evidence and a durable idempotency key.

Workbench account links use their own additive `workbench` migration component. The commercial `core` component remains independently versioned so an older production image can safely restart during a rollback.

For every new live payment proof, the serialized admission gate checks refund capacity and calls the policy signer for fresh readiness of the quote's exact repository before invoking settlement. The signer must prove a distinct read-only verifier App installation and a freshly minted one-repository token. A failed probe cannot reserve or settle a payment. Settlement recovery deliberately skips this new-payment probe because its durable reservation may already have paid on-chain; recovery must remain able to register the liability and finish the existing transaction while intake is closed.

## Capability upgrade observer

Mizuki records every finalized paid failure as a capability proposal, including the first forced micro-refund canary. `GET /v1/capabilities/:capabilityId/handoff` publishes its deterministic failure and benchmark contract plus `handoffSha256`; the same read is available through the `mizuki_capability_handoff` MCP tool and the public capabilities page. An external release authority may then submit a signed, benchmarked, independently reviewed artifact to Mizuki Updater using the core upgrade UUID as the manifest `proposalId` and the published hash as `sourceHandoffSha256`. The core never creates a signature or submits a proposal on that authority's behalf.

Configure `MIZUKI_UPDATER_URL` and `MIZUKI_UPDATER_TOKEN` together, using the updater's read-only credential rather than its submission credential. The core resolves each outstanding proposal through the updater's authenticated read API and copies only verified receipt identifiers, hashes, deployment evidence, and durable updater state into its public capability record. It recomputes the current handoff from the capability, upgrade trigger, and ordered failure evidence before every transition. A missing proposal, malformed evidence, an older handoff invalidated by new failure evidence, or any hash mismatch leaves the capability unchanged. Reconciliation is idempotent and resumes from the last stored transition after restart.

## Launch gates

Keep external paid intake closed until both operator-funded mainnet canaries are public:

1. A paid 2 USDC issue produces a real PR and publishes the settled receipt, variable execution estimate, and omitted-cost categories without claiming revenue before signer discharge or claiming gross margin.
2. A forced failure returns the full 2 USDC to the facilitator-verified payer, reports `refunded`, and publishes a hashed capability handoff for external implementation.

Then target 10 paid jobs, 7 PRs, 5 merges, and 3 distinct external maintainers with App-authorized paid jobs outside `MIZUKI_INTERNAL_REPOS`. Positive gross margin remains unmet until provider billing adjustments, chain/facilitator fees, and infrastructure are durably recorded. Lead demos with issue -> quote -> payment -> PR/refund; route receipts, margin, and external adoption come next. Token activity is surfaced through `MIZUKI_TOKEN_MINT`, but it is not used to disguise missing product traction.
