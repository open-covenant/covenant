# Mizuki production-readiness audit

- Audit date: 23 August 2026
- Launch deadline: 19 September 2026
- Scope: the Mizuki workflow, API, web app, policy signer, updater, coding-gateway integration, deployment artifacts, and escrow program.

## Verdict

**NO-GO for public paid intake. Production readiness: 69/100.**

The code-local commercial core is substantially implemented. No open code-local P0 was found after the final correctness fixes and three-database verification pass. The paid engine now fails closed before accepting new work, preserves already-settled recovery during incidents, publishes the exact reviewed file bytes, and routes every failed settled job into the signer-backed refund path. The refund-to-bounty loop does not advertise an open bounty until a finalized on-chain escrow exists. The external policy signer independently verifies chain, GitHub, capacity, price, and immutable program facts before signing.

That is not sufficient to open paid intake. Hosted CI is green for revision `ada1d5e7d`, its escrow artifact remains reproducible, and the website plus fail-closed public API are live on Render from that exact revision. The API reports liveness and deployment safety but deliberately reports not ready because the policy signer, coding gateway, updater, GitHub App, reviewer route, and x402 facilitator are not configured live. Provider credentials and funding are absent; the custom domain is not DNS-verified; no platform restore has been proven; the escrow program has not been independently reviewed or deployed immutably; the live model/sandbox route is unbenchmarked; and neither required public mainnet canary has happened. The stated traction and verified-margin targets are also at zero in the supplied evidence.

Keep paid intake, new bounty claims, and updater promotion closed. The next work is production proof, not feature expansion.

| Area                                   | Score | Reason                                                                                                                          |
| -------------------------------------- | ----: | ------------------------------------------------------------------------------------------------------------------------------- |
| Paid maintenance engine                | 20/25 | Strong bounded and recoverable local flow; real route quality and scope classification unproven.                                |
| Refund-to-bounty engine                | 19/25 | Strong state and custody controls; no live end-to-end chain canary.                                                             |
| External policy signer                 | 20/25 | Strong independent policy boundary; no live keys, RPC disagreement drill, or deployed-program proof.                            |
| Deployment and operations              |  9/15 | Website, API, CI, process startup, and closed controls are proven; private services, DNS, recovery, and paid execution are not. |
| Live evidence, economics, and traction |  1/10 | Research queue exists; no qualifying paid jobs, merges, maintainers, or verified margin.                                        |

## Evidence verified

