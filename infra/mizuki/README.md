# Mizuki operations

This directory contains the production blueprint and runbooks for Mizuki's commercial maintenance service. The design keeps the transaction signer on Render's private network and gives the API no signing key.

## Deployment order

1. Review `env-contract.md` and prepare every required secret in a password manager.
2. Run the repository-bound `$HOME/bin/renderctl status` and `$HOME/bin/renderctl guard`, then require `$HOME/bin/renderctl exec -- render workspace current -o json` to return the same workspace ID before validating the selected Blueprint. The wrapper must use a repository-isolated Render CLI config; never inherit or repair a global CLI profile during a deployment.
3. Use `render-bootstrap.yaml` only for the closed private-service bootstrap documented in `render-bootstrap.md`. Apply either Blueprint from protected `main` only.
4. Promote the existing bootstrap Blueprint by changing that same Blueprint's path to `render.yaml`; never create a second Blueprint that owns the same signer, gateway, updater, or databases.
5. Confirm the Blueprint-linked signer, gateway, updater, and controller tokens resolve into the production runtime. Do not copy those tokens into another service.
6. Confirm the signer, gateway, and updater resolve only on Render's private network. Never expose them as web services.
7. Deploy signer, gateway, updater, and controller first. Close both durable admission controls before every runtime deploy; the runtime predeploy command rejects any open state. Deploy the zero-authority shadow and prove its tokenless closed-state probe, then deploy the sole production runtime and prove the authenticated application dependency probe before cutting over the web app. The deployment probe excludes only updater health to keep the controller path acyclic; operator `/readyz` and paid admission still require updater health. Automatic deploys are disabled intentionally.
8. With intake closed, verify the website proxies only to `mizuki-runtime-production`, the production runtime alone uses `mizuki-postgres`, and the previous source-built API is suspended. Confirm startup reached the x402 facilitator and found the exact mainnet SVM route before enabling intake.
9. Register the signer's escrow authority as the ClawPump payout wallet through the operator-controlled signed wallet flow. Confirm the platform displays the exact address and retain the registration receipt.
10. Run the read-only checks below before any payment.

### Payment-expiry reader-first rollout

`payment_expired` is a new terminal job state. Deploy it in two phases so an overlapping older runtime cannot bind a terminal attempt back to a job:

1. Close both durable admission controls and leave `MIZUKI_PAYMENT_EXPIRY_WRITES_ENABLED=0`.
2. Deploy the signer migration, then shadow, then production runtime. Do not invoke settlement recovery manually during this phase.
3. Wait for Render to report the previous production instance stopped. Confirm only the new immutable image is serving, then read `/v1/admin/admission` and verify both controls remain closed.
4. Exercise read-only recovery against a fixture containing `payment_expired`; the attempt endpoint must remain `expired_unpaid`, payment status must remain `unpaid`, and account obligations must remain zero.
5. Change `MIZUKI_PAYMENT_EXPIRY_WRITES_ENABLED` to `1` and deploy production again. Every overlapping instance now contains the terminal-state reader guards.
6. Run settlement recovery for one signer-confirmed `expired_unpaid` fixture, repeat the read-only checks, and only then run the paid/refund canaries.
7. Reopen intake only after the invariant scan is clean. A rollback to an image without the reader guards must keep intake closed and the expiry write gate disabled.

```sh
test -n "$MIZUKI_PRODUCTION_URL"
curl -fsS "$MIZUKI_PRODUCTION_URL/healthz"
curl -fsS "$MIZUKI_PRODUCTION_URL/readyz"
curl -fsS "$MIZUKI_PRODUCTION_URL/v1/admission"
curl -fsS https://mizuki.opencovenant.org/healthz
curl -fsS https://mizuki.opencovenant.org/
```

The audit migration is boot-compatible with the previously deployed core: it leaves `commercial-core` at v1 and records the ledger under a separate migration component. A rollback to the old runtime must remain read-only and closed-state only. Do not call its admission mutation endpoint; it cannot append the new ledger. Restore the current runtime before any admission mutation. Any current-row/audit mismatch fails the current runtime closed and requires database reconciliation before service restoration.

Check signer, gateway, and updater health from a one-off shell on the production runtime's private network:

```sh
curl -fsS http://mizuki-policy-signer:8792/health
curl -fsS http://mizuki-coding-gateway:8642/healthz
curl -fsS http://mizuki-updater:8793/health
```

The controller authenticates every route. Check it from an updater shell, which receives only the
linked controller credential:

```sh
curl -fsS -H "Authorization: Bearer $MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN" \
  http://mizuki-deployment-controller:8794/healthz
```

Do not run a public canary until all health checks are stable, the mainnet wallets have only the documented reserve, and the operator can complete the signer recovery drill in `runbooks/incident-recovery.md`.

Fund only the isolated SOL capability authority through `runbooks/escrow-capacity.md`. Never create a ledger credit or mutate a bounty to represent incoming custody.

Token launch does not gate useful maintenance work and creator-fee reporting does not authorize spending. Before attributing any creator-fee distribution to capability funding, reconcile the ClawPump-reported `totalSent` delta with a finalized transfer to the configured escrow authority. Never count the reported total itself as on-chain custody evidence; every public rescue bounty must still show its own signer-created escrow transaction.

Fresh databases start with paid intake and new bounty claims closed. Before every authenticated `POST /v1/admin/admission`, read the non-cacheable `GET /v1/admin/admission` and bind the mutation to its `expectedRevision`. A stale request that could enable either path returns `409`; a closure remains fail-safe and wins over an in-flight stale open. Payment admission, new-claim binding, settlement recovery, and control mutation share one PostgreSQL advisory lock across overlapping runtime processes, so a successful close response is global. After the preflight succeeds, open only the path needed for the canary and record its reason. Close both switches before every deploy, incident reconciliation, or key rotation, then retain the returned row from `GET /v1/admin/admission/audit`. The API returns `503` instead of authorizing payment or binding a claimant when the control row cannot be read.

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

- `render-bootstrap.yaml`: closed signer, gateway, and updater bootstrap.
- `render-bootstrap.md`: guarded bootstrap and same-Blueprint promotion procedure.
- `render.yaml`: full production Blueprint.
- `env-contract.md`: secrets, ownership, rotation, and equality constraints.
- `runbooks/escrow-capacity.md`: controlled SOL capacity funding and reconciliation.
- `runbooks/canary-success.md`: public $2 issue-to-merge proof.
- `runbooks/canary-refund-bounty.md`: real refund-to-bounty proof.
- `runbooks/incident-recovery.md`: signer, refund, and escrow incidents.
- `runbooks/alerts.md`: page thresholds and commercial-core stop actions.
- `launch-plan.md`: outreach, stream cadence, and hard launch gates through 19 September 2026.
