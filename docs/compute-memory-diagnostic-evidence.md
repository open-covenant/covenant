# Measured memory diagnostic — 2026-10-20

The existing Phala diagnostic CVM passed a genuine TDX verification and a
synthetic SSH memory/swap check after its guest image was updated. The live
SSH role reported the required non-swappable tmpfs mounts, no active guest
swap, zero cgroup swap allowance/usage and disabled core dumps. This was an
**Open synthetic diagnostic**, not a paid Covenant lease, customer workload,
new CVM or production release. KMS production qualification remains rejected.

## Immutable scope

| Identity | Value |
| --- | --- |
| Guest image source | [`232bf28755387a4cbf803a53b02fc0880e0eb7c7`](https://github.com/open-covenant/covenant/commit/232bf28755387a4cbf803a53b02fc0880e0eb7c7) |
| Published image digest | `sha256:42893bf85bc07eb1bd3e9e5a48fe3ab3cd36b931d548080314691f7c7134cc7a` |
| Exact app-compose SHA-256 | `16e98135fb1055afe2acf775356c3887199c06dc70270e26cd3037f25e831fd3` |
| Existing CVM | `32ed28e4-0a6f-4dff-a9bf-ba84c38d6e9f` |
| Application | `d5f0ef708c68c52916a7b0da313ae416eb0b980d` |
| TDX instance | `fc551bcb5f96283763f42da3578805ec7e2ecfe3` |
| New diagnostic job | `a5a502b3-35d8-4e31-9d8b-b81a6e85b543` |
| Generation | `phala-a5a502b3-35d8-4e31-9d8b-b81a6e85b543-1` |
| Challenge | `3eba9d2f-38f7-4720-b602-e1e2cada9657` |
| SSH fingerprint | `SHA256:VKsJnrIDVmW0KOpk9BZCl3QfsXUilf+8cvSBZDV6P8E` |
| Diagnostic service expiry | `2026-10-20T19:47:00.419Z` |

Both containers used the pinned digest in
`ghcr.io/open-covenant/covenant/compute-node-public`. Anonymous registry reads
verified the original manifest/config bytes, `linux/amd64`, exact source and
`attested-cpu-diagnostic` labels, and an unchanged source tag before/after.
The config digest was
`sha256:50d123614118a82b5c168856a276b3f2765385c3a12a40ff109ccc06d034f0e8`.
The prepared and provider-normalized workload hashes matched, including the
explicit app-compose string `"swap_size": "0"`. The diagnostic window was
45 minutes; the new expiry has **not** been live-verified by this record.

## Live observations

| Event | UTC |
| --- | --- |
| Anonymous image verification completed | `19:01:42.628Z` |
| Provider Compose update started | `19:02:12.417Z` |
| Provider Compose update completed | `19:03:10.080Z` |
| Challenge issued | `19:05:14.231Z` |
| Genuine quote response completed | `19:05:18.303Z` |
| Current-clock hardware verification | `19:05:19.881Z` |
| SSH runtime check and synthetic cleanup completed | `19:05:27.609Z` |

Intel signature and current collateral verification passed with platform and
QE status `UpToDate`, no advisories, and the complete independent production
launch reference. The reference SHA-256 was
`05f87257b1bf37a77f1b00d62e144f18dbc23bd6e3be72dbb3fdda6b0b49a841`;
the [measurement recipe](../deploy/compute-attested-cpu/measurement/README.md)
derives the expected dstack 0.6.1 launch tuple from official source/artifacts.
The exact workload, challenge, lease scope and SSH key matched. The result
was evidence class `Attested`, served class `Open`, without a clock override
or GPU identity. SSH used the verified key and a TLS-validated gateway.

The SSH session ran as uid `10001` on kernel `6.18.40-dstack`. Its retained
raw kernel observations showed:

- `/workspace`: 128-MiB tmpfs with `noswap`; `/run`: 8 MiB; `/tmp`: 16 MiB;
  `/host-public`: 1 MiB. All four mounts had `noswap`, `nosuid` and `nodev`;
  the latter three also had `noexec`.
- `/proc/swaps` contained only its header. Private cgroup v2 reported
  `memory.max=805306368` (768 MiB), `memory.swap.max=0` and
  `memory.swap.current=0`.
- Core soft/hard limits were both zero, and the kernel core pattern was
  `core`, not a pipe handler. The runtime guard passed, with fresh SSH
  watchdog status bound to this policy and host key.

The guest generated, wrote, read back, hashed and removed **1,048,576 synthetic
bytes** inside `/workspace`; removal was confirmed. This was an on-guest
write through the verified SSH session, not a 1-MiB client upload.

The raw runtime snapshot covers the **SSH role only**. Successful quote
collection exercised the measured collector's own runtime guard and its
lease/key-bound SSH-status checks before and after guest RPCs. It is not a
separately captured raw collector cgroup/mount snapshot.

The authenticated provider operation record now reports this exact Compose
update as `completed`, targeting `running`, with the new workload hash.
That is a different operation from the earlier failed update retained in the
[original diagnostic record](compute-hardware-diagnostic-evidence.md); the
historical failure has not been rewritten as successful.

## Separate CI evidence

[Image job 118823967232](https://github.com/open-covenant/covenant/actions/runs/39479474760/job/118823967232)
passed at the exact source above: 47 Python tests and the real Docker
SSH/SFTP runtime smoke. The smoke included a 64-MiB SSH transfer, enforced
workspace capacity, restart erasure/key rotation and watchdog/refusal cases.
It used a mock guest quote socket, so it establishes container behavior,
not hardware attestation. The live TDX and 1-MiB checks above are separate.

All four [native platform builds](https://github.com/open-covenant/covenant/actions/runs/39479484329)
also passed at this source, as did the
[Rust and landing CI jobs](https://github.com/open-covenant/covenant/actions/runs/39479474712).
Signing/release was skipped for the PR; no native publication is claimed.

## Retained evidence hashes

These SHA-256 values identify privately retained records. Raw quotes,
account responses, credentials and local paths are not published here.

| Artifact | SHA-256 |
| --- | --- |
| Diagnostic plan | `818e31164c537f5910f0bd6ac2d1e352d3c0f55cde5420bb5bf0f06dbae87201` |
| Diagnostic policy | `fb520ae6f2a23c23023d37e7da99ea1cb3d37a66121aee82661a66414209bb7f` |
| Anonymous registry verification | `9f5dd7cb90a90e8a170618f2d3c387c55ed7f6804ed2fd7cb77c8f8483790e02` |
| Hardware verification result | `9897e2656319b1fd7df6dd15603c2b6e6e3e334d4de42b41aeb3b9d1fba68f75` |
| SSH raw runtime result | `e76be7dba89744256b99efeb23fdadac31a6ce7a42cbae75bc7068ad3b1b2ac8` |
| SSH acceptance/cleanup record | `bf724392c1a122053e2ecc93b95c0d44a20d92652abc1fe9c73c86210632f014` |
| Provider operation response | `bb5f05988bf568800327dd1353f275bd68be82df9c5647e643175a0e093c710a` |
| Image CI record | `21e1e7daf47eead2a6a155b012fc6d1050211a539eb4689625eafcad3a4494e5` |

The [KMS assessment](../deploy/compute-attested-cpu/kms-policy-review.md)
remains rejected: all 18 assessed managed/Base/Ethereum endpoints reported
disabled OS-image verification. This diagnostic neither changes that policy
nor proves provider-independent persistent-secret custody. It created no new
VM and demonstrated no paid coordinator lease, workspace grant, settlement,
provider deletion or production trust promotion. The retained CVM may continue
to incur provider charges; application expiry is not provider teardown.
