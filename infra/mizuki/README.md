# Mizuki operations

This directory contains the production blueprint and runbooks for Mizuki's commercial maintenance service. The design keeps the transaction signer on Render's private network and gives the API no signing key.

## Deployment order

1. Review `env-contract.md` and prepare every required secret in a password manager.
2. Validate `render.yaml` from this directory with `render blueprints validate render.yaml` when the Render CLI supports an explicit file, or copy it to the repository root on the deployment branch and run `render blueprints validate`.
3. Apply the Blueprint from protected `main` only after the digest-pinned runtime image and its immutable release evidence exist.
4. Confirm the Blueprint-linked signer, gateway, updater, and controller tokens resolve into the production runtime. Do not copy those tokens into another service.
5. Confirm the signer, gateway, and updater resolve only on Render's private network. Never expose them as web services.
6. Deploy signer, gateway, updater, and controller first. Deploy shadow, prove the full functional probe, then deploy the sole production runtime and web app. Automatic deploys are disabled intentionally.
7. With intake closed, verify the website proxies only to `mizuki-runtime-production`, the production runtime alone uses `mizuki-postgres`, and the previous source-built API is suspended. Confirm startup reached the x402 facilitator and found the exact mainnet SVM route before enabling intake.
8. Register the signer's escrow authority as the ClawPump payout wallet through the operator-controlled signed wallet flow. Confirm the platform displays the exact address and retain the registration receipt.
9. Run the read-only checks below before any payment.

```sh
test -n "$MIZUKI_PRODUCTION_URL"
curl -fsS "$MIZUKI_PRODUCTION_URL/healthz"
curl -fsS "$MIZUKI_PRODUCTION_URL/readyz"
curl -fsS "$MIZUKI_PRODUCTION_URL/v1/admission"
curl -fsS https://mizuki.covenant.org/healthz
curl -fsS https://mizuki.covenant.org/
```

Check private-service health from a one-off shell on the production runtime's private network:

```sh
curl -fsS http://mizuki-policy-signer:8792/health
curl -fsS http://mizuki-coding-gateway:8642/healthz
curl -fsS http://mizuki-updater:8793/health
curl -fsS http://mizuki-deployment-controller:8794/healthz
```

Do not run a public canary until all health checks are stable, the mainnet wallets have only the documented reserve, and the operator can complete the signer recovery drill in `runbooks/incident-recovery.md`.

Fund only the isolated SOL capability authority through `runbooks/escrow-capacity.md`. Never create a ledger credit or mutate a bounty to represent incoming custody.

Token launch does not gate useful maintenance work and creator-fee reporting does not authorize spending. Before attributing any creator-fee distribution to capability funding, reconcile the ClawPump-reported `totalSent` delta with a finalized transfer to the configured escrow authority. Never count the reported total itself as on-chain custody evidence; every public rescue bounty must still show its own signer-created escrow transaction.

Fresh databases start with paid intake and new bounty claims closed. After the preflight succeeds, open only the path needed for the canary with an authenticated `POST /v1/admin/admission` request and record its reason. Close both switches before incident reconciliation or key rotation. The API returns `503` instead of authorizing payment or binding a claimant when the control row cannot be read.

## Deliberate constraints

- `autoDeploy` is off for every service. Promotion must follow tests, shadow health, and an explicit operator decision.
- The signer has a $25 per-operation ceiling and a $100 rolling 24-hour ceiling. Raising either is a policy change, not an incident workaround.
- The signer requires two distinct finalized-history RPC providers and two independently operated SOL/USD feeds. Price observations more than 500 basis points apart fail closed before escrow reservation.
- Production runtime, shadow, signer, updater, and controller use separate PostgreSQL resources and credentials, except that the production runtime deliberately retains the canonical commercial `mizuki-postgres`. A commercial database compromise must not permit mutation of signer operations, updater approvals, or deployment state. None can be replaced by an in-memory store in production.
- The production runtime receives only the signer's private URL and bearer token. It never receives the signing key.
- Every service uses a paid, non-sleeping plan and every database uses a persistent paid plan. Do not downgrade any of them during the event.
- Public intake stays limited to public repositories and Micro or Standard work until the traction gates are passed.
- Mainnet intake remains closed until the immutable escrow program ID and executable hash are pinned, independently reviewed, and exercised on devnet.

## Files

- `render.yaml`: Render Blueprint.
- `env-contract.md`: secrets, ownership, rotation, and equality constraints.
- `runbooks/escrow-capacity.md`: controlled SOL capacity funding and reconciliation.
- `runbooks/canary-success.md`: public $2 issue-to-merge proof.
- `runbooks/canary-refund-bounty.md`: real refund-to-bounty proof.
- `runbooks/incident-recovery.md`: signer, refund, and escrow incidents.
- `runbooks/alerts.md`: page thresholds and commercial-core stop actions.
- `launch-plan.md`: outreach, stream cadence, and hard launch gates through 19 September 2026.
