# Production environment contract

Live values belong in Render's encrypted settings and the operator password manager. Never commit addresses tied to a person, private keys, bearer tokens, webhook secrets, or signer mappings with personal labels.

## Cross-service invariants

| Producer                            | Consumer                                 | Required relationship                                                                                                        |
| ----------------------------------- | ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| Signer `MIZUKI_SIGNER_AUTH_TOKEN`   | API `MIZUKI_POLICY_SIGNER_TOKEN`         | Blueprint `fromService` link; at least 32 random characters.                                                                 |
| Signer private host and port        | API `MIZUKI_POLICY_SIGNER_URL`           | Blueprint private service discovery only. No public signer route.                                                            |
| Gateway `CODER_AUTH_TOKEN`          | API `MIZUKI_CODING_GATEWAY_TOKEN`        | Blueprint `fromService` link. The browser never receives it.                                                                 |
| Updater `MIZUKI_UPDATER_READ_TOKEN` | API `MIZUKI_UPDATER_TOKEN`               | Read-only credential only. The API never receives updater submission authority.                                              |
| API `MIZUKI_PAY_TO`                 | Signer `MIZUKI_REFUND_TREASURY`          | Exact same wallet. Paid admission fails closed if signer readiness reports another treasury.                                 |
| ClawPump payout wallet              | Signer `MIZUKI_ESCROW_AUTHORITY`         | Register this exact public key through ClawPump's signed wallet flow. API readiness rejects a different signer authority.    |
| API x402 asset                      | Signer `MIZUKI_REFUND_MINT`              | Canonical mainnet USDC mint, six decimals, legacy Token Program. Verify the address on-chain.                                |
| API `MIZUKI_JOB_AUTHORITY_SEED`     | Signer `MIZUKI_JOB_AUTHORITY_PUBLIC_KEY` | Dedicated Ed25519 pair used only for refund-liability register, execute, and discharge requests.                             |
| Web x402 challenge                  | API x402 challenge                       | v2 exact SVM route, fixed quote amount, same asset and recipient. The browser rejects drift before signing.                  |
| API GitHub App                      | Maintainer authorization receipt         | Installed on the target public repository; label event actor retains triage-or-higher permission at payment and publication. |

The API, signer, and updater have separate PostgreSQL databases and credentials. No component receives another component's database URL. The refund custody key, escrow custody key, and job-authority key must all be distinct.

Generate the job-authority seed once, store it immediately, and derive only its public key for the signer:

```sh
authority_seed=$(openssl rand -base64 32 | tr -d '\n')
MIZUKI_JOB_AUTHORITY_SEED="$authority_seed" pnpm --filter @covenant/mizuki build
MIZUKI_JOB_AUTHORITY_SEED="$authority_seed" pnpm --filter @covenant/mizuki job-authority-public
```

Store `authority_seed` as the API secret and the command output as the signer public key. Never reuse either Solana custody key.

## API

