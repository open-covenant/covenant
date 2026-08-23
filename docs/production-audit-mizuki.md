# Mizuki production-readiness audit

- Audit date: 23 August 2026
- Launch deadline: 19 September 2026
- Scope: the Mizuki paid-maintenance engine, refund-to-bounty loop, external policy signer, coding gateway, updater, deployment boundary, and Solana escrow program.

## Verdict

**NO-GO for public paid intake. Production readiness: 66/100.**

The last hosted release baseline passed at revision `dfeffb0a8c8280bb7b3844bead750fccf7233ae7`, produced a 104,376-byte SBPFv2 artifact, and exercised 31 host and artifact-backed escrow tests. That exact historical artifact was then deployed to devnet and completed fund, bind, exact release, bound-expiry refund, unbound-expiry refund, wrong-claimant rejection, expired-release rejection, and replay rejection with finalized public transactions. A separately funded provider account also completed synthetic coding and review route canaries.

That is meaningful integration proof, but it is not production proof. The devnet program is upgradeable. The policy signer, coding gateway, updater, GitHub Apps, model route, sandbox, and project settlement accounts are not operating together in production. There is no approved immutable mainnet program, no funded end-to-end settlement path, no independent escrow review, no public paid job, and no public customer-refund-to-funded-bounty canary.

Keep paid intake, new bounty claims, and updater promotion closed. A newer release candidate contains additional fail-closed controls for payment admission and recovery, provider-balance admission, updater check identity, and permanent escrow evidence. Those changes are not part of the hosted baseline and do not inherit its evidence. They require a frozen commit, a new clean hosted run, and a new artifact hash before deployment or release claims.

| Area                                   | Score | Reason                                                                                                                                |
| -------------------------------------- | ----: | ------------------------------------------------------------------------------------------------------------------------------------- |
| Paid maintenance engine                | 19/25 | Bounded source path and funded synthetic model canary; no real-repository sandbox run or paid GitHub publication.                     |
| Refund-to-bounty engine                | 19/25 | Exact release and both refund paths passed on devnet; the commercial refund-to-public-bounty loop remains unproven.                   |
| External policy signer                 | 18/25 | Strong independent policy boundary and live canary runner; the service, funded production custody, and mainnet deployment are absent. |
| Deployment and operations              |  8/15 | Hosted CI, public website/API liveness, and closed controls exist; private services, DNS, restore, and promotion proof do not.        |
| Live evidence, economics, and traction |  2/10 | Funded model-route canaries passed, but no paid jobs, external maintainers, complete cost receipts, or verified positive margin.      |

## Historical verified release baseline

