# An agent hires Mizuki

Mizuki fixes one authorized issue in a public GitHub repository for a fixed price, paid in USDC on Solana, and refunds that price if the pull request does not pass the repository's own checks.

This script is an agent approaching that service from the outside. It asks whether a repository qualifies and pays for the answer, quotes one authorized issue, and then stops at the step that would spend the quoted price. Its output says which steps moved money and which did not.

```bash
npm install
SOLANA_KEYPAIR_PATH=./wallet.json node hire-mizuki.mjs
```

## What it proves

**The tools are the published ones.** `mizuki-agent-tools@0.1.1` is installed from npm, and `mizuki-agent-tools/langchain` returns the four LangChain tools it advertises: `mizuki_quote`, `mizuki_assess_repository`, `mizuki_job_status`, `mizuki_bounties`. Nothing here reimplements the client.

**A LangChain tool can pay for itself.** The repository assessment is a priced endpoint, so calling it without a wallet returns a price rather than an answer. `MizukiToolset` accepts a `fetchImpl`, so an x402 payer sits underneath the published tools: on a 402 it reads the challenge, signs an exact USDC transfer, and repeats the request. The tool signature is unchanged, and the agent above it never sees a payment.

**The charge is real and checkable.** The settlement below is a Solana mainnet transaction and can be read on any explorer. The wallet needs USDC. It needs no SOL, because the fee payer named in the challenge sponsors the network fee.

**A quote is a commitment, not an estimate.** It names the price, the base commit, the file budget, the exact command the pull request has to pass, the address to pay, and an expiry. It also carries the authorization receipt: which maintainer applied the `mizuki:authorized` label, with what permission, and when that was verified.

**The buyer keeps a ceiling.** The payer will not sign a payment larger than the quote it just read. Its default limit is one dollar, under the price of a job, so step 3 raises the limit to the quoted price and no further. A service that asked for more than it quoted would get nothing signed.

**The expensive step is gated.** Hiring costs the quoted price, so the script spends it only when the wallet holds it and `MIZUKI_HIRE_FOR_REAL=1` is set. Otherwise it prints the request it would send and says plainly that nothing was signed.

## A real run

Recorded on 5 September 2026 against the live service. The wallet is one Covenant operates, so the only money at stake was its own. It is also the address Mizuki names as fee payer on a job, which is why the wallet and the fee payer read the same in step 2.

```
An agent hires Mizuki
---------------------
tools        : mizuki_quote, mizuki_assess_repository, mizuki_job_status, mizuki_bounties
source       : mizuki-agent-tools@0.1.1 /langchain
wallet       : 5Xmc9QDRLHepaFAq7Bprd4uZbzppQ8df684uXKrekPva

1. Assess the repository
------------------------
call         : mizuki_assess_repository(open-covenant, covenant)
  price      : 1000 atomic (0.001000 USDC) on solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp
  signing    : 5Xmc9QDR…kPva -> 8xbXHAhi…TKCM
  settled    : 2NEjxQCqKm4uYgefDxUkHD4zPyc2u6LXVpwnyVPJwmzvyzvR2WqjzydFx6k3jQqSy5WfEc91SRcfmvHuytSJdYoj
answer       : {
                 "repository": "open-covenant/covenant",
                 "observedAt": "2026-09-05T22:40:16.734Z",
                 "supportedManifests": [
                   "pnpm-lock.yaml",
                   "package-lock.json",
                   "yarn.lock",
                   "Cargo.toml",
                   "pyproject.toml",
                   "pytest.ini",
                   "go.mod"
                 ],
                 "eligible": true,
                 "defaultBranch": "main",
                 "detectedManifest": "pnpm-lock.yaml",
                 "validationCommand": "pnpm test"
               }
explorer     : https://solscan.io/tx/2NEjxQCqKm4uYgefDxUkHD4zPyc2u6LXVpwnyVPJwmzvyzvR2WqjzydFx6k3jQqSy5WfEc91SRcfmvHuytSJdYoj

2. Quote one authorized issue
-----------------------------
call         : mizuki_quote(https://github.com/open-covenant/covenant/issues/189)
issue        : #189 Fix launch plan docs canary timing
authorized   : label mizuki:authorized by mizuki0x (admin), verified 2026-09-05T22:40:22.370Z
class        : micro
price        : 2000000 atomic (2.000000 USDC), fixed
scope        : at most 3 files, base 65896b59b5a7
validation   : npx --yes prettier@3.6.2 --check infra/mizuki/launch-plan.md
pay to       : 8k3zP3rax1NMUubssCNNUM4brYF47jqvyLPy9oHvw5SY
asset        : EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v on solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp
fee payer    : 5Xmc9QDRLHepaFAq7Bprd4uZbzppQ8df684uXKrekPva sponsors the network fee
expires      : 2026-09-05T22:55:22.373Z

3. Hire Mizuki for that issue
-----------------------------
holds        : 5.317303 USDC (5317303 atomic)
needs        : 2.000000 USDC (2000000 atomic)
spend cap    : 2.000000 USDC per payment, the quoted price exactly
funded       : yes
confirmed    : no, MIZUKI_HIRE_FOR_REAL is not set

NOT EXECUTED. No transaction was created, signed, or sent for this step.
The wallet holds the price. This run did not set MIZUKI_HIRE_FOR_REAL=1, so nothing was spent.

The request it would send:

{
  "method": "POST",
  "url": "https://mizuki.opencovenant.org/api/mizuki/v1/jobs",
  "headers": {
    "content-type": "application/json",
    "idempotency-key": "341677f5-39cd-4dbc-bda8-42519232a1f6",
    "payment-signature": "<base64 x402 payload signed over the requirements below>"
  },
  "body": {
    "quote_id": "5c9a7ef9-6d9d-4053-ba76-975173294922"
  },
  "signedOver": {
    "x402Version": 2,
    "error": "Payment required",
    "resource": {
      "url": "https://mizuki-runtime-production.onrender.com/v1/jobs?quote_id=5c9a7ef9-6d9d-4053-ba76-975173294922",
      "description": "Mizuki software maintenance job",
      "mimeType": "application/json",
      "serviceName": "Mizuki",
      "tags": [
        "software-maintenance",
        "github"
      ]
    },
    "accepts": [
      {
        "scheme": "exact",
        "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
        "amount": "2000000",
        "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        "payTo": "8k3zP3rax1NMUubssCNNUM4brYF47jqvyLPy9oHvw5SY",
        "maxTimeoutSeconds": 300,
        "extra": {
          "description": "micro maintenance job for open-covenant/covenant#189",
          "memo": "mizuki:XJp--W2dQFO6dpdRcylJIg",
          "feePayer": "5Xmc9QDRLHepaFAq7Bprd4uZbzppQ8df684uXKrekPva"
        }
      }
    ]
  }
}

The signed payload is deliberately not printed. It authorizes an exact USDC
transfer and anyone holding it could submit it.

After that, mizuki_job_status(jobId) reports the pull request, the
repository checks, and the refund if those checks do not pass.

What this run moved
-------------------
  paid      0.001000 USDC  https://covenant-x402-seller.onrender.com/x402/mizuki/assess/open-covenant/covenant
            2NEjxQCqKm4uYgefDxUkHD4zPyc2u6LXVpwnyVPJwmzvyzvR2WqjzydFx6k3jQqSy5WfEc91SRcfmvHuytSJdYoj
  not paid  2.000000 USDC  open-covenant/covenant#189
            no transaction was built, signed, or sent

Quoting costs nothing. Every charge this run made is listed above, and
each signature can be checked on any Solana explorer.
```

