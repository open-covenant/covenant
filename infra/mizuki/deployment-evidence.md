# Mizuki deployment evidence

- Evidence date: 24 August 2026
- Verified web revision: `3b865484c7f9b1d469782437cd858f043cd2f251`
- Verified API revision: `4fb0378f879c2eb38b8b6c611cc35d940de00029`
- Launch verdict: **NO-GO for public paid intake**

This record distinguishes verified public infrastructure from the incomplete commercial path. Healthy website and API processes do not authorize payment intake, bounty claims, or autonomous promotion.

## Currently deployed historical provenance

The entries in this section describe the closed bootstrap services currently on Render. They are historical deployment evidence, not the current release candidate and not authority for mainnet deployment. The newer hosted candidate and escrow artifact recorded in `docs/production-audit-mizuki.md` supersede them for source review; neither candidate is live until a separately recorded deployment completes.

| Evidence                           | Verified result                                                                                                                        |
| ---------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Identity release                   | [Run 32654849761](https://github.com/open-covenant/covenant/actions/runs/32654849761) completed successfully at revision `3b865484`    |
| Identity repository CI             | [Run 32654849740](https://github.com/open-covenant/covenant/actions/runs/32654849740) completed successfully at revision `3b865484`    |
| Mizuki workflow                    | [Run 32645440904](https://github.com/open-covenant/covenant/actions/runs/32645440904) completed successfully at revision `ada1d5e7d`   |
| Repository CI                      | [Run 32645440926](https://github.com/open-covenant/covenant/actions/runs/32645440926) completed successfully at revision `ada1d5e7d`   |
| Application verification           | 576 tests passed across 76 files                                                                                                       |
| Process verification               | Compiled API startup, standalone web HTTP semantics, and clean shutdown passed                                                         |
| Database recovery                  | Full normalized data and sequence restore comparison passed for all three application databases                                        |
| Escrow host verification           | 6 host tests passed                                                                                                                    |
| Escrow SBPF verification           | 24 artifact-backed tests passed                                                                                                        |
| Historical escrow artifact SHA-256 | `075302c38b9f895ea4aa94b9c61d9d83f98b85fd8f5e2cd0e915d08430b92b3f`, unchanged across the builds represented by this historical section |

The matching artifact digest proves reproducibility for the hosted build inputs. It does not prove that those bytes are deployed immutably on mainnet or that a live signer is pinned to them.

## Audited release candidate

[Pull request 147](https://github.com/open-covenant/covenant/pull/147) contains the current commercial-core candidate. Its audited code revision is `22850f9fe938ac57c87e3fde5c8e0e00271ee9f0`. The separate push and pull-request Mizuki workflows and repository CI workflows passed every required context, including PostgreSQL-backed application suites, full database dump/restore equality, the scoped production dependency audit, process smokes, Blueprint invariants, Rust, EVM, landing, and Socket checks.

The two hosted escrow runners produced byte-identical program artifacts, Cargo manifests, lockfile, and ABI. Both report artifact SHA-256 `2d24fd43b65a7bb31b39007b93717b1f65615df39aeec33b9eebe83bb89a2237` and Solana executable hash `42bd1e28a27ad9fe1c08f38c83008fe67db12081480b77cf4adeeeb06fcf038a`, and both complete checksum manifests pass. This is repeatability across canonical hosted runners, not an independent third-party audit or on-chain deployment receipt.

The pull request is conflict-free and up to date with `main`, but it has no submitted review. Protected `main` requires a qualifying independent approval after the final push. No service, permanent release, or mainnet ceremony may consume this candidate before that gate passes.

## Funded provider route canaries

The provider account was activated through a finalized 50,000-microunit canonical-USDC `DepositUsdc` transaction on Solana mainnet. The public transaction is [`ExRVdg…Ro3ih`](https://explorer.solana.com/tx/ExRVdguFoDeHTCF9P1yfKozcpxML9Y4s1WdzYTFDdeRcMVkjVGwP4qwmYJpPc4DBwtcbuQwt3QdTNh4KdzRo3ih), finalized at slot `441239653` with block time `2026-08-23T20:47:54Z` through sovereign program `BBAdcqUkg68JXNiPQ1HR1wujfZuayyK3eQTQSYAh6FSW`. The token became active and the authenticated catalog returned 1,013 models.

| Route    | Selected model        | Funded canary result                                                                                                                                        |
| -------- | --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Coding   | `openai/gpt-oss-120b` | Marketplace route forced a readiness tool call, matched its nonce, and reported usage of 147 input plus 61 output tokens.                                   |
| Reviewer | `deepseek-v4-flash`   | Marketplace route parsed strict decision JSON under `max_tokens: 512` and reported usage of 1,177 input plus 493 output tokens.                             |
| Both     | —                     | Valid positive balance and provider-ID headers were present; provider cost and request-ID fields were absent, so this is not complete unit-economics proof. |

On 24 August, the coding route was re-qualified and pinned to `deepseek-v3.2`. A funded marketplace-only, no-retention request returned the canonical `deepseek.v3.2` identity, a valid `read_file` tool call, provider `7ae6c424-9863-4563-8863-19a06c65d00b`, request `01M0TD4706S5G6AHQVQZ5Y4KKY`, and token usage of 309 input plus 21 output tokens in 7.3 seconds.

The unqualified `deepseek-v3.2` request canonicalized and failed the tool call. The canonical `deepseek/deepseek-v3.2` request also failed the tool call and returned conflicting duplicate balance values. Neither is approved. The redacted artifact is content-addressed at `https://raw.githubusercontent.com/open-covenant/covenant/main/infra/mizuki/evidence/usepod-route-2026-08-23.json#sha256=21bbff5860332305ec090c9bb8245de36e6e53819a97d29401724c5c3644c441`; it retains no credential, deposit code, account balance, or provider identifier.

This advances route selection, not launch readiness. Canary funding is 0.05 USDC, below the configured 4,000,000-microunit production floor, and no real-repository coding plus sandbox benchmark has passed. Paid intake remains closed.

The 24 August read-only recheck found the required x402 v2 exact mainnet route still advertised with two eligible signers. Two unrelated finalized RPC providers agreed that its selected fee payer held 2,956,725,332 lamports. That is facilitator fee capacity, not Mizuki refund, escrow, settlement, or provider runway.

## Mainnet release preflight

Independent finalized reads through `api.mainnet-beta.solana.com` and `solana-rpc.publicnode.com` agreed at slots `441245625` and `441245514`: the proposed mainnet program account did not exist; the dedicated deployer, refund treasury, escrow authority, and job authority each had zero lamports; and all four derived canonical-USDC accounts were absent. The redacted result is content-addressed at `https://raw.githubusercontent.com/open-covenant/covenant/main/infra/mizuki/evidence/mainnet-preflight-2026-08-23.json#sha256=8debdc14b45ec698f0af45f1a758036d5c343bf3abe4c66fc2174fde17cbe70e`.

This is a fail-closed funding and non-deployment receipt. It does not authorize a deploy. The release ceremony still requires at least 750,000,000 lamports in the dedicated deployer, independent review, a hosted reproducible build for the exact release revision, final deployment with no upgrade authority, and byte/hash agreement through two unrelated finalized RPC providers.

## 24 August mainnet readiness recheck

The [current redacted receipt](https://raw.githubusercontent.com/open-covenant/covenant/main/infra/mizuki/evidence/mainnet-readiness-2026-08-24.json#sha256=e345f8249d16671c4474542c9ad8b6d9d06b95cea231821f7a74a2bad40d77e9) records a second read-only no-go observation. Both unrelated providers returned the canonical mainnet genesis hash and agreed at finalized slot `441281187` that the proposed program, four project authorities, and four canonical-USDC accounts were all absent.

Current permanent deployment rent is 727,661,040 lamports for the 104,421-byte program-data account plus 1,141,440 lamports for the 36-byte loader Program account: 728,802,480 lamports before transaction fees or ceremony working capital. The committed deployment floor remains 750,000,000 lamports and the dedicated deployer remains absent. The receipt also binds the facilitator observation, the historical 50,000-microunit marketplace deposit, the unproven 4,000,000-microunit live balance floor, and the still-unauthenticated sandbox tariff boundary.

## Render state

| Resource             | Identifier / address                                                                      | Verified state                                                         |
| -------------------- | ----------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| Website              | `srv-da5egcrl550s73cmhqh0` · [mizuki-9by5.onrender.com](https://mizuki-9by5.onrender.com) | Deploy `dep-da5ir6gjo6nc73cmtrmg` live from `3b865484`; Starter        |
| Website health       | [`/healthz`](https://mizuki-9by5.onrender.com/healthz)                                    | HTTP 200                                                               |
| Profile image        | [`/mizuki-avatar.jpg`](https://mizuki-9by5.onrender.com/mizuki-avatar.jpg)                | HTTP 200 JPEG; canonical bytes verified by SHA-256                     |
| API                  | `srv-da5fg02jobas73ei3n30` · [mizuki-api.onrender.com](https://mizuki-api.onrender.com)   | Deploy `dep-da5jc5rbc2fs7399emlg` live from `4fb0378f`; Starter        |
| API liveness         | [`/healthz`](https://mizuki-api.onrender.com/healthz)                                     | HTTP 200                                                               |
| API deploy safety    | [`/deployz`](https://mizuki-api.onrender.com/deployz)                                     | HTTP 200                                                               |
| API readiness        | [`/readyz`](https://mizuki-api.onrender.com/readyz)                                       | HTTP 503; protected dependencies intentionally incomplete              |
| Admission            | [`/v1/admission`](https://mizuki-api.onrender.com/v1/admission)                           | Intake false; claims false; revision zero                              |
| Commercial counters  | [`/v1/metrics`](https://mizuki-api.onrender.com/v1/metrics)                               | All job, PR, refund, bounty, maintainer, revenue, and flow counts zero |
| Paid Postgres        | `dpg-da5ekm8u01pc73evr3d0-a`                                                              | Available; Basic 256 MB; PostgreSQL 16; 5 GB; Frankfurt                |
| Custom domain        | `mizuki.covenant.org`                                                                     | Attached in Render; DNS not verified                                   |
| Required DNS         | CNAME host `mizuki` → `mizuki-9by5.onrender.com`                                          | Pending at the authoritative DNS provider                              |
| Policy signer        | —                                                                                         | Not deployed                                                           |
| Coding gateway       | —                                                                                         | Not deployed                                                           |
| Updater              | —                                                                                         | Not deployed                                                           |
| Controller           | —                                                                                         | Not deployed                                                           |
| Image runtimes       | —                                                                                         | Shadow and production services not deployed                            |
| Bootstrap Blueprint  | `infra/mizuki/render-bootstrap.yaml`                                                      | Valid; plans three private services and two isolated databases         |
| Production Blueprint | `infra/mizuki/render.yaml`                                                                | Invalid until the `mizuki-ghcr` registry credential exists             |
| GitHub Apps          | Core, Policy Verifier, Updater                                                            | No matching Apps or organization installations exist                   |

Both currently hosted Render services have automatic deployment disabled and still point at the unprotected release-candidate branch. They must move to protected `main` only through the reviewed Blueprint promotion. The source-built API is a closed bootstrap deployment, not the production target: it must remain closed and be suspended during the image-runtime cutover. The production Blueprint now permits only `mizuki-runtime-production` to serve the website and use the canonical commercial database. The website currently has demo mode disabled, targets the closed bootstrap API, and shares a valid service-held proxy secret with it. Public verification confirmed:

- the homepage renders live zero-state metrics without demo or stale-API warnings;
- the canonical profile image appears in the header, footer, browser icons, install manifest, and social metadata;
- the live profile image SHA-256 is `f01397753222adc7ab4aebc784cd3ae7faec557f73a4d4a6f2c1f1b7f0423505`;
- `/work` visibly reports closed intake and omits quote and payment controls;
- a valid quote request through the web proxy returns HTTP 503 with intake paused before GitHub or payment work;
- missing valid-UUID job and bounty receipt routes return HTTP 404 with normal and empty User-Agent headers;
- all expected index pages, health, robots, and manifest routes return HTTP 200;
- browser inspection found the expected live state and no console errors.

An earlier release candidate failed at process startup before it could serve traffic. No commercial workflow ran. The final verifier now boots the compiled API and standalone web server as hosted gates, which caught and prevents recurrence of both that startup failure and streamed receipt soft-404s.

No secret values, connection strings, signing material, or provider credentials belong in this record.

The canonical asset is ready for ClawPump's documented `upload_agent_avatar` flow. No authenticated Agent MCP key or agent ID is configured in this workspace, so an external profile mutation is not claimed. The checked-in setup requires `get_agent_asset_url` and `get_agent` verification after upload.

## Custom-domain boundary

Render has accepted `mizuki.covenant.org`, but the hostname is not live. Its authoritative nameservers are outside the available Cloudflare zone, and DNS still resolves through the existing wildcard A record. The only authorized pending change is:

- record type: CNAME;
- host/name: `mizuki`;
- target/value: `mizuki-9by5.onrender.com`;
- TTL: provider default or 3600.

Do not alter the apex, nameservers, mail records, or wildcard record. Render can verify and provision the certificate after the authoritative record propagates.

## Closed controls

The following controls remain closed:

- paid job intake;
- new bounty claims;
- updater promotion.

They must remain closed while the signer, gateway, updater, immutable program identity, production provider runway, real-repository route benchmark, and funded custody reserves are absent. The API is intentionally useful for public evidence while refusing new economic work.

## Remaining launch blockers

1. Deploy the policy signer, coding gateway, updater, controller, shadow, and sole image-backed production runtime from a hosted-verified revision; cut the website over and suspend the source-built bootstrap API before any paid traffic.
2. Raise provider funding above the 4,000,000-microunit floor, configure the sandbox, facilitator, independent RPC, and price providers with production-scoped keys, and pass a real-repository benchmark without exposing credentials.
3. Fund tightly capped customer-refund, bounty-SOL, settlement, and transaction-fee reserves and prove signer capacity checks against them.
4. Complete independent escrow review, adversarial devnet canaries, reproducible verification, and an immutable mainnet program deployment pinned by the signer.
5. Verify the custom-domain CNAME and perform platform restart plus backup/PITR recovery drills for every financial database.
6. Publish the successful paid PR canary and the forced-refund-to-funded-bounty canary through the actual production services.
7. Meet the external traction and fully loaded positive-margin gates before opening public paid intake.

Until all blockers are resolved, hosted build, API, and website evidence must not be presented as proof that the paid maintenance engine is operational.
