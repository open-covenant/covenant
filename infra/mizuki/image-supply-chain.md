# Mizuki application image supply chain

The application promotion identity is a single-platform GHCR reference of the form:

```text
ghcr.io/open-covenant/covenant/mizuki@sha256:<64 lowercase hex characters>
```

Tags are not accepted as promotion identity. A source commit alone is also insufficient: the promoted value must include the registry digest produced and attested by `mizuki-image.yml`.

## Publication gate

The workflow publishes only on protected `main`, or when a manually supplied full commit SHA is already reachable from `origin/main`. It is not triggered by pull requests and the jobs reject repositories other than `open-covenant/covenant`, so a fork cannot obtain `packages:write` or OIDC publication authority.

Before the image build, the workflow checks out that exact commit, runs the core Postgres integration suite, typecheck, and build, then builds only `linux/amd64`. The Node base image, pnpm version, Buildx version, BuildKit image, JavaScript actions, package versions, and lockfile are pinned. `SOURCE_DATE_EPOCH` is the source commit timestamp. No build secret is passed to Docker.

The registry exporter pushes by digest without creating a tag. BuildKit-native SBOM and provenance output is disabled because adding in-image attestations changes a nominally single-platform result into an OCI index. Instead, GitHub signs build provenance and an SPDX JSON SBOM as referrer attestations after the image digest is fixed.

## Evidence contract

Each source commit has one release tag, `mizuki-image-<full-commit-sha>`. The build job stages
its evidence as a short-lived Actions artifact, but promotion consumes the permanent GitHub
Release assets. The contents-capable job first creates a draft, replaces any assets left by a
failed draft attempt, and only then publishes it. Publication fails unless GitHub reports the
release as non-draft and immutable, the tag resolves through the Git data API to the exact source
commit, and every release asset's REST `size` and `sha256:` digest match the downloaded bytes.

The immutable release contains exactly:

- `manifest.oci.json`: exact bytes downloaded from GHCR for the published digest;
- `image-config.oci.json`: the content-addressed config proving `linux/amd64`;
- `promotion-input.json`: commit, digest-qualified reference, platform, manifest digest and byte size, material hashes, and workflow run identity;
- `sbom.spdx.json`;
- GitHub OIDC attestation bundles for the image provenance, image SBOM, and promotion input;
- `build-metadata.json` and `SHA256SUMS`.

Publication fails unless the downloaded object is an OCI image manifest rather than an index, its raw SHA-256 equals the builder digest, its registry response reports the same digest, its size is bounded, every descriptor is content-addressed, and its config is exactly `linux/amd64`.

An idempotent rerun does not rebuild or edit an existing immutable release. It downloads all nine
canonical assets, verifies their REST size and digest, verifies the complete `SHA256SUMS` file,
rechecks the commit/tag/image/manifest binding, and verifies the downloaded provenance, SBOM, and
promotion-input bundles against the protected workflow identity and its recorded signer commit. It
then emits the same promotion URL. A mutable draft may be repaired; a published non-immutable
release or an immutable release that fails any check is a hard failure.

## Operator verification

Download the evidence from the commit release and verify it before copying the reference into an
upgrade manifest:

```bash
gh release download mizuki-image-<full-commit-sha> \
  --repo open-covenant/covenant \
  --dir mizuki-image-evidence
(
  cd mizuki-image-evidence
  sha256sum --check --strict SHA256SUMS
)
jq -er '.image.reference' mizuki-image-evidence/promotion-input.json
gh attestation verify \
  "oci://$(jq -er '.image.reference' mizuki-image-evidence/promotion-input.json)" \
  --repo open-covenant/covenant
```

The updater and deployment controller must compare the complete `name@sha256:digest` value with the signed promotion input. They must not resolve a tag at admission time, accept a tag plus an out-of-band digest, or silently substitute a multi-platform index.

The manifest URL in `promotion-input.json` is always:

```text
https://github.com/open-covenant/covenant/releases/download/mizuki-image-<full-commit-sha>/manifest.oci.json
```

Runtime downloaders accept that credential-free URL and exactly one manual redirect. GitHub
currently redirects public release assets to
`https://release-assets.githubusercontent.com/github-production-release-asset/1219904470/<uuid>`.
The redirect verifier requires HTTPS, the exact repository ID and UUID path shape, GitHub's signed
read-only blob query, and both signed content-disposition fields naming `manifest.oci.json`. It
rejects direct CDN input, another repository or asset, credentials, input queries or fragments,
relative redirects, and a second redirect. Download requests carry only `Accept`; authorization is
never forwarded to the CDN. The configured artifact origins must therefore be exactly
`https://github.com` and `https://release-assets.githubusercontent.com`.

## GitHub Apps

The JSON files in `github-apps/` are least-privilege registration inputs, not credentials. Generate separate private keys, installation IDs, OAuth secret, and webhook secret after registration; never commit them. Install the core App only on public repositories that explicitly opt in. Install the updater only on the protected application repository.

The policy verifier manifest grants read-only repository access and no events. The signer authenticates the App, discovers the installation for the exact target repository, and directly mints a short-lived repository-scoped token with the manifest's exact four read permissions. Do not give the signer write permission or a core/updater private key.

The core manifest uses stable same-origin callback and webhook endpoints under `mizuki.covenant.org`. Register it only after that hostname, the web proxy's signed-webhook path, and the sole production runtime have been verified end to end.
