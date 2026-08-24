# Production Audit: Mizuki Workbench

## Executive summary

The Workbench is a real account product layered onto the existing maintenance, payment, refund, signer, and bounty systems. It does not create a second commercial path or simulate customer balances. Independent review found three release blockers and several high-impact production gaps. Every listed blocker is closed, covered by tests, and included in the release gate before the matching runtime and website are promoted.

## Critical issues (P0 — block release)

- [x] Overall repository policy failure could be overridden by issue eligibility in the client normalizer. `readyForWork: false` is now authoritative and covered by a regression test.
- [x] Boolean issue eligibility could be interpreted as missing, allowing an authorized but unsupported issue to appear ready. Boolean eligibility is now handled explicitly and covered by a regression test.
- [x] Workbench tables were initially registered as `core` schema v2, which would make the deployed runtime unable to restart after a candidate migration. They now use an additive `workbench` v1 component while `core` remains unchanged at v1.

## High priority (P1 — fix before launch)

- [x] Bound GitHub- and signer-backed account reads by source and account, reduce repository-list call amplification, and keep issue enumeration bounded.
- [x] Fail closed when a verified maintainer's quote cannot be durably linked to its account. Never return payable terms for an account job that cannot appear in account history.
- [x] Populate the Available bounty tab from the public open-bounty feed and keep account history separate.
- [x] Publish `refund_pending` billing activity as pending rather than displaying a permanent zero count until finalization.
- [x] Present bounty rewards using exact lamports/SOL and label USD values as approximate reference values.
- [x] Distinguish missing GitHub installations and invalid label provenance from provider, credential, rate-limit, and transient failures.

## Medium priority (P2 — fix in this release where bounded)

- [x] Remove contradictory repository copy that marked checks missing while the repository was ready. Validation commands are confirmed per issue during preflight.
- [x] Replace the account-mutating issue-list GET with an explicit repository-connect POST. Keep GET reads idempotent.
- [x] Refetch bounty state after claim, wallet-proof, pull-request, and dispute mutations.
- [x] Apply database-level account-job bounds and expose truncation or pagination semantics honestly.
- [x] Make billing and integrations directly reachable from the mobile More surface.
- [x] Keep Workbench bounty authentication returns inside the Workbench route.
- [x] Remove stale gendered references and keep all Mizuki product copy on it/its language.
- [x] Use the exact refund contract in product copy: refund of the quoted USDC payment, with network and wallet fees separate.

## Security assessment

Account ownership is derived from the signed GitHub session's immutable GitHub ID. Quote ownership is unique, and account repository links are created only after current maintainer permission is verified through the delivery App. Account endpoints use private, no-store responses. OAuth tokens are not persisted in Workbench.

The Workbench does not receive custody credentials, modify signer policy, bypass payment admission, or introduce a new settlement path. Existing public job and bounty receipts remain public by design. The new database component is additive and transactionally migrated under the existing advisory lock.

High-cost GitHub and signer reads have dual per-source and per-account limits. Provider outages, invalid provenance, and user authorization failures have distinct public classifications. Repository connection is an intentional POST; issue enumeration remains a read-only GET.

## Performance assessment

Each account can link at most 25 repositories. New links are serialized with an account-scoped database lock; refreshes of existing links remain idempotent. Repository reads fetch at most 26 rows to detect truncation, use metadata-only GitHub reads, and are bounded to four concurrent workers. Issue enumeration is capped at 20 records and five concurrent workers, with permission checks cached per actor. Admission is applied before external calls.

Account jobs fetch at most 101 rows to return a 100-record page with explicit truncation. Billing fetches at most 1,001 rows for a 1,000-job scope and labels whether totals represent lifetime history or the latest bounded records.

## Observability assessment

The commercial runtime already exposes health, dependency readiness, admission controls, activity, treasury, and unit-economics evidence. Workbench errors deliberately avoid retrying payments or presenting guessed state. Account endpoint rate-limit failures return `429` with `Retry-After` through the existing admission layer.

Launch verification must record the exact runtime image digest, merge commit, migration rows, Render deployment IDs, website commit, admission revisions, and production smoke results. Account data itself must not be added to public telemetry.

## Architecture assessment

The architecture keeps one commercial source of truth:

1. GitHub-backed preflight validates repository, maintainer, authorization, scope, and separate policy readiness.
2. The existing quote endpoint creates fixed payment terms.
3. The existing x402 job endpoint settles and enters the unchanged protected execution path.
4. Workbench reads the resulting account-linked jobs, billing activity, refunds, and bounties.

There is no prepaid balance, fabricated credit system, private-repository promise, unsolicited pull-request path, or parallel refund implementation.

## Test coverage

Completed local coverage:

- Mizuki core: 352 passing tests, 14 skipped integration-dependent cases.
- PostgreSQL migration and integration suite: 14 passing tests.
- Policy signer: 205 passing tests, 2 skipped integration-dependent cases.
- Updater: 94 passing tests, 2 skipped integration-dependent cases.
- Deployment controller: 39 passing tests, 1 skipped integration-dependent case.
- Coding gateway: 182 passing tests.
- Workbench web: 70 passing tests.
- Core and Workbench typechecks and production builds pass.
- Full verification passes through web smoke; the local machine has `cargo-build-sbf` 3.1.13 while the pinned release gate requires 4.0 or newer. The protected CI Rust producer remains authoritative for that gate.

Release-blocking regression coverage includes:

- External-call admission and bounded fan-out.
- Durable quote/account-link failure.
- Pending-refund billing activity.
- Typed GitHub outage versus setup errors.
- Explicit repository connection with read-only issue listing.
- Post-mutation bounty refresh.
- Repository-cap concurrency and bounded account bounty queries.

## Release action plan

1. Re-run core, PostgreSQL, web, signer, updater, controller, gateway, formatting, and source-identity checks.
2. Merge only after all protected checks cover the exact final head and a non-pusher approves that head.
3. Verify the immutable main-only runtime image evidence.
4. Confirm controller and updater ledgers are idle using short-lived private one-off jobs.
5. Promote shadow, close production admission only for the production promotion window, promote and finalize, then deploy the exact website commit.
6. Verify authenticated and unauthenticated production behavior before reopening admission.