| Variable                                                               | Source                 | Pre-deploy proof                                                                         |
| ---------------------------------------------------------------------- | ---------------------- | ---------------------------------------------------------------------------------------- |
| `MIZUKI_PUBLIC_BASE_URL`                                               | Fixed HTTPS URL        | Exact public API origin used in x402 resource binding.                                   |
| `MIZUKI_WEB_ORIGIN`                                                    | Fixed HTTPS URL        | Exact web origin; no wildcard CORS.                                                      |
| `MIZUKI_TRUSTED_PROXY_HOPS`                                            | Fixed integer          | Explicitly `1` on Render. Missing proxy metadata falls back to the direct socket source. |
| `MIZUKI_RATE_LIMIT_MAX_SOURCES`                                        | Fixed capacity         | `10000`; excess sources share a bounded overflow bucket instead of allocating memory.    |
| `MIZUKI_SSE_MAX_CONNECTIONS` / `MIZUKI_SSE_MAX_CONNECTIONS_PER_SOURCE` | Fixed capacities       | `100` globally and `3` per source; excess streams return `429`.                          |
| `MIZUKI_SSE_IDLE_TIMEOUT_MS`                                           | Fixed duration         | `120000`; streams close after two minutes without a new activity event.                  |
| `MIZUKI_READINESS_REFRESH_MS` / `MIZUKI_READINESS_MAX_AGE_MS`          | Fixed durations        | `30000` / `90000`; stale complete dependency evidence returns `503`.                     |
| `MIZUKI_READINESS_TIMEOUT_MS`                                          | Fixed duration         | `20000`; a hanging dependency fails the current readiness attempt.                       |
| `MIZUKI_ESCROW_READINESS_MIN_LAMPORTS`                                 | Fixed reserve floor    | `1000000000`; paid admission requires signer-reported escrow capacity above this floor.  |
| `MIZUKI_DATABASE_URL`                                                  | API database           | TLS connection, schema migration, restart recovery, and unique-payment tests pass.       |
| `MIZUKI_ADMIN_TOKEN`                                                   | Generated secret       | At least 32 characters; absent from web and logs.                                        |
| `MIZUKI_CODING_GATEWAY_URL` / `MIZUKI_CODING_GATEWAY_TOKEN`            | Private service links  | Authenticated health, submit, status, and artifact benchmark pass.                       |
| `USEPOD_API_KEY`                                                       | Secret                 | Independent review request succeeds through the documented OpenAI-compatible interface.  |
| `USEPOD_MODEL`                                                         | Pinned route           | Coding route cleared the published Micro/Standard benchmark.                             |
| `USEPOD_REVIEW_MODEL`                                                  | Different pinned route | Must not equal the coding route; clean-context review benchmark passes.                  |
| `MIZUKI_PAY_TO`                                                        | Signer service link    | Equals signer refund treasury and has a canonical USDC associated token account.         |
| `MIZUKI_X402_FACILITATOR`                                              | Fixed HTTPS URL        | `/supported` advertises x402 v2, exact SVM mainnet, and a valid fee payer.               |
| `MIZUKI_POLICY_SIGNER_URL` / `MIZUKI_POLICY_SIGNER_TOKEN`              | Private service links  | Readiness succeeds and reports matching treasury, mint, decimals, and available reserve. |
| `MIZUKI_JOB_AUTHORITY_SEED`                                            | Secret                 | Canonical base64 for exactly 32 bytes; derived public key matches signer setting.        |
| `MIZUKI_GITHUB_APP_ID` / `MIZUKI_GITHUB_PRIVATE_KEY`                   | Secrets                | Repository installation token can read issue events and publish a branch and PR.         |
| `MIZUKI_GITHUB_CLIENT_ID` / `MIZUKI_GITHUB_CLIENT_SECRET`              | Secrets                | OAuth callback is exactly `/v1/auth/github/callback`.                                    |
| `MIZUKI_GITHUB_WEBHOOK_SECRET`                                         | Secret                 | Signed pull-request webhook replay passes; invalid signatures fail.                      |
| `MIZUKI_SESSION_SECRET`                                                | Generated secret       | At least 32 characters; absent from signer, gateway, and updater.                        |
| `MIZUKI_UPDATER_URL` / `MIZUKI_UPDATER_TOKEN`                          | Private service links  | Authenticated read works; proposal submission with this token returns unauthorized.      |
| `MIZUKI_INTERNAL_REPOS`                                                | Fixed list             | Contains every operator-controlled repository so they cannot count as external traction. |

The GitHub App needs repository Contents read/write, Issues read, Pull requests read/write, Metadata read, and Members read. Subscribe only to Pull request events. Public v1 intake remains installation- and label-authorized; Mizuki never opens unsolicited PRs.

The database initializes paid intake and new bounty claims as closed. `GET /v1/admission` exposes the non-sensitive state. Only an authenticated operator may change it through `POST /v1/admin/admission`; every change requires a reason and is serialized with payment admission and claim binding. A missing or unreadable control row blocks both paths. Existing jobs, refunds, active-claim PR submission, and disputes remain recoverable while new intake is paused.

