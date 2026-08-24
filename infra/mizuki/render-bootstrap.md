# Closed Render bootstrap

This Blueprint creates only the private policy signer, coding gateway, updater, their two isolated PostgreSQL databases, and the gateway's 1 GB disk. It does not create a public runtime, website, deployment controller, production database, shadow database, or image-backed service.

The updater intentionally omits deployment-controller settings. It can expose liveness and durable audit state, but readiness and proposal submission remain closed until the controller is separately reviewed and linked. Gateway readiness includes the non-billable provider-balance floor, while operator funding provenance and custody receipts remain separate gates. This bootstrap never authorizes paid intake.

## Before provisioning

1. Merge the reviewed release to protected `main`, record its full commit SHA, and confirm every required hosted check passed. The Blueprint must not source an unprotected branch.
2. Run `$HOME/bin/renderctl status` and `$HOME/bin/renderctl guard` from the repository root. Require `$HOME/bin/renderctl exec -- render workspace current -o json` to return the same workspace ID through the repository-isolated CLI config. Stop if any command fails or reports another workspace.
3. Validate `infra/mizuki/render-bootstrap.yaml`. The expected plan is exactly three private services and two PostgreSQL databases: five actions total.
4. Supply every `sync: false` value through Render's encrypted initial-Blueprint form. Never place a secret in this repository, a shell history entry, or deployment evidence.

## Create the Blueprint

Create a **new** Blueprint in the Render Dashboard with:

- repository: `https://github.com/open-covenant/covenant`;
- branch: `main`;
- Blueprint path: `infra/mizuki/render-bootstrap.yaml`;
- Auto Sync: **Off**.

Review the plan before applying it. It must contain only:

- `mizuki-policy-signer`;
- `mizuki-coding-gateway` and its 1 GB `/var/data` disk;
- `mizuki-updater`;
- `mizuki-signer-postgres`;
- `mizuki-updater-postgres`.

Never repurpose an existing Blueprint, and never manage these resources from a second Blueprint. Do not create, sync, or deploy anything if the release SHA, resource list, workspace, or secret inventory differs from the reviewed record.

After provisioning, leave automatic deployment disabled. Treat updater `/readyz`, signer readiness, and gateway `/readyz` as required fail-closed gates; do not connect the commercial runtime or open intake during bootstrap.

## Promote the same Blueprint

The bootstrap is a temporary path for the eventual production Blueprint, not a separate owner. After the immutable image, registry credential, controller, runtime databases, and every remaining secret have passed review, edit this Blueprint's path from `infra/mizuki/render-bootstrap.yaml` to `infra/mizuki/render.yaml` with Auto Sync still off. Validate the resulting plan before applying it. The plan must retain the existing signer, gateway, updater, and their databases by exact name; it may add only the reviewed controller, shadow runtime, production runtime, website, and isolated databases. Stop if Render proposes deleting or recreating a financial service or database. Never create a second Blueprint for the full file.