The [Mizuki workflow](https://github.com/open-covenant/covenant/actions/runs/32662760151) and [repository CI workflow](https://github.com/open-covenant/covenant/actions/runs/32662760145) completed successfully for revision `dfeffb0a8c8280bb7b3844bead750fccf7233ae7`. Every result in this section is historical evidence for that revision only.

| Component                                | Hosted result                                                       |
| ---------------------------------------- | ------------------------------------------------------------------- |
| `@covenant/mizuki`                       | 248 tests passed; typecheck, build, and process smoke passed        |
| `@covenant/mizuki-policy-signer`         | 164 tests passed; typecheck and build passed                        |
| `@covenant/mizuki-updater`               | 64 tests passed; typecheck and build passed                         |
| `@covenant/mizuki-deployment-controller` | 37 tests passed; typecheck and build passed                         |
| `@covenant/coding-gateway`               | 159 tests passed; typecheck and build passed                        |
| `@mizuki/web`                            | 39 tests passed; typecheck, production build, and HTTP smoke passed |

Application total: **711 tests passed across 90 files**. The workflow also exercised full data-and-sequence dump/restore equality across the four isolated test databases. This is CI restore evidence, not a managed production backup or point-in-time recovery drill.

Escrow verification passed six host tests and 25 artifact-backed SBPF tests. The hosted artifact archive records:

- Git revision: `dfeffb0a8c8280bb7b3844bead750fccf7233ae7`
- Artifact SHA-256: `2d24fd43b65a7bb31b39007b93717b1f65615df39aeec33b9eebe83bb89a2237`
- Solana executable hash: `42bd1e28a27ad9fe1c08f38c83008fe67db12081480b77cf4adeeeb06fcf038a`
- Artifact length: 104,376 bytes
- ELF machine: registered `EM_SBF` value `263`
- SBPF version and flags: v2 and `0x2`
- Toolchain: `cargo-build-sbf 4.0.0`, platform-tools `v1.53`, Rust `1.89.0`, and `solana-verify 0.5.0`

The archive is retained by the historical workflow for 90 days. It has not been copied into a permanent release, so it remains time-limited evidence rather than a durable public transparency record.

## Unverified release-candidate hardening

The current release candidate adds four controls that directly address the commercial trust boundary:

- Before a facilitator payment broadcast, the policy signer must independently revalidate and persist a repository admission bound to the quote, repository, issue, base ref and SHA, reservation-key hash, and complete payment-authorization hash. It validates the full authorization transiently but retains only a non-replayable fingerprint: message hash, client signature, fee payer, amount, and admitted payment window. Recovery first asks the signer to reconcile that fingerprint through two independent RPC providers. Only dual-provider proof that the transaction is absent permits one facilitator replay; disagreement, scan exhaustion, altered evidence, or an incomplete history fails closed. A known finalized transaction is verified directly against the same fingerprint, so its liability can be registered after a prolonged outage without a second charge or an arbitrary wall-clock expiry.
- The updater no longer treats a check name or commit status as sufficient evidence. Each required check is pinned to an exact GitHub App, workflow ID, workflow path, event, repository, pull request, head and base refs, and head and base SHAs. It joins the check run to its workflow run through the check-suite identity and requires the workflow file at the candidate commit to match the Git blob at the signed base commit.
- Coding-gateway readiness now queries the provider's nonbillable authenticated balance endpoint and requires exactly one `X-Balance-Remaining` header to agree with a safe-integer `usdc_balance`. Readiness and run admission fail closed on a malformed, duplicated, conflicting, or insufficient result, with a floor of 4,000,000 USDC microunits.
- The protected-main workflow now defines a permanent escrow-release path with the exact SBPF artifact, ABI, manifests, complete checksums, build metadata, toolchain receipts, and provenance. It creates the release as a draft, uploads and verifies every asset, publishes it, and requires the resulting commit-tagged release to report immutable.

The local integrated release gate passes for this candidate: core 298 tests, signer 183 tests, updater 88 tests, deployment controller 36 tests, coding gateway 179 tests, and web 42 tests, with all typechecks, builds, process smokes, formatting, dependency checks, and Blueprint invariants passing. A separate PostgreSQL run passed all 185 signer tests, including all three migrations. These remain local results. The release candidate has not yet produced a hosted receipt or permanent escrow release, and none of these controls is proven live. The historical artifact and test counts above remain the latest hosted baseline.

## Public deployment boundary

Live probes on 23 August 2026 returned HTTP 200 from the Render website, API liveness, and closed deployment endpoints. API readiness returned HTTP 503 with protected dependencies incomplete, while public admission returned HTTP 200 with paid intake and new claims both false at revision zero. The API and web are deployed from different historical revisions, and both Render services still follow the unprotected release-candidate branch rather than protected `main`. This is a safe closed state, not proof that the current candidate or any private commercial service is deployed.

The canonical `mizuki.covenant.org` hostname still resolves through the provider's wildcard A record rather than the required service CNAME, and its TLS certificate does not match the hostname. The Render origin remains the only working public website endpoint until DNS and certificate issuance complete.

## Live devnet escrow canary

The 23 August 2026 canary passed against [devnet program `3yA83Hkj1e78J54n6DBGEJonB9Fug3XRwjGwEzxShfHn`](https://explorer.solana.com/address/3yA83Hkj1e78J54n6DBGEJonB9Fug3XRwjGwEzxShfHn?cluster=devnet). Finalized program metadata reported loader-v3, 104,376 deployed bytes, and an upgrade authority. The runner compared the deployed bytes to the hosted artifact before submitting transactions and recorded `deployedArtifactMatch: true`.

The redacted receipt reports canonical payload digest `34d2dfd348e480477a06c2b3f082b33667aeef1f88909b92e3d6b1b2451a5e67`. Its pre-execution recovery journal reports canonical payload digest `849cb9504be370dbc3ec5439692988250cbe920058d3a45dbd7fd95b3d117cc1`. Both files were written with owner-only permissions. They are not yet committed as public release artifacts, so the public signatures below are the independently inspectable evidence.

| Scenario              | Result                                                                                                                                                     |
| --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Prefunded release     | Adopted rent-safe prefunded PDAs, bound a claimant, rejected the wrong claimant, released the exact 1,000,000-lamport principal, and rejected two replays. |
| Bound expiry refund   | Funded and bound, rejected release at expiry, refunded the exact principal, closed state and vault, and retained the terminal guard.                       |
| Unbound expiry refund | Funded without a claimant, refunded the exact principal at expiry, closed state and vault, and retained the terminal guard.                                |

All 13 recorded signatures were independently queried at finalized commitment: nine intended transactions finalized with no error and four intended rejection transactions finalized with an error.

| Flow              | Action          | Expected status | Public transaction                                                                                                                                       |
| ----------------- | --------------- | --------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Prefunded release | Prefund         | Success         | [`4CrPqd…rBkY`](https://explorer.solana.com/tx/4CrPqdXxerQSrSHJp2WSK9NK8Xfd5dgkZrEhhPoLeLR5zX4eQj7am8ke5UNNagWtH28L4i5anKZaHBeMrXPwrBkY?cluster=devnet)  |
| Prefunded release | Fund            | Success         | [`4852vY…RDNF`](https://explorer.solana.com/tx/4852vYWM3E59EDRiWj4dZqisPw3zNYEdHQ5eEigRWdG6QDJ8qtXrxiAdZN1kMuA77NREGbhHJ8wf2hxCZzKdRDNF?cluster=devnet)  |
| Prefunded release | Bind            | Success         | [`65krex…eZyN`](https://explorer.solana.com/tx/65krexfEzs4bKe5kqqoY8hRyp6yxx9fAcn3272mqeQ5WK45yxQuvrJccZyvYU7x1pPVskrbRTPYwCRNh2jEoeZyN?cluster=devnet)  |
| Prefunded release | Wrong claimant  | Rejected        | [`2rm9YE…cNgpE`](https://explorer.solana.com/tx/2rm9YEBK5awFCt3vxWXRdCuBXHkwGS9YJBmC1P2gJr1hZYQU5TuiYZxfYd1xZf5Ey6pbX6vEqPddD9mejsUcNgpE?cluster=devnet) |
| Prefunded release | Release         | Success         | [`3f5LS9…7deBq`](https://explorer.solana.com/tx/3f5LS9UVCqJ3x4xkP9LGdrGBnouwKDAaCtSdB3SeLuW2Vm6KYLjdZtgHVXthsxaKhHfiegJ543U8p5MkLop7deBq?cluster=devnet) |
| Prefunded release | Replay release  | Rejected        | [`29y4iS…mVos`](https://explorer.solana.com/tx/29y4iSVXr2bbET3X6pRqrBsxwcDwFQ9XgDZ4xcohL8YmtgBZ7ZjD29NGY8QLp4LP9pfEXtwgVu5nYRjazBx5mVos?cluster=devnet)  |
| Prefunded release | Replay fund     | Rejected        | [`4eYr7y…RXC5C`](https://explorer.solana.com/tx/4eYr7yuqQ4dRiBM5Frv6unBzcL2cJu39Dqeg78hoDn383v6zYNziUuNdNTC1n19Xx6Ys8c3a9RmG6TDVhwURXC5C?cluster=devnet) |
| Bound refund      | Fund            | Success         | [`5r3Dnw…sQHQ`](https://explorer.solana.com/tx/5r3DnwThWyAuvi3svwmT9GKjmu8euGXpAJtRn2e8XbE8jgWRbHp7tpCYoMYY1CXz5tCVS4m5soJWkZ6qBk2XsQHQ?cluster=devnet)  |
| Bound refund      | Bind            | Success         | [`4ydqAN…q9FT`](https://explorer.solana.com/tx/4ydqAN4i7igxS1VfwKFEPA3FzcLoYmooLTh8C6MtGxdg4qo1tGwrhJqXRqQwdrKZ3HuW2D4D2kaka4DCqFqu9qFT?cluster=devnet)  |
| Bound refund      | Expired release | Rejected        | [`4f2Prf…KZ3h`](https://explorer.solana.com/tx/4f2PrfAvHp6uu6xUpAP8VURfdYxEFEEbx4mfmh2qaGBcjEVxU3pnjH24B46bGW2QVZSn527Aph9aHaCGmYzVKZ3h?cluster=devnet)  |
| Bound refund      | Refund          | Success         | [`4nNjno…Fhch`](https://explorer.solana.com/tx/4nNjno3WivWUE4rsnDevugPigKmWR8t9nYQ8wJHQCQfmX7BwVmDELXqM3AJ15CNnT8oJ4dix9oJh3YEjHkGqFhch?cluster=devnet)  |
| Unbound refund    | Fund            | Success         | [`3k2YfE…qEjt`](https://explorer.solana.com/tx/3k2YfEAfgzGwnCeXRwR2ffKTFniKWS7Q2fbnmFSwjNexLDyvPcqJjrq63VCQLAoNbhdmE3aQmPXQozceFy6FqEjt?cluster=devnet)  |
| Unbound refund    | Refund          | Success         | [`3gwwMR…wQcQ`](https://explorer.solana.com/tx/3gwwMRGsnXZ8SZEsBXeBmnCJ7vcb6GckxVDMXfR3qnbm3HfnTdbrvPns58AhAVYiKEqntfLHEwfxVpVpjL4kwQcQ?cluster=devnet)  |

Transaction links are evidence of program behavior only; they do not prove the external service boundary, production key custody, paid settlement, or public bounty operations.

## Protected-core assessment

### Paid maintenance engine

The source has the right commercial shape:

- Public repositories only; pull-request URLs and explicit feature or security-sensitive work are rejected.
- A repository-scoped GitHub App and human maintainer authorization are required.
- Quotes pin the repository base SHA, issue content, class, file cap, fixed USDC price, and variable-cost ceiling.
- The current release candidate requires a signer-persisted, repository-bound admission before any facilitator broadcast. Automatic and operator recovery share the same fail-closed transition, and transaction checkpoints prevent a second broadcast.
- Intake is closed by default without blocking recovery of already-settled work.
- Execution requires an authenticated persistent coding gateway, the configured marketplace route, exact sandbox resource binding, cost admission, and at least 4,000,000 provider-balance microunits reported consistently by the provider's nonbillable balance endpoint.
- Review uses a distinct model, sees exact changed bytes and validation output, and permits one bounded repair.
- Publication rechecks authorization and base SHA; a post-settlement failure enters the refund path.

The funded route canaries now establish that the selected coding route can force and return a tool call and that the selected reviewer can return strict decision JSON. They do not establish coding quality, sandbox compatibility, delivery reliability, complete cost accounting, or margin on a real repository.

### Full refund to funded rescue bounty

The source preserves the required ordering:

1. Register the exact customer liability after finalized settlement and before paid execution.
2. Serialize successful-delivery discharge against exact-principal refund.
3. Require finalized refund evidence before creating a rescue bounty.
4. Keep the bounty closed until a claimant-free on-chain escrow is finalized.
5. Bind one contributor identity and wallet proof.
6. Require qualifying merged work and independent review before release.
7. Refund expired unbound or bound escrow principal to the authority.

The devnet canary proves the on-chain release and refund primitives, including terminal closure and replay resistance. It does not prove the full customer-refund-to-public-contributor flow.

### External policy signer

The signer remains the critical trust boundary:

- It has its own authenticated service and database boundary.
- Refund and bounty authorities have separate caps, so discretionary bounty activity cannot consume protected refund capacity.
- It builds escrow instructions itself and does not trust caller-supplied serialized transactions or accounts.
- It independently verifies GitHub evidence, wallet proof, exact balances, finality, price freshness, deployed program bytes, loader state, and null mainnet upgrade authority.
- The current release candidate persists exact repository admission before payment broadcast, binds the receipt to the payment facts, and requires that durable evidence again during recovery.
- It persists signed bytes and reconciliation state before broadcast.
- It requires agreement from independent RPC and SOL/USD sources.

The devnet runner exercised local authority signing and finalized reconciliation, not a deployed external signer service. Production custody, authenticated private connectivity, two-provider disagreement drills, restart recovery, and immutable-program allowlisting remain unproven.

### Failure-to-capability updater

The updater is useful only if it stays outside the commercial trust path. The current release candidate requires signed proposal, benchmark, independent review, exact repository and pull-request identity, and required checks from pinned GitHub App and workflow producers. It rejects a candidate when the workflow ID, path, event, check-suite relationship, head or base refs and SHAs, or repository identity differs, and it rejects a candidate that changes the trusted workflow definition relative to the signed base commit. Promotion remains closed by default and still requires durable admission, shadow health, production soak, and verified rollback. Public proposals and benchmark artifacts can be shown before autonomous promotion is enabled.

## P0 launch blockers

### 1. The protected commercial services are not live together

The public API correctly remains not ready. There is no production receipt proving authenticated core-to-signer, core-to-gateway, core-to-updater, or GitHub App connectivity; signer-backed repository admission and recovery; provider-balance readiness; exact workflow-producer enforcement; restart recovery; RPC disagreement; or fail-closed startup. Deploy the private services from one verified revision with every public control closed, then prove those properties before a payment challenge can be issued.

### 2. Settlement and custody are not funded end to end

Two unrelated finalized RPC reads at slots `441258031` and `441258032` agreed that the dedicated release, refund, escrow, and job-authority addresses had zero lamports, every derived USDC account was absent, and the proposed mainnet program account did not exist. One facilitator reports exactly one eligible x402 v2 exact Solana-mainnet route with two eligible signers, but the project recipient path is not funded. The marketplace account received a finalized 50,000-microunit canonical-USDC deposit and completed two authenticated canaries, but this 0.05 USDC canary funding is 3,950,000 microunits below the production floor. The credential was intentionally not retained by the public service, so the live balance cannot currently be rechecked. Unrelated wallet balances are not project reserves and must not be moved without explicit provenance and approval.

Fund only capped canary accounts through documented provider paths. Prove a model tool call, a settlement, exact customer refund capacity, escrow funding, and provider cost receipts before opening intake.

### 3. Mainnet escrow custody is not release-ready

The devnet program is upgradeable and cannot be used as production immutability evidence. The two-RPC preflight confirms the proposed mainnet program account is absent and the dedicated deployer has zero lamports; it is proof of a blocked boundary, not deployment evidence. The release candidate defines a protected-main workflow for permanent, commit-tagged immutable escrow evidence, but that workflow has not run on the hosting platform and no permanent release exists. There is no approved mainnet program ID, independent third-party review, independent reproducible-build receipt, immutable deployment, or two-unrelated-RPC program-data match. The current mainnet program-data rent estimate for the historical artifact is 0.72766104 SOL, which exceeds the documented project-controlled deployment balance.

Complete independent review, reproduce the approved artifact, fund the ceremony wallet with explicit provenance, deploy with `--final`, and verify loader state, null upgrade authority, byte equality, raw SHA-256, and Solana executable hash through two independent finalized RPC providers before enabling escrow signing.

### 4. The two public commercial canaries have not happened

There is no public 2 USDC issue-to-quote-to-payment-to-validated-PR-to-merge receipt. There is no forced post-payment failure proving exact customer refund, funded rescue bounty, external claim, accepted PR, merge, and contributor payout. Both must run through the actual production services and project-controlled accounts. The devnet escrow canary does not count as either commercial canary or external traction.

### 5. Live route quality and complete unit economics remain unproven

The selected coding canary pinned `openai/gpt-oss-120b`, forced a tool call, matched its nonce, and reported 147 input plus 61 output tokens. The selected reviewer canary pinned `deepseek-v4-flash`, parsed strict decision JSON under a 512-token ceiling, and reported 1,177 input plus 493 output tokens. Both used the marketplace route and returned valid positive balance and provider-ID headers, but neither returned provider cost or request-ID fields. The two tested v3.2 routes failed their tool calls; the canonical route also returned conflicting duplicate balance values.

No real-repository coding plus sandbox benchmark has run. The required matrix still needs validation, repair, review, latency, sandbox seconds, billed provider cost, facilitator and chain cost, and terminal job outcome. Do not show positive gross margin until all provider adjustments, refunds, bounty liabilities, and infrastructure costs are durable and reconciled.

### 6. Traction gate is unmet

Evidence shows no qualifying paid external jobs, PRs, merges, external maintainers, refund-rate history, or verified positive margin. Launch minimum remains 10 paid jobs, at least seven PRs, at least five merges, at least three external maintainers, 100% successful principal refunds, and positive fully loaded gross margin. Operator-owned repositories and internal canaries do not count as external demand.

## Required execution sequence

1. Freeze one release candidate and rerun application CI, escrow build/tests, dependency audit, process smokes, database restore, and clean-scope packaging from that exact revision. The hosted run must exercise the new signer migration, repository-admission recovery, provider-balance readiness, and updater workflow-identity gate; statically validate the protected-main release definition before merge.
2. After protected-main merge, verify the release job publishes the approved artifact, metadata, checksums, and provenance in a commit-tagged immutable release. Obtain an independent escrow review and independent reproducible-build/hash match.
3. Create or verify the repository-scoped GitHub Apps, fund provider runway above the pinned floor and custody accounts, and benchmark the exact live model and sandbox route on a real repository.
4. Deploy the immutable mainnet program from the approved artifact and verify it through two unrelated finalized RPC providers.
5. Deploy the signer, gateway, and updater privately with intake, claims, and promotion closed. Prove readiness, disagreement, restart, restore, duplicate-request, and rollback drills.
6. Run low-value internal mainnet release and refund canaries, reconcile every balance and fee, and keep their evidence public.
7. Run the public paid success canary and forced-refund-to-funded-bounty canary. Open only a tightly capped Micro cohort if both pass.
8. Begin permission-first maintainer outreach and publish route quality, complete unit economics, refunds, PRs, and merges continuously.

## Hard stop conditions

Paid intake must remain or immediately become closed if any of the following occurs:

- refund capacity is unavailable, stale, or mismatched;
- the signer has not durably admitted the exact repository and payment facts before broadcast, or recovery cannot reproduce that admission;
- a settled payment lacks a registered liability or exceeds the recovery-age alarm;
- any refund is short, late, duplicated, sent to the wrong payer, or not finalized;
- a bounty is visible as open without matching finalized escrow evidence;
- GitHub authorization, pinned base, validation, review hash, or published bytes disagree;
- signer RPCs, price sources, program bytes, loader state, or program authority disagree;
- provider balance is below 4,000,000 microunits or its authenticated body and canonical header disagree;
- the model route or sandbox differs from the quote or exceeds its cost cap;
- a required updater check cannot be tied to the pinned App, workflow, workflow definition, pull request, base, and candidate commit;
- gross margin is shown as positive without complete durable cost receipts;
- promotion or deployment opens without recorded authorization and successful rehearsal.

The implementation now has credible devnet escrow evidence and limited funded provider-route evidence. It does not yet have the live commercial evidence required to take customer funds.