`CLAWPUMP_PAYOUT_WALLET` is always linked to the signer escrow authority in the Blueprint. `CLAWPUMP_API_KEY`, `CLAWPUMP_AGENT_ID`, and `MIZUKI_TOKEN_MINT` remain unset until their operator-owned launch steps are complete. Before setting the agent ID, an operator must register the exact escrow authority through the ClawPump dashboard or `set_external_wallet` signed flow, retain the response, and verify one distribution against finalized chain history. Mizuki never receives the wallet private key. The earnings API is platform-reported accounting; only the escrow funding transaction proves bounty custody.

## Policy signer

| Variable                                                              | Pre-deploy proof                                                                             |
| --------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `MIZUKI_SIGNER_DATABASE_URL`                                          | Dedicated TLS database; operation leases and restart reconciliation pass.                    |
| `MIZUKI_SIGNER_RPC_URL` / `MIZUKI_SIGNER_SECONDARY_RPC_URL`           | Distinct finalized-history providers return equal program and settlement facts.              |
| `MIZUKI_REFUND_PRIVATE_KEY_JSON`                                      | Dedicated low-balance key; public key equals `MIZUKI_REFUND_TREASURY`.                       |
| `MIZUKI_ESCROW_PRIVATE_KEY_JSON`                                      | Different dedicated key; public key equals `MIZUKI_ESCROW_AUTHORITY`.                        |
| `MIZUKI_SIGNER_GITHUB_TOKEN`                                          | Read-only public-repository metadata access; no content, workflow, or administration writes. |
| `MIZUKI_JOB_AUTHORITY_PUBLIC_KEY`                                     | Exact public key derived from the API seed; not a custody key.                               |
| `MIZUKI_REFUND_TREASURY`                                              | Holds enough canonical USDC for every registered liability plus operating headroom.          |
| `MIZUKI_REFUND_MINT`                                                  | Canonical mainnet USDC mint; six decimals; `spl-token`.                                      |
| `MIZUKI_ESCROW_PROGRAM_ID`                                            | Independently reviewed immutable loader-v3 program deployed from the verified artifact.      |
| `MIZUKI_ESCROW_PROGRAM_DATA_SHA256`                                   | SHA-256 of executable bytes after the 45-byte loader metadata prefix.                        |
| `MIZUKI_SOL_USD_PRICE_URL` / `MIZUKI_SOL_USD_SECONDARY_PRICE_URL`     | Two distinct HTTPS providers; bounded, recent response fixtures pass.                        |
| `MIZUKI_SOL_USD_PRICE_TOKEN` / `MIZUKI_SOL_USD_SECONDARY_PRICE_TOKEN` | Separate secrets when feeds require authentication.                                          |
| `MIZUKI_SOL_USD_MAX_DIVERGENCE_BPS`                                   | Exactly `500`; greater disagreement fails closed.                                            |

Keep `MIZUKI_SIGNER_MOCK_MODE=false`, the per-operation ceiling at $25, and both rolling 24-hour ceilings at $100 through the event. Raising a ceiling is a reviewed policy change, never an incident workaround.

## Coding gateway

| Variable                         | Pre-deploy proof                                                               |
| -------------------------------- | ------------------------------------------------------------------------------ |
| `CODER_AUTH_TOKEN`               | Generated secret linked only to the API.                                       |
| `CODER_BACKEND`                  | Exactly `usepod`.                                                              |
| `CODER_MODEL` / `USEPOD_API_KEY` | Pinned marketplace route passes the benchmark and cost cap.                    |
| `E2B_API_KEY`                    | Restricted sandbox account with spend alerts and no production credentials.    |
| `E2B_EGRESS_ALLOW`               | Only package registries and public source hosts required by accepted work.     |
| `LEDGER_PATH` / `RUN_STORE_PATH` | Separate files under `/var/data`; restart and corruption tests pass.           |
| Gateway readiness durations      | Refresh `120000`, maximum age `300000`, timeout `20000`; stale evidence fails. |
| Spend and concurrency limits     | $4 per run, $25 per day, $500 per month, two concurrent runs.                  |

The gateway receives no GitHub App key, treasury key, signer token, updater token, or database URL. Its authenticated readiness path refreshes both the exact UsePod model catalog and E2B create/exec/destroy evidence; it never returns upstream response bodies. A completed response is not visible until its durable artifact receipt is written.

## Web