The application verifier ran against three fresh Postgres databases and exited successfully. The [Mizuki workflow](https://github.com/open-covenant/covenant/actions/runs/32645440904) and [repository CI workflow](https://github.com/open-covenant/covenant/actions/runs/32645440926) are green at revision `ada1d5e7d`.

| Component                        | Final result                                                              |
| -------------------------------- | ------------------------------------------------------------------------- |
| `@covenant/mizuki`               | 218 tests passed; typecheck, build, and process startup smoke passed      |
| `@covenant/mizuki-policy-signer` | 131 tests passed; typecheck and build passed                              |
| `@covenant/mizuki-updater`       | 61 tests passed; typecheck and build passed                               |
| `@covenant/coding-gateway`       | 130 tests passed; build passed                                            |
| `@mizuki/web`                    | 36 tests passed; typecheck, production build, and HTTP smoke passed       |
| Release checks                   | Blueprint invariants, Prettier, scoped production dependency audit passed |

Application total: **576 tests passed across 76 files**.

Escrow verification passed six host tests and 24 artifact-backed SBPF tests. The release artifact SHA-256 is `075302c38b9f895ea4aa94b9c61d9d83f98b85fd8f5e2cd0e915d08430b92b3f`, reproduced byte-for-byte across two hosted runs.

This establishes hosted build provenance for the tested revision and artifact. It does not establish deployed-program identity, immutability, custody safety, or a live transaction path. The API smoke boots the compiled server, checks liveness, and requires clean shutdown. The web smoke boots the standalone production server against a fake API and proves real HTTP 404 responses for missing receipts, non-404 outage pages, valid receipt rendering, and clean shutdown. The generated Next.js `next-env.d.ts` file is intentionally excluded from the source-format gate; the application source and deployment artifacts remain covered.

A separate packaging check assembled the intended scope in a fresh isolated checkout. `pnpm 10.31.0 install --frozen-lockfile` succeeded across all 20 workspaces, all five application builds passed, and the standalone web output succeeded with no `public/` directory present. The GitHub Action pins resolve to their stated releases, the updater Docker base is digest-pinned, and the exact staging allowlist excludes unrelated repository work. The database restore job compares normalized full data and sequence dumps across all three restored databases and was proven locally on Postgres 16.14. Hosted CI now passes, but the policy signer, coding gateway, and updater have not been deployed and platform backup/PITR recovery remains unproven.

Render service `srv-da5egcrl550s73cmhqh0` serves the website at [mizuki-9by5.onrender.com](https://mizuki-9by5.onrender.com) from deploy `dep-da5g9obtqb8s73acrh9g` at revision `ada1d5e7d`; [`/healthz`](https://mizuki-9by5.onrender.com/healthz) returns HTTP 200. API service `srv-da5fg02jobas73ei3n30` serves [mizuki-api.onrender.com](https://mizuki-api.onrender.com) from deploy `dep-da5g9o6417fc73fcmfgg` at the same revision. Its liveness and deploy-safety endpoints return HTTP 200, readiness returns HTTP 503 with incomplete dependencies named, admission reports intake and claims closed at revision zero, and a valid quote request returns HTTP 503 before GitHub or payment work. Both services have automatic deploy disabled. Paid Postgres resource `dpg-da5ekm8u01pc73evr3d0-a` is available on the Basic plan in Frankfurt.

The website proxies the public API with matching service-held credentials and demo mode disabled. It shows authoritative zero metrics and closed intake, omits quote and payment controls while closed, and returns real HTTP 404 responses for nonexistent job and bounty receipts. Browser inspection found the expected live state, male language, and no console errors. The custom domain `mizuki.covenant.org` is attached but DNS is not verified; the authoritative provider still serves the wildcard A record instead of CNAME host `mizuki` targeting `mizuki-9by5.onrender.com`.

The policy signer, coding gateway, and updater are not deployed. Paid intake, new bounty claims, and updater promotion remain closed. No evidence in scope proves a deployed GitHub App, live x402 settlement, live facilitator recovery, E2B egress denial, marketplace tool-call reliability, finalized refund, funded bounty, contributor payout, private-service connectivity, backup/PITR recovery, or updater promotion/rollback.

## Protected-core assessment

### Paid maintenance engine

The engine has the right commercial shape and is useful without relying on a verification-only story:

- Public repositories only; pull-request URLs are rejected.
- A repository-scoped GitHub App and issue label are required. The authorization receipt records a human label event by an actor with current repository permission. Feature, enhancement, security, vulnerability, and explicit new-capability requests fail before a payment challenge. Authenticated current labels and frozen issue content are rechecked before settlement and publication; title/body drift requires a new quote.
- Quotes pin the default-branch SHA, fixed 2/10 USDC prices, 3/10-file caps, variable-cost ceilings, and a deterministic repository validation command.
- x402 v2 settlement fixes the SVM network, canonical USDC mint, amount, recipient, and resource. Payment authorization is durable before settlement and recovery is idempotent.
- New intake is closed by default. Closing intake does not block background or authenticated recovery of an existing `settlement_pending` reservation.
- A single optimistic state transition admits paid work once. Production readiness requires authenticated persistent gateway runs, the configured marketplace route, and an E2B sandbox.
- Repository artifacts are exact-or-fail: changed text files are captured without truncation; missing, binary, over-128,000-byte, over-total-limit, or excess-count changes fail the run.
- The reviewer sees the patch, exact files, and validation results, uses a distinct required model, and permits one repair. The persisted review hash binds the pinned base and complete artifact object; publication uses those same file bytes.
- Publication rechecks repository head and maintainer authorization. Failure after settlement enters the refund path.

Residual risk is predominantly real-repository quality. The maintenance gate fails closed on explicit new-capability signals, but price class and validation selection still use title/body regex plus root-manifest detection rather than repository-aware issue decomposition. Work whose actual change surface does not fit the quote will fail safely, but can still hurt delivery and margin.

### Full refund to funded rescue bounty

The financial state machine is materially stronger than an application-ledger promise:

- Refund liability registration happens immediately after finalized settlement and before paid processing.
- Refund and successful-delivery discharge serialize on the same liability, so only one outcome can consume it.
- A refund is accepted only when signer evidence matches job, settlement, payer, mint, decimals, and exact principal. Signed bytes and transaction state are durable and idempotent across restart and retry.
- A rescue bounty is created only after the customer refund is finalized.
- A bounty becomes public and claimable only after the signer has funded its claimant-free SOL escrow and finalized the funding transaction. A dedicated Prometheus gauge detects any open bounty missing that evidence and the runbook closes claims on a nonzero value.
- Claim binding requires GitHub OAuth plus a single-use wallet-signature challenge. The signer binds the immutable GitHub identity, wallet, repository, issue, amount, and expiry on-chain.
- Contributor work is file-capped, forbidden-path checked, requires at least one passing repository check, and receives independent review.
- Release requires signer-verified public merge evidence authored by the bound claimant and closing the immutable issue before expiry. Release, expiry refund, and dispute resolution are serialized and recoverable.

The remaining blocker is proof: none of these paths has been exercised against the deployed program with real value.

### External policy signer

This is the strongest trust boundary in the delivery:

- It is a private authenticated service with its own Postgres database and separate refund and escrow authorities.
- Customer refunds and discretionary escrow funding use separate rolling caps, so bounty activity cannot consume protected refund capacity.
- Job-authority requests are action-bound and Ed25519-signed. The signer does not trust payer, amount, mint, recipient, GitHub merge, or program state supplied by the application.
- Settlement and transaction finality must agree across two distinct RPC providers.
- Refund capacity is reserved transactionally against both finalized treasury balance and the rolling liability cap.
- Prepared signed bytes, leases, resource keys, attempts, and terminal evidence are durable. Blockhash replacement is allowed only after the old transaction can no longer land.
- Escrow signing requires loader-v3 deployment with no upgrade authority and identical pinned executable bytes on both RPCs.
- Two independent, freshness-bounded SOL/USD sources must agree within the configured divergence limit.

The service still needs live operational proof with production-scoped keys, independent RPCs and price sources, restart drills, and a real immutable program deployment.

### Failure-to-capability updater

The updater strengthens the public failure-to-capability story without controlling the commercial path. Proposal, benchmark, and review authorities are distinct; manifests, handoff artifacts, GitHub repository/branch/check policy, shadow health, production soak, rollback, and audit-chain state are durable. Promotion is closed by default in Postgres and is serialized across the control check, hook call, and verifying transition.

This path is not ready for autonomous promotion. Deployment hooks are external to this repository, and a successful rollback hook is not followed by independent verification that the prior revision is active and healthy. Keep promotion closed through launch; public proposals and benchmarks can be shown without granting deploy authority.

## P0 launch blockers

### 1. The protected commercial path is incomplete

Revision `ada1d5e7d` has green hosted CI receipts, a reproducible escrow artifact, and healthy Render website and API processes. The API is useful for public evidence and proves fail-closed admission, but it correctly reports not ready. The external policy signer, coding gateway, and updater are absent, so the live platform cannot safely settle, execute, review, publish, refund, fund, or promote work.

Required: deploy the three private services from the verified revision, bind their private credentials without recording secrets in evidence, keep intake, claims, and promotion closed, and prove authenticated service-to-service connectivity plus fail-closed startup before any canary payment.

### 2. Live authority, funding, DNS, and recovery proof are missing

The website, API, and one paid Postgres resource are available. The custom domain is attached but not DNS-verified; the required record is CNAME host `mizuki` to `mizuki-9by5.onrender.com`. Live model and sandbox providers, production-scoped authorities and RPC keys, facilitator configuration, treasury/refund reserves, bounty SOL funding, and the immutable program identity are not proven. There is also no receipt for complete readiness, stateful restart recovery, or restoration of financial state from a platform backup/PITR point.

Required: verify DNS, configure the live providers and authorities, fund only capped canary reserves, run a 30-minute readiness soak, exercise RPC disagreement and duplicate requests, restart every stateful process, and restore every production database into an isolated environment. Hosted CI compares complete normalized data and sequence dumps, but it is not a substitute for a platform backup/PITR drill followed by application reconciliation.

### 3. Escrow custody is not release-ready

The ABI still has a null program ID, `SECURITY.md` explicitly states there is no independent third-party audit, and there is no finalized devnet canary, immutable mainnet deployment, independent `solana-verify` receipt, or two-RPC on-chain program-data match.

Required: independent review of create/bind/release/refund/dispute semantics, adversarial devnet canaries, reproducible build comparison, finalized immutable deployment, signer configuration pinned to the verified program and executable hash, and low-value public create/release/refund mainnet canaries.

### 4. The two public commercial canaries have not happened

There is no public 2 USDC issue-to-quote-to-payment-to-validated-PR-to-merge receipt. There is no forced post-payment failure proving exact principal refund, funded rescue bounty, external claim, accepted PR, merge, and payout.

Both must be public, loud, and performed with the actual production services. Internal canaries prove integration and refund integrity; they do not count as external traction.

### 5. Live route quality and complete unit economics are unknown

The exact marketplace route and sandbox have mock-backed unit coverage but no live benchmark matrix or provider billing receipts. The dashboard correctly labels application-ledger allocation as non-custodial, suppresses revenue until liability discharge, marks demo traction as an illustrative fixture, and leaves gross margin unverified because provider adjustments, facilitator/chain fees, and infrastructure are not fully captured. That honesty must remain.

Required: at least 12 representative runs across docs/config, tests, and small bugs; record delivery, validation, repair, review, latency, tool behavior, tokens, sandbox seconds, route identity, billed provider cost, facilitator/chain cost, and failure/refund outcome. Freeze eligible issue classes based on measured results.

### 6. Traction gate is unmet

Evidence currently shows no qualifying paid external jobs, PRs, merges, external maintainers, refund-rate history, or verified positive margin. The research queue now contains 30 unique compatible repositories and issues: 20 Micro and 10 Standard. All 30 pass the exact current production quote gate plus a stricter maintenance-only review. A final live pass found them public, nonarchived, open, unassigned, without a linked open PR or human claim comment, and backed by the exact validation marker, command, and class the current quote code selects. No outreach has been sent, so this is a prepared acquisition queue, not traction.

Launch minimum remains: 10 paid jobs, at least 7 PRs, at least 5 merges, at least 3 external maintainers, 100% successful principal refunds, and positive fully loaded gross margin. Operator-owned repositories do not count as external demand.

## P1 risks and pressure points

1. **Refund registration has a 24-hour recovery boundary.** Recovery now continues while intake is closed, which fixes the immediate incident wedge. A signer or database outage lasting beyond 86,400 seconds after settlement can still make automated liability registration reject an otherwise valid payment. Alert on `settlement_pending` age immediately and either reserve capacity before settlement or add a tightly authorized recovery proof that cannot manufacture liabilities.
2. **Complexity classification is intentionally crude.** The new maintenance-only gate closes the known feature/enhancement bypass at quote, payment, and publication, including issue drift. Regex price classification and root-manifest validation can still underestimate implementation complexity or choose a repository-wide command that is slow or broken. Keep early intake Micro-only with explicit operator inspection and benchmarked issue classes.
3. **Sandbox resource isolation needs a deployment receipt.** E2B creation passes requested resource and egress intent, but effective CPU, memory, disk, and network policy depend on the deployed template/provider behavior. Pin a hardened template and prove denied hosts, secret absence, timeout destruction, and restart cleanup live.
4. **Browser financial flows lack end-to-end coverage.** The mocked quote flow passed production-browser QA at 390×844 with no console errors, and the issue-URL pattern has a regression for modern Unicode-set semantics. There is still no automated live wallet payment, lost-response recovery, OAuth callback, wallet binding, bounty claim, PR submission, dispute, or accessibility suite.
5. **Observability is not operationalized.** Readiness, Prometheus state-age and unfunded-open-bounty gauges, alert thresholds, and stop-action runbooks are present, but they are not wired to a production monitor or pager and there are no SLOs, distributed traces, or production dead-letter views. Correlate API, gateway, signer, GitHub, and updater operations and prove alerts on state age, reserve coverage, settlement recovery, refund finality, bounty funding, and provider cost drift.
6. **Backup evidence stops at hosted CI.** Full data and sequence equality is stronger than a migration-only smoke test and the hosted workflow is green, but platform backup/PITR restoration remains unexecuted. Restore production data into an isolated environment, then prove application reconciliation against the restored financial state.
7. **Updater rollback proof is incomplete.** A successful rollback-hook response marks the record rolled back without independently confirming the former revision is active and healthy. Keep promotion disabled until the real hook system and regression rollback are rehearsed.
8. **The gateway is single-instance file persistence.** Run and spend receipts are durable on one mounted disk, but whole-file JSON persistence is not a horizontally safe queue. This is acceptable for tightly capped canaries, not scale-out.

## P2 hardening after launch gates

- Add a bounded timeout to the web proxy and a wallet-compatible Content Security Policy.
- Replace full-table reconciliation and per-client SSE database polling with indexed work queues and shared fan-out before volume grows.
- Add server-side session revocation, signing-key versions, logout, and session inventory.
- Anchor updater and public receipt roots outside the databases before describing them as independently tamper-proof.
- Move transaction keys from process environment to managed signing when balances exceed canary limits.
- Add structured retention and redaction policy for activity, model, GitHub, and financial evidence.

## Required execution sequence

### 22–24 August: freeze and release

1. Preserve the green CI receipts, process-smoke results, exact Render deploys, and reproducible artifact for revision `ada1d5e7d`; rerun the same gates for any later release revision.
2. Verify the custom-domain CNAME, deploy the policy signer, coding gateway, and updater, then complete connectivity, restart, restore, RPC disagreement, duplicate request, and signer recovery drills with every public control still closed.
3. Configure and fund the capped live providers and authorities without placing secrets in repository evidence.
4. Complete the live route benchmark and publish cost coverage without claiming verified margin.
5. Complete independent escrow review, adversarial devnet canaries, reproducible verification, and immutable deployment.
6. Reverify the 30 screened candidates and confirm the first canary participants. Do not contact maintainers before the internal canaries pass.

### 25–26 August: public canaries

1. Publish the successful 2 USDC issue-to-merge canary with settlement, route, validation, review, PR, merge, cost, and liability-discharge evidence.
2. Publish the forced-failure canary with exact refund, funded bounty, claimant binding, external PR/check/review, merge, and escrow release evidence.
3. Keep updater promotion closed. Show signed proposal and benchmark evidence separately from deployment authority.

### 27 August–12 September: external proof

Run targeted, permission-first maintainer onboarding in cohorts. Count only real payments and independently controlled repositories. Publish route quality and complete cost coverage continuously. Do not expand beyond Micro work until delivery, refund, and margin gates support it.

### 13–19 September: reliability and evidence freeze

Stop feature work. Reconcile every payment, liability, refund, escrow, PR, merge, and provider receipt. Rehearse the 15-minute stream around real issue-to-PR and refund-to-capability evidence. Freeze claims to evidence that an external reviewer can follow independently.

## Hard stop conditions

Paid intake must remain or immediately become closed if any of the following occurs:

- refund capacity is unavailable, stale, or mismatched;
- a settled payment lacks a registered liability or exceeds the recovery-age alarm;
- any refund is short, late beyond the published operating window, sent to the wrong payer, duplicated, or not finalized;
- a bounty is visible as open without a matching finalized escrow;
- GitHub authorization, pinned base, validation, review hash, or published bytes disagree;
- signer RPCs, price sources, program bytes, or program authority disagree;
- the live route exceeds a quote cost cap or falls below the accepted-class reliability threshold;
- gross margin is shown as positive without complete durable cost receipts;
- any promotion or deployment control opens without a recorded operator reason and successful rehearsal.

The implementation is now credible enough to earn production evidence. It is not credible to substitute local tests, token activity, or internal dogfooding for that evidence.
