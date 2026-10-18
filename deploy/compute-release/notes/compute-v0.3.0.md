Covenant Compute 0.3.0 adds encrypted workspace checkpoints to the buyer CLI.
Save a checkpoint file independently of a GPU lease, then restore it with the
same buyer identity. The buyer encrypts and decrypts locally; storage receives
ciphertext, not the workspace key.

## New workspace commands

- `workspace create`, `list` and `status` manage workspace metadata and the
  client's pinned version.
- `workspace save <id> <file>` streams an encrypted checkpoint and commits its
  verified ciphertext digest. Local pending state survives process restart.
- `workspace retry <id>` reconciles the original upload and ciphertext after an
  uncertain outcome. It does not create a replacement upload to bypass it.
- `workspace restore <id> <file> [--lease UUID]` checks the pinned snapshot and
  restores a new local file. An existing destination is never overwritten.
- `workspace backup` and `workspace import` move a private key/version backup
  between clients using the same buyer identity and coordinator endpoint.
- `workspace delete` tombstones the workspace and schedules object cleanup
  after outstanding grants expire.

Workspace operations require a coordinator whose private object storage has
passed provider acceptance. Installing this CLI does not enable that service.
Saving a checkpoint creates no rental or transfer from the buyer's Compute
balance. Provider storage and request costs are separate from Compute payments.

A snapshot is a file, such as an archived checkpoint directory; this release
does not synchronize remote directories. `--lease` checks lease ownership and
trust requirements but does not transfer plaintext to a machine. Use an
independently authenticated connection for that transfer. Stronger hardware
trust and recovery of actual training progress require their own acceptance.

The format supports up to 4 GiB of plaintext in authenticated chunks; the
coordinator's configured storage ceiling may be lower and includes ciphertext
overhead, pending uploads and cleanup obligations. Back up the workspace key
and buyer identity privately. Neither the coordinator nor the storage provider
can recover a lost encryption key.

## Included commands and platforms

Each archive contains `covenant-compute`, `covenant-compute-mcp`,
`covenant-compute-node` and `covenant-compute-stake`, all reporting version
`0.3.0`, plus the license, notice, command guide and these release notes.
The workspace interface is in `covenant-compute`; the MCP binary's tools are not
expanded by this release.

Archives are available for Apple silicon and Intel macOS, and x86-64 and ARM64
Linux. Linux binaries use static musl linking. Download the matching archive,
verify it, unpack it and install the commands from that directory:

```sh
# macOS browser downloads only, after verifying the archive:
xattr -d com.apple.quarantine covenant-compute*
mkdir -p ~/.local/bin
install -m 0755 covenant-compute* ~/.local/bin/
covenant-compute --version
covenant-compute workspace --help
```

On Linux, omit the `xattr` line. Homebrew and the website install script use
separately pinned releases; check that they report
`0.3.0` before using them for this release's acceptance.

## Verify a download

Each archive and `checksums.txt` has a cosign signature (`.sig`) and certificate
(`.pem`) from the release workflow. For the Apple silicon archive:

```sh
cosign verify-blob \
  --signature covenant-compute-darwin-arm64.tar.gz.sig \
  --certificate covenant-compute-darwin-arm64.tar.gz.pem \
  --certificate-identity https://github.com/open-covenant/covenant/.github/workflows/compute-release.yml@refs/tags/compute-v0.3.0 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  covenant-compute-darwin-arm64.tar.gz

grep '  covenant-compute-darwin-arm64.tar.gz$' checksums.txt | shasum -a 256 -c
gh attestation verify covenant-compute-darwin-arm64.tar.gz --repo open-covenant/covenant
```

Use the corresponding filename for another platform. Signature and build
provenance verification establish which release workflow produced the archive;
installed-client and production workspace acceptance remain separate checks.

The [workspace guide](https://github.com/open-covenant/covenant/blob/compute-v0.3.0/docs/compute-workspaces.md)
covers the full workflow, private backups, cleanup and storage limits.