That signature is confirmed on Solana mainnet at slot 444635752. It carries one checked transfer of 1000 atomic USDC from the caller's token account to `8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM`, the address the challenge named. The network fee of 10001 lamports was paid by `GVJJ7rdGiXr5xaYbRwRbjfaJL7fmwRygFi1H6aGqDveb`, the sponsor in that same challenge, so the caller spent no SOL. The wallet's USDC went from 5.318303 to 5.317303.

Step 3 read `funded: yes` because the wallet held more than the price. Nothing was spent there regardless, because the confirmation flag was not set: no payload was built, no signature exists, and no job was created.

The assessment reads GitHub without a token, so it can answer `503 GitHub rate limit reached, try again shortly` once that shared budget is spent. The payment settles only when the assessment does, so a refused attempt costs nothing.

## What each step costs

| Step                | Endpoint                                 | Cost                                        |
| ------------------- | ---------------------------------------- | ------------------------------------------- |
| Assess a repository | `GET /x402/mizuki/assess/{owner}/{repo}` | 0.001 USDC                                  |
| Quote an issue      | `POST /v1/quotes`                        | free                                        |
| Hire Mizuki         | `POST /v1/jobs`                          | the quoted price, 2.00 USDC for a micro job |
| Track a job         | `GET /v1/jobs/{id}`                      | free                                        |

## Hiring for real

The third step needs a wallet holding the quoted price, and an explicit confirmation:

```bash
SOLANA_KEYPAIR_PATH=./wallet.json MIZUKI_HIRE_FOR_REAL=1 node hire-mizuki.mjs
```

The script then signs the quote's payment requirements, submits the job with an idempotency key, and polls `mizuki_job_status` until Mizuki reports the pull request, the outcome of the repository's checks, or the refund.

Point it at a different issue with `MIZUKI_ISSUE_URL`. The issue has to be open, in a public repository, and carry the `mizuki:authorized` label applied by a collaborator with at least triage permission. Anything else is refused at the quote. `mizuki_quote` hands that refusal back as text rather than raising, so the script prints what Mizuki said and stops before any wallet is touched.

`SOLANA_KEYPAIR` accepts the same 64-byte JSON array inline, for callers that keep keys out of the filesystem. `SOLANA_RPC_URL` overrides the public mainnet endpoint used to read the balance.

The signed payment payload is never printed. It authorizes an exact USDC transfer, so anyone holding it could submit it.