| Variable                              | Pre-deploy proof                                                                                                    |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `MIZUKI_API_URL`                      | Server-side API URL; public proxy allowlist rejects admin paths.                                                    |
| `NEXT_PUBLIC_MIZUKI_APP_URL`          | Exact HTTPS web origin.                                                                                             |
| `NEXT_PUBLIC_MIZUKI_GITHUB_OAUTH_URL` | Same-origin public API proxy path.                                                                                  |
| `NEXT_PUBLIC_SOLANA_NETWORK`          | Exactly `solana` for production.                                                                                    |
| `NEXT_PUBLIC_SOLANA_RPC_URL`          | Production mainnet RPC suitable for browser mint and blockhash reads; public by design, never embed a secret token. |

The web service receives no private key or internal bearer token. The wallet signs the official x402 SVM transaction; the server-side proxy forwards only the allowlisted v2 payment headers.

## Updater

| Variable                                                               | Pre-deploy proof                                                               |
| ---------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| `MIZUKI_UPDATER_AUTH_TOKEN`                                            | Write/admin token held only by the external release operator.                  |
| `MIZUKI_UPDATER_READ_TOKEN`                                            | Distinct read-only token linked to the API.                                    |
| `MIZUKI_UPDATER_PROPOSAL_KEYS_JSON`                                    | Trusted Ed25519 proposal authorities only.                                     |
| `MIZUKI_UPDATER_BENCHMARK_KEYS_JSON`                                   | Separate benchmark authorities.                                                |
| `MIZUKI_UPDATER_REVIEW_KEYS_JSON`                                      | Separate independent review authorities.                                       |
| `MIZUKI_UPDATER_GITHUB_APP_ID` / `MIZUKI_UPDATER_GITHUB_PRIVATE_KEY`   | App can write only allowlisted repositories and `mizuki/capability/` branches. |
| `MIZUKI_UPDATER_SHADOW_HOOK_URL`                                       | Creates an isolated candidate deployment for the exact proposal SHA.           |
| `MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE`                            | Contains literal `{deploymentId}` and reports candidate health.                |
| `MIZUKI_UPDATER_PROMOTE_HOOK_URL` / `MIZUKI_UPDATER_ROLLBACK_HOOK_URL` | Promote the reviewed SHA or restore the recorded healthy SHA.                  |
| `MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE`                         | Distinct endpoint proving the merge and promotion are active in production.    |
| `MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN`                                     | Dedicated token accepted only by the configured deployment origin.             |
| `MIZUKI_UPDATER_PROMOTION_SOAK_MS`                                     | `120000`; exact candidate stays continuously healthy before completion.        |
| `MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS`                                  | `600000`; bounded verification ends in rollback, never implicit completion.    |

The updater never receives signer custody, signer auth, treasury, x402, gateway, or API admin secrets. The API observes signed updater evidence with a read-only token; it cannot propose, approve, mutate promotion control, merge, deploy, or roll back an upgrade. Promotion control is database-backed and closed by default. The external release operator must enable an exact revision with a reason through authenticated `PUT /v1/admin/promotion-control`; close it again after the observed release window. Promotion admission and control mutation serialize through one database advisory gate. A closed control blocks only a new promotion hook. Health monitoring and rollback continue for an already promoted candidate.

Promotion hook success enters durable health verification. The production endpoint must report `environment: production`, the exact candidate and merge SHAs, the persisted promotion operation ID, and `active: true` for every healthy observation. Shadow evidence cannot satisfy this gate. Regression, timeout, or exhausted retries invokes rollback before the record can become `completed`.

## Rotation order

1. Stop new paid intake and reconcile every accepted liability, job, escrow, and signer operation.
2. Rotate the signer bearer token through the Blueprint link and confirm unauthorized calls fail.
3. Rotate gateway and updater read tokens independently and repeat their negative authorization tests.
4. Rotate GitHub App keys and webhook secret, then replay a signed delivery.
5. Rotate model, sandbox, ClawPump, RPC, and price-source credentials independently.
6. Rotate refund custody, escrow custody, and job authority separately. Update the matching public setting atomically and run both canaries again before reopening intake.
7. Record role, timestamp, old fingerprint, and new fingerprint. Never record secret material.
