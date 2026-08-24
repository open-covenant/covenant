# Mizuki production-readiness audit

- Audit date: 24 August 2026
- Launch deadline: 19 September 2026
- Scope: the Mizuki paid-maintenance engine, refund-to-bounty loop, external policy signer, coding gateway, updater, deployment boundary, and Solana escrow program.

## Verdict

**NO-GO for public paid intake. Production readiness: 72/100.**

The exact release candidate at revision `22850f9fe938ac57c87e3fde5c8e0e00271ee9f0` passed every hosted application, escrow, Rust, EVM, landing, dependency, and supply-chain check. Two separate GitHub-hosted runners built byte-identical 104,376-byte SBPFv2 artifacts with raw SHA-256 `2d24fd43b65a7bb31b39007b93717b1f65615df39aeec33b9eebe83bb89a2237` and Solana executable hash `42bd1e28a27ad9fe1c08f38c83008fe67db12081480b77cf4adeeeb06fcf038a`. The candidate remains unmerged because protected `main` requires an independent approval covering the final push.

This is meaningful release evidence, but it is not production proof. The historical devnet program is upgradeable. The policy signer, coding gateway, updater, GitHub Apps, model route, sandbox, and project settlement accounts are not operating together in production. There is no approved immutable mainnet program, no funded end-to-end settlement path, no independent third-party escrow review, no public paid job, and no public customer-refund-to-funded-bounty canary.

Keep paid intake, new bounty claims, and updater promotion closed. The candidate's fail-closed controls for payment admission and recovery, provider-balance admission, updater check identity, crash-safe promotion, isolated deployment roles, and permanent escrow evidence have hosted test receipts. None is proven live until the protected merge, permanent releases, closed production deployment, fault drills, funding, and public canaries complete.

| Area                                   | Score | Reason                                                                                                                                 |
| -------------------------------------- | ----: | -------------------------------------------------------------------------------------------------------------------------------------- |
| Paid maintenance engine                | 20/25 | Hosted repository, payment-recovery, gateway, and publication tests pass; no real-repository sandbox run or paid GitHub publication.   |
| Refund-to-bounty engine                | 20/25 | Hosted exactly-once recovery tests and devnet escrow flows pass; the commercial refund-to-public-bounty loop remains unproven.         |
| External policy signer                 | 20/25 | The independent evidence, custody, review, and recovery boundary passes hosted tests; the production service and custody are absent.   |
| Deployment and operations              | 10/15 | Hosted CI, restore equality, zero-authority shadow, and updater recovery pass; private services, DNS, and live promotion proof do not. |
| Live evidence, economics, and traction |  2/10 | Funded model-route canaries passed, but no paid jobs, external maintainers, complete cost receipts, or verified positive margin.       |

## Current verified release candidate

