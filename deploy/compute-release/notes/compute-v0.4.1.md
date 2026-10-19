Covenant Compute 0.4.1 adds Cloudflare's security-audit workflow to the buyer CLI
and MCP server. Agents can load its instructions progressively or install the
complete package offline, including its report schema and Node.js validators.

## Use the security audit workflow

Inspect the bundle and export it to an agent skill directory:

```sh
covenant-compute audit-skill --list
covenant-compute audit-skill --read SKILL.md
mkdir -p .agents/skills
covenant-compute audit-skill --export .agents/skills/covenant-compute-security-audit
```

The export destination must not already exist. The command refuses to replace an
installed copy. The package contains a Covenant entrypoint, `UPSTREAM.json`, and
unchanged Cloudflare workflow documents, schema, validators, tests, and license
under `upstream/`. Cloudflare's source is pinned at
`c1c8a8c1471069fb0e188eeaff69b8e8db6564a8`; the manifest records each file's SHA-256.

MCP clients can call `compute.audit` with `{}` to read the entrypoint, then pass a
`document` such as `upstream/RECONNAISSANCE.md` to load another file. Restart an
existing MCP server after upgrading its executable so clients discover the tool.

The calling agent coordinates the six-phase audit, fresh independent reviewers,
coverage ledger, evidence retention, and required execution sandbox. Loading or
exporting the bundle makes no purchase, sends no repository to an operator, and
does not execute the audit. This release does not add a paid security-audit job
kind or implement the sandbox. Validator success checks record structure and
semantics; it does not establish that a vulnerability was reproduced.

The [audit guide](https://github.com/open-covenant/covenant/blob/compute-v0.4.1/docs/compute-security-audit.md)
describes the complete workflow, output artifacts, execution requirements, and
separate authorization boundaries for remote assistance and remediation.

## Included commands and verification

The archive contains `covenant-compute`, `covenant-compute-mcp`,
`covenant-compute-node`, and `covenant-compute-stake`, all versioned `0.4.1`, plus
the Apache license, notice, command guide, these notes, and Cloudflare's MIT
`SECURITY_AUDIT_LICENSE`. The workflow builds Apple silicon and Intel macOS, and
x86-64 and ARM64 Linux. Linux executables use static musl linking.

Each platform build checks the extracted executables and bundled audit files
before the archive is published. Verify a downloaded archive's checksum,
signature, and workflow provenance. For Linux x86-64:

```sh
cosign verify-blob \
  --signature covenant-compute-linux-x86_64.tar.gz.sig \
  --certificate covenant-compute-linux-x86_64.tar.gz.pem \
  --certificate-identity https://github.com/open-covenant/covenant/.github/workflows/compute-release.yml@refs/tags/compute-v0.4.1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  covenant-compute-linux-x86_64.tar.gz
grep '  covenant-compute-linux-x86_64.tar.gz$' checksums.txt | sha256sum -c
gh attestation verify covenant-compute-linux-x86_64.tar.gz --repo open-covenant/covenant
```

Use the matching filename for another platform and `shasum -a 256 -c` on macOS.
Signature and provenance checks identify the published build; they do not prove
security-audit correctness. Homebrew and the website installer pin releases
separately, so check `covenant-compute --version` after upgrading.
