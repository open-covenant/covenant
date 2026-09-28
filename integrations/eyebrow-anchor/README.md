# eyebrow anchor

Publish a tamper-evident, timestamped commitment to a machine's AI agent tooling.

[eyebrow](https://github.com/alexverify/eyebrow) inventories the skills, MCP servers, plugins and
hooks installed for the coding agents on a machine, hashes every file behind each one, and writes
`eyebrowlock.json`. That lockfile answers "what is actually installed here" precisely enough to
catch a swapped skill or a quietly edited MCP config. It answers it only as well as the disk it
sits on: whoever can change the artifacts can change the record of them.

This writes the lockfile's digest through the Covenant settlement program's `anchor_receipt_batch`.
After that the scan has a block time, a signer, and a merkle root that anyone can read. Revising
history means producing a second anchor with a different root, in public, later.

The inventory itself never leaves the machine. Only two 32-byte hashes and a count go on chain.

## Status

The settlement program `cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y` is deployed and callable on
Solana devnet. On mainnet-beta the program was closed on 2026-09-21 and its replacement is not yet
deployed, so `anchor_receipt_batch` currently has nowhere to land there. `anchor-scan.mjs` checks
for this before it builds a transaction and refuses rather than burning a fee.

Reading is unaffected. The `ReceiptBatch` accounts anchored before the close are still on
mainnet-beta and `verify-anchor.mjs --cluster mainnet` decodes them today. When the program is
redeployed, point `PROGRAM_ID` in `chain.mjs` at the new id and the write path works unchanged.

## Use

Requires Node 20+ and the `eyebrow` binary (`brew install alexverify/tap/eyebrow`).

```sh
npm install

# derive everything, send nothing, spend nothing
node anchor-scan.mjs --dry-run --global

# scan and anchor
node anchor-scan.mjs --cluster devnet --global --keypair ~/.config/solana/id.json

# check a lockfile against what is on chain
node verify-anchor.mjs --cluster devnet --lockfile eyebrowlock.json --batch-id <hex>

# and prove one artifact was in that scan
node verify-anchor.mjs --cluster devnet --lockfile eyebrowlock.json --batch-id <hex> \
  --prove 083e1e251b90edcd
```

`--lockfile` on `anchor-scan.mjs` anchors a lockfile you already have instead of running a fresh
scan. `--global` includes the user-home scope, which is where most agent tooling lives. Keep the
lockfile: the verifier needs the exact bytes that were anchored.

`anchor_receipt_batch` accepts only the settlement `Config` authority as signer, so the anchoring
key is whichever key holds that role on the cluster you target. The account the program opens is
117 bytes, so an anchor costs about 0.00125 SOL of rent plus the signature fee.

`verify-anchor.mjs` exits 0 when every check passes and 1 when any of them does not, so it drops
into CI as a gate.

## What gets anchored

Three values, derived from the lockfile alone.

| Instruction argument | Value |
| --- | --- |
| `batch_id` | identifies this scan, and seeds the PDA that holds the record |
| `merkle_root` | RFC 6962 root over the artifact digests, which supports inclusion proofs |
| `receipt_count` | the number of artifacts in the scan |

The record lands at the PDA `["receipt_batch", batch_id]` under the settlement program and holds
the root, the count, the signer, and the block timestamp.

## Recomputing it yourself

An anchor nobody else can reproduce proves nothing, so every value below is SHA-256 over bytes the
lockfile already contains. A third party needs the lockfile, the batch id, and a SHA-256
implementation. Nothing in this directory is required.

**1. Artifact digest.** For each entry in `artifacts`:

- If the entry has a `contentHash` of the form `sha256-<64 lowercase hex>`, the digest is those 32
  bytes. This is eyebrow's own fold over every file it hashed for that artifact.
- Otherwise (remote MCP servers, which eyebrow marks `REMOTE-UNHASHABLE` and pins by TLS SPKI), the
  digest is SHA-256 over the UTF-8 string

  ```
  "eyebrow-artifact:v1\n" + id + "\n" + tool + "\n" + scope + "\n" + type + "\n" + name + "\n"
    + source.kind + "\n" + source.ref + "\n" + source.certSpki + "\n"
  ```

  with a missing field written as the empty string. A `contentHash` in any other format is an
  error, never a fallback.

**2. Canonical order.** Sort the artifacts ascending by the UTF-8 bytes of `id`. Duplicate ids are
an error. eyebrow already emits them in this order; sorting anyway keeps the anchor a function of
the lockfile's contents rather than of the scanner's output order.

**3. Merkle root.** RFC 6962 section 2.1, over the sorted 32-byte digests as entries:

```
MTH({})     = SHA-256()
MTH({d0})   = SHA-256(0x00 || d0)
MTH(D[n])   = SHA-256(0x01 || MTH(D[0:k]) || MTH(D[k:n]))   k = largest power of two < n
```

Audit paths and their verification follow RFC 6962 section 2.1.1 unchanged, which is what
`--prove` prints and checks.

**4. Content hash.** eyebrow 0.5.3 writes a `contentHash` per artifact and none for the lockfile as
a whole, so this defines the scan-level one: SHA-256 over the UTF-8 string

```
"eyebrow-scan-content:v1\n" + generator + "\n" + generatedAt + "\n" + count + "\n"
```

followed by one line `id + " " + hex(digest) + "\n"` per artifact in the canonical order, where
`count` is the decimal artifact count and `hex` is lowercase.

**5. Batch id.** SHA-256 over the UTF-8 string `"eyebrow-scan:v1:" + hex(contentHash)`, lowercase
hex.

Including `generatedAt` means two scans of identical tooling taken at different times get different
batch ids and the same root, so a machine can anchor its state again tomorrow and the two records
sit side by side.

### Reference implementation

Roughly thirty lines of Python standard library, sharing no code with this directory:

```python
import hashlib, json, re

lock = json.load(open("eyebrowlock.json"))

def digest(a):
    ch = a.get("contentHash")
    if ch:
        m = re.fullmatch(r"sha256-([0-9a-f]{64})", ch)
        if not m: raise ValueError(ch)
        return bytes.fromhex(m.group(1))
    s = a.get("source", {})
    pre = "eyebrow-artifact:v1\n" + "\n".join([
        a["id"], a.get("tool",""), a.get("scope",""), a.get("type",""), a.get("name",""),
        s.get("kind",""), s.get("ref",""), s.get("certSpki",""),
    ]) + "\n"
    return hashlib.sha256(pre.encode()).digest()

leaves = sorted(((a["id"], digest(a)) for a in lock["artifacts"]), key=lambda x: x[0].encode())

def mth(ds):
    if not ds: return hashlib.sha256(b"").digest()
    if len(ds) == 1: return hashlib.sha256(b"\x00" + ds[0]).digest()
    k = 1 << ((len(ds) - 1).bit_length() - 1)
    return hashlib.sha256(b"\x01" + mth(ds[:k]) + mth(ds[k:])).digest()

root = mth([d for _, d in leaves])
pre = ("eyebrow-scan-content:v1\n" + lock["generator"] + "\n" + lock["generatedAt"] + "\n"
       + str(len(leaves)) + "\n" + "".join(f"{i} {d.hex()}\n" for i, d in leaves))
content = hashlib.sha256(pre.encode()).digest()
batch = hashlib.sha256(("eyebrow-scan:v1:" + content.hex()).encode()).digest()

print("merkle root", root.hex())
print("batch id   ", batch.hex())
```

Reading the anchor back needs no SDK either. The `ReceiptBatch` account at
`["receipt_batch", batch_id]` is 117 bytes: an 8-byte Anchor discriminator
(`SHA-256("account:ReceiptBatch")[..8]`), then `batch_id` (32), `authority` (32), `merkle_root`
(32), `receipt_count` (u32 little-endian), `created_at` (i64 little-endian), `bump` (u8).

## Worked example

A scan of this machine's agent tooling, 27 artifacts, anchored on devnet on 2026-09-23.

```
content hash   44d3cc38dae0292de3368b0a4457185a40d4f5a74bb79bfc524b489a2660ccee
batch id       bd00d24586892bdf37069924fb6b349022538c7449debd167b451464a0d5e39d
merkle root    e8e4628a4a87c7b5d1cc8b881bead98345362b78f20f84a56a3ff658615a80a5
receipt count  27
batch account  57mJEneF7D31PZuW7ybXxeLqd3yip24fgeTx3B5HzfcS
signature      2ebUArgrpKZG2vzBYDcnYzDeZnWtJfYtmfAi2kJ7gAEZQr6rYYChBr4VUfDyzK4kVmDMAJGqfsL1WmUfQnCC1Xiq
```

<https://explorer.solana.com/tx/2ebUArgrpKZG2vzBYDcnYzDeZnWtJfYtmfAi2kJ7gAEZQr6rYYChBr4VUfDyzK4kVmDMAJGqfsL1WmUfQnCC1Xiq?cluster=devnet>

Verifying that lockfile against that batch id passes five of five checks and prints a five-hash
audit path for any artifact you name. Changing one artifact's hash in the lockfile and verifying
again fails: the lockfile no longer derives the anchored batch id, the recomputed root no longer
matches the anchored one, and the audit path no longer reaches it.

## Scope and limits

The anchor commits to what eyebrow found. It says nothing about whether those artifacts are
trustworthy, and it cannot show that eyebrow saw everything on the machine. Whoever holds the
`Config` authority key decides what gets anchored; the guarantee is that once a root is on chain it
cannot be revised silently.

`eyebrowlock.json` records absolute filesystem paths, so it is gitignored here. Distribute the
lockfile deliberately, or distribute only the digests and an audit path.

## Tests

```sh
npm test
```

Covers the derivation against the published RFC 6962 vectors for zero through eight entries,
inclusion proofs at every index, ordering independence, and the tamper cases. No network, no keys.

## Files

| | |
| --- | --- |
| `derive.mjs` | lockfile to batch id, merkle root, count, and audit paths |
| `chain.mjs` | program id, PDAs, the instruction, and the account decoder |
| `anchor-scan.mjs` | scan, derive, report, send |
| `verify-anchor.mjs` | recompute, read the chain, compare, exit non-zero on any mismatch |
| `derive.test.mjs` | `npm test` |

Client-side only. The settlement program is unchanged.