The [push Mizuki workflow](https://github.com/open-covenant/covenant/actions/runs/32677095050), [pull-request Mizuki workflow](https://github.com/open-covenant/covenant/actions/runs/32677097996), [push repository CI](https://github.com/open-covenant/covenant/actions/runs/32677095080), and [pull-request repository CI](https://github.com/open-covenant/covenant/actions/runs/32677097994) completed successfully for the exact candidate tree. Required `application`, `escrow`, `rust`, and `landing` contexts all passed. The branch is up to date with protected `main` and has no merge conflicts.

| Component                                | Hosted result                                                       |
| ---------------------------------------- | ------------------------------------------------------------------- |
| `@covenant/mizuki`                       | 312 tests passed; typecheck, build, and process smoke passed        |
| `@covenant/mizuki-policy-signer`         | 203 tests passed; typecheck and build passed                        |
| `@covenant/mizuki-updater`               | 96 tests passed; typecheck and build passed                         |
| `@covenant/mizuki-deployment-controller` | 39 tests passed; typecheck and build passed                         |
| `@covenant/coding-gateway`               | 179 tests passed; typecheck and build passed                        |
| `@mizuki/web`                            | 42 tests passed; typecheck, production build, and HTTP smoke passed |

Application total: **871 tests passed**. The workflow also exercised full data-and-sequence dump/restore equality across the four isolated test databases. This is CI restore evidence, not a managed production backup or point-in-time recovery drill.

Escrow verification passed six host tests and 25 artifact-backed SBPF tests. The hosted artifact archive records:

- Push artifact revision: `22850f9fe938ac57c87e3fde5c8e0e00271ee9f0`
- Pull-request artifact revision: `c26401c0eef01daecccd732ba5d16a26b58b17f8`
- Artifact SHA-256: `2d24fd43b65a7bb31b39007b93717b1f65615df39aeec33b9eebe83bb89a2237`
- Solana executable hash: `42bd1e28a27ad9fe1c08f38c83008fe67db12081480b77cf4adeeeb06fcf038a`
- Artifact length: 104,376 bytes
- ELF machine: registered `EM_SBF` value `263`
- SBPF version and flags: v2 and `0x2`
- Toolchain: `cargo-build-sbf 4.0.0`, platform-tools `v1.53`, Rust `1.89.0`, and `solana-verify 0.5.0`

Both archives pass their complete strict checksum manifests. Their SBPF program bytes, Cargo manifests, lockfile, and ABI are byte-identical despite building on separate hosted runners. They are retained for 90 days and have not yet been copied into a permanent release, so they remain time-limited evidence rather than a durable public transparency record.

## Verified release-candidate hardening

The current release candidate adds five controls that directly address the commercial trust boundary:

- Before a facilitator payment broadcast, the policy signer must independently revalidate and persist a repository admission bound to the quote, repository, issue, base ref and SHA, reservation-key hash, and complete payment-authorization hash. It validates the full authorization transiently but retains only a non-replayable fingerprint: message hash, client signature, fee payer, amount, and admitted payment window. Recovery first asks the signer to reconcile that fingerprint through two independent RPC providers. Confirmed absence or bounded-history exhaustion may trigger one replay of the exact durably admitted wire transaction; provider disagreement, altered evidence, or a replacement transaction fails closed. The signer still verifies the resulting finalized signature against the original fingerprint before liability registration, so neither a facilitator receipt nor a history flood can authorize arbitrary paid work.
- The updater no longer treats a check name or commit status as sufficient evidence. Each required check is pinned to an exact GitHub App, workflow ID, workflow path, event, repository, pull request, head and base refs, and head and base SHAs. It joins the check run to its workflow run through the check-suite identity and requires the workflow file at the candidate commit to match the Git blob at the signed base commit.
- Coding-gateway readiness now queries the provider's nonbillable authenticated balance endpoint and requires exactly one `X-Balance-Remaining` header to agree with a safe-integer `usdc_balance`. Readiness and run admission fail closed on a malformed, duplicated, conflicting, or insufficient result, with a floor of 4,000,000 USDC microunits.
- The protected-main workflow now defines a permanent escrow-release path with the exact SBPF artifact, ABI, manifests, complete checksums, build metadata, toolchain receipts, and provenance. It creates the release as a draft, uploads and verifies every asset, publishes it, and requires the resulting commit-tagged release to report immutable.
- Operator admission mutations now bind the exact current revision. A stale request that could open paid intake or new claims fails without mutation, while an emergency close remains fail-safe and wins over an in-flight open. Payment admission, claim binding, settlement recovery, and control mutation share one PostgreSQL advisory lock across overlapping runtime processes, and runtime predeploy refuses any open state. Every committed state is inserted atomically into an append-only PostgreSQL audit ledger whose rows reject update, delete, and truncate operations. This closes the delayed-request and rolling-deploy reopening races and makes the containment evidence required by the canary and incident runbooks durable.

The hosted integrated release gate passes with all PostgreSQL suites enabled, every typecheck and build, process smokes, formatting, the scoped production dependency audit, Blueprint invariants, and full data-and-sequence restore equality. The admission-control hardening additionally passes 318 core tests against a fresh local PostgreSQL 16 database, including the exact deployed-v1 upgrade, cross-process locking, concurrent open/close, stale-reopen rollback, restart persistence, tamper rejection, and append-only trigger coverage; its exact final head still requires the normal hosted gate. The escrow job passes six host tests and 25 LiteSVM lifecycle and adversarial tests against the exact hosted SBPFv2 artifact. These controls are not live. The candidate has not produced its protected-main permanent escrow or image release, and no private production service uses it.

## Public deployment boundary

Live probes on 24 August 2026 returned HTTP 200 from the Render website, API liveness, and closed deployment endpoints. API readiness returned HTTP 503 with protected dependencies incomplete, while public admission returned HTTP 200 with paid intake and new claims both false at revision zero. The API and web are deployed from different historical revisions, and both Render services still follow the unprotected release-candidate branch rather than protected `main`. This is a safe closed state, not proof that the current candidate or any private commercial service is deployed.

A read-only production metadata probe confirmed the core database is on migration `commercial-core` v1 with SHA-256 `1e1c7b752aead2d673a8d82fba69113344ada76444a1263e6bc80bffb0d80429`, the exact column shape covered by the upgrade test, no admission-control audit component or table, and both controls closed at revision zero. This establishes a verified audit-migration starting point; it does not deploy or activate the candidate.

The closed bootstrap Blueprint now validates against the bound Render workspace and plans exactly three private services plus two isolated databases. The full production Blueprint fails validation because the required `mizuki-ghcr` registry credential does not exist. No Mizuki Blueprint owns the current resources, and none of the signer, gateway, updater, controller, shadow runtime, production image runtime, or isolated financial databases exists. The three required GitHub Apps and their organization installations are also absent.

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

### 0. The protected release is not approved or merged

[Pull request 147](https://github.com/open-covenant/covenant/pull/147) is conflict-free, up to date with `main`, and green on every required hosted context. It has no submitted review. Protected `main` requires one approval, dismisses stale approvals, requires approval after the final push by someone other than the pusher, enforces the rule for administrators, and requires linear history. No deployment or permanent release may use the candidate until that exact final head receives a qualifying independent approval and merges through the normal squash or rebase path.

### 1. The protected commercial services are not live together

The public API correctly remains not ready. Only the historical source-built API, website, and canonical commercial database exist on Render. There is no production receipt proving authenticated core-to-signer, core-to-gateway, core-to-updater, or GitHub App connectivity; signer-backed repository admission and recovery; provider-balance readiness; exact workflow-producer enforcement; restart recovery; RPC disagreement; or fail-closed startup. The closed bootstrap plan validates, but the full plan cannot validate until the registry credential exists. Deploy the private services from one verified revision with every public control closed, then prove those properties before a payment challenge can be issued.

### 2. Settlement and custody are not funded end to end

Two unrelated finalized RPC reads agreed at slot `441281187` that the dedicated release, refund, escrow, and job-authority addresses, every derived USDC account, and the proposed mainnet program account were absent. One facilitator reports exactly one eligible x402 v2 exact Solana-mainnet route with two eligible signers; its selected fee payer held 2,956,725,332 lamports by both providers. That funds facilitator fees only. The project recipient, refund, escrow, job, and deployment paths remain unfunded. The marketplace account received a finalized 50,000-microunit canonical-USDC deposit and completed two authenticated canaries, but this 0.05 USDC canary funding is at least 3,950,000 microunits below the production floor. The credential was intentionally not retained by the public service, so the live balance cannot currently be rechecked. Unrelated wallet balances are not project reserves and must not be moved without explicit provenance and approval.

Fund only capped canary accounts through documented provider paths. Prove a model tool call, a settlement, exact customer refund capacity, escrow funding, and provider cost receipts before opening intake.

### 3. Mainnet escrow custody is not release-ready

The devnet program is upgradeable and cannot be used as production immutability evidence. The two-RPC preflight confirms the proposed mainnet program account is absent and the dedicated deployer has zero lamports; it is proof of a blocked boundary, not deployment evidence. The release candidate defines a protected-main workflow for permanent, commit-tagged immutable escrow evidence, but that workflow has not run on the hosting platform and no permanent release exists. Two canonical-workflow runners produced identical artifacts, but there is still no approved mainnet program ID, independent third-party review, independently operated reproducible-build receipt, immutable deployment, or two-unrelated-RPC program-data match. Current permanent rent is 0.72766104 SOL for program data plus 0.00114144 SOL for the loader Program account, or 0.72880248 SOL before fees and ceremony working capital. The committed deployer floor is 0.75 SOL and the dedicated deployer has no account.

Complete independent review, reproduce the approved artifact, fund the ceremony wallet with explicit provenance, deploy with `--final`, and verify loader state, null upgrade authority, byte equality, raw SHA-256, and Solana executable hash through two independent finalized RPC providers before enabling escrow signing.

### 4. The two public commercial canaries have not happened

There is no public 2 USDC issue-to-quote-to-payment-to-validated-PR-to-merge receipt. There is no forced post-payment failure proving exact customer refund, funded rescue bounty, external claim, accepted PR, merge, and contributor payout. Both must run through the actual production services and project-controlled accounts. The devnet escrow canary does not count as either commercial canary or external traction.

### 5. Live route quality and complete unit economics remain unproven

The selected coding canary pinned `openai/gpt-oss-120b`, forced a tool call, matched its nonce, and reported 147 input plus 61 output tokens. The selected reviewer canary pinned `deepseek-v4-flash`, parsed strict decision JSON under a 512-token ceiling, and reported 1,177 input plus 493 output tokens. Both used the marketplace route and returned valid positive balance and provider-ID headers, but neither returned provider cost or request-ID fields. The two tested v3.2 routes failed their tool calls; the canonical route also returned conflicting duplicate balance values.

No real-repository coding plus sandbox benchmark has run. The required matrix still needs validation, repair, review, latency, sandbox seconds, billed provider cost, facilitator and chain cost, and terminal job outcome. Do not show positive gross margin until all provider adjustments, refunds, bounty liabilities, and infrastructure costs are durable and reconciled.

### 6. Traction gate is unmet

Evidence shows no qualifying paid external jobs, PRs, merges, external maintainers, refund-rate history, or verified positive margin. Launch minimum remains 10 paid jobs, at least seven PRs, at least five merges, at least three external maintainers, 100% successful principal refunds, and positive fully loaded gross margin. Operator-owned repositories and internal canaries do not count as external demand.

## Required execution sequence

1. **Completed for the previously audited code revision:** application CI, escrow build/tests, dependency audit, process smokes, database restore, clean-scope packaging, signer migrations, repository-admission recovery, provider-balance readiness, updater workflow identity, and protected-main release validation all passed on hosted runners. Revision-safe admission and its append-only audit pass locally against a fresh PostgreSQL database; the exact resulting head must repeat every required hosted check before merge.
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
- an admission mutation is not bound to the current revision or its committed audit row cannot be read unchanged.

The implementation now has credible devnet escrow evidence and limited funded provider-route evidence. It does not yet have the live commercial evidence required to take customer funds.
