# Measured CPU diagnostic evidence — 2026-10-20

A genuine Intel TDX quote from Covenant's Phala diagnostic guest passed the
Covenant verifier with current Intel collateral, `UpToDate` platform and QE
status, and no advisories. The SSH key tested against that guest matches the
key bound into the verified quote. This was a **manually scoped synthetic
diagnostic**, not a paid Covenant lease, customer workload, settlement, GPU
attestation or production trust promotion. The compiled served ceiling and
the returned served class remain **`Open`**.

## Scope and source

| Identity | Recorded value |
| --- | --- |
| Original diagnostic job | `74328493-feaf-4341-ab05-3111a0742739` |
| Phala CVM | `32ed28e4-0a6f-4dff-a9bf-ba84c38d6e9f` |
| Application | `d5f0ef708c68c52916a7b0da313ae416eb0b980d` |
| TDX instance | `fc551bcb5f96283763f42da3578805ec7e2ecfe3` |
| Challenge | `526df65f-8a1e-479e-9c85-7f112f11f1c1` |
| Operator public identity | `APhcYAfCPoPZELd4RqBMNN42UTRM4drxE6WLN8XnbGcE` |
| Buyer public identity | `Ans9dgwsnMByTYnpuy3sspLvLFHaycCS4XTVYWGzSk27` |
| Guest SSH fingerprint | `SHA256:Chime7EtlmhjTYJEcUMUhJZ9F8GXMQ+Haa5Hffdf1WU` |

The SSH endpoint was
`fc551bcb5f96283763f42da3578805ec7e2ecfe3-1022.dstack-pha-prod5.phala.network:443`,
user `covenant`, through a TLS-validated gateway with strict SSH key checking.
The collector used instance port `18090`, mapped to container port `8090`.

Both application containers used
`ghcr.io/open-covenant/covenant/compute-node-public@sha256:922c3ba47a5a20ab30d8603fb08c9fdfdad1f21bd59ec5744848666e82a1058e`.
Its OCI revision label is
[`46af6d7d4f7a3a73795100d0620fab478bc272a5`](https://github.com/open-covenant/covenant/commit/46af6d7d4f7a3a73795100d0620fab478bc272a5).
The corrected client, port mapping and verifier source at
`984fd4114bf3f56926fe6d26fb724c4ead238309` had passing
[PR #369 checks](https://github.com/open-covenant/covenant/pull/369/checks).
The Docker smoke separately exercised real OpenSSH/SFTP, isolation, replay
and expiry using a fake quote socket; those checks are not hardware evidence.

The guest was production
[`dstack 0.6.1`](https://github.com/Dstack-TEE/dstack/releases/tag/mkosi-os-v0.6.1),
source `7f143148cc49f4a09799958835d310e050137cb3`, with 1 vCPU, 2 GiB memory
and the TDX lite profile. Its OS image hash was
`ec43cb8c5e04b6bb6c53b33b16076a01d83d01f7f4b703f2f78a0d947d6247e6`.
The [independent launch replay](../deploy/compute-attested-cpu/measurement/README.md)
derives the expected MRTD/RTMR0–2 from official release artifacts and VM
configuration; quoted register values are not used as expected measurements.

## Recorded results and UTC times

| Event | UTC |
| --- | --- |
| Challenge issued | `2026-10-20T16:22:12.878Z` |
| Quote response completed, HTTP 200 | `2026-10-20T16:22:16.984Z` |
| Current Intel collateral fetched | `16:22:18.324Z–16:22:18.719Z` |
| Strict verification completed | `2026-10-20T16:22:18.734Z` |
| SSH diagnostic, including cleanup | `16:22:33.193Z–16:23:06.982Z` |
| Identical quote request replay rejected, HTTP 409 | `2026-10-20T16:23:11.518Z` |
| Offline binding control and substitutions | `16:23:34.235Z–16:23:34.806Z` |
| Original verdict expires | `2026-10-20T16:27:18.734Z` |
| Retained-record correlation completed | `2026-10-20T16:28:33.295Z` |
| Diagnostic service lease expiry | `2026-10-20T16:38:02.849Z` |
| Active SSH session closed by remote host | `2026-10-20T16:38:02.993Z` |
| Authenticated UI confirmed both containers exited 0 | approximately `2026-10-20T16:38:26Z` |

The verifier returned evidence class `Attested`, served class `Open`, and no
GPU identity. Its mode was the actual current clock, without a clock override.

All 16 SSH diagnostic steps passed: 13 successful SSH/SFTP operations, two
expected network rejections, and local generation of the wrong-buyer test key.
The session ran as uid `10001` in `/workspace`; synthetic file write/read and
SFTP round trips succeeded. Buyer access could not read the host private key,
reach a mounted dstack socket, observe the collector token in its environment,
read root PID 1's environment, or modify the authorization, public-key or
collector-code files. Wrong-buyer authentication and an intentionally wrong
host key were rejected; a final valid login succeeded. Synthetic files were
removed and their directory deletion confirmed.

A separate correlation record recomputed the public-key fingerprint, checked
the dedicated `known_hosts` entry, verified every step's stdout/stderr hash,
and matched the original job, generation and expiry. All 15 network steps
occurred after verification and before verdict, challenge and lease expiry.
The original SSH journal remains labelled `unverified`; it was not rewritten
to imply that the SSH runner itself verified hardware.

The unchanged quote and collateral passed an offline control. Substituting
the endpoint, SSH key or workload produced three report-data mismatch
rejections. These checks made no new network requests and finished within
the original challenge window.

The final expiry probe first confirmed working SSH and collector access. An
authenticated SSH command started before the original cutoff and was closed by
the remote host **144 ms after expiry**, without a timeout or its completion
marker. A new SSH attempt failed; the collector completed TLS but returned no
HTTP response. Those network observations alone were labelled ambiguous.
The authenticated Phala runtime view subsequently showed both `dstack-ssh-1`
and `dstack-collector-1` as `Exited (0)`, corroborating shutdown. A separate
correlation record retains this additional evidence without rewriting the
conservative network result. No expiry extension, restart, new quote or
replacement instance was used. This check concerned service lifetime; the
earlier hardware verdict was already expired and was not reused as a grant.

## Retained artifact hashes

These SHA-256 values identify privately retained evidence and logs; the raw
records are not published here.

| Artifact | SHA-256 |
| --- | --- |
| Raw TDX quote bytes | `54b73b6d3b4d293ae84cf09f94f9694b198ff26b142734f1ea4dfb7bee860028` |
| Hardware evidence / verdict evidence digest | `5cf3da701c3b2c88a1ae53c277f3b256b929391ea8ca094888c96ffa0d85fbbc` |
| Exact quote HTTP response body | `b69f3f41ed809f14f4c7d48a73b359d0b941478bbaa3cf1d395aaf5700a4d345` |
| Verification result | `7a84fd5acb9a953543d1c7b977902589230d9f90da75a6aa4c7db8a4463d9c4d` |
| Intel collateral | `a96e48e4d076a7176f739c2957355ca2d4083d804be11624785b38e94db97070` |
| Verifier reference bundle | `05f87257b1bf37a77f1b00d62e144f18dbc23bd6e3be72dbb3fdda6b0b49a841` |
| SSH diagnostic results | `077bb3003843c749227756935d08517d67a4f1749dc82817832f010f0fa858fe` |
| Binding mutation results | `c9c10497ee715b87f5d22496b2a60ddfb5c1b4ab54170eec563da08619965109` |
| Separate correlation record | `a4f65fecb2eec5e36576284abe0bf194b5f8529b192fa0757c288a9e41a3f431` |
| Expiry network diagnostic | `b70688862fa8230b46852044ddf644ae68456f8b450acef95961290e2768cc89` |
| Expiry plus runtime correlation | `ca1621b92a4fe6ab4c6ec9635c185e12bca3b8ef560f7cec4ff79c09e1f4f31d` |

## Remaining acceptance boundaries

The measured raw app-compose hash was
`c4428b75f358f9f3295dea7fa73c59ab590344ee42717e7080c6c6167ea5ac97`;
its nested Compose YAML exactly matched the prepared candidate, SHA-256
`0d35032786f117b2cd654a09d6683f31302bce72d2db70ae78fb199f81972adb`.
The wrapper was initially obtained from guest Info and separately reviewed.
Matching its hash does not establish independently approved provenance or
complete administrative isolation.

In particular, the measured pre-launch script consumes host-controlled
`user_config`, including a root-key import path. The observed serial log
reported **zero imported keys**. Production OS metadata has `is_dev=false`;
the production profile excludes native `sshd`. A key-import path alone does
not prove reachable native SSH. KMS key-release and upgrade policy, privileged
configuration paths, `secure_time=false`, and public log/system-information
settings still require acceptance review. The container access tests do not
close those broader administrative questions.

Live endpoints, container listings and serial logs confirmed the application
services started; serial logs also reported successful `app-compose.service`
completion. The Phala dashboard nevertheless reported failed UpdateCompose
and Boot operations. That provider-status discrepancy remains unreconciled;
this record does not claim a cleanly completed platform operation.

No paid coordinator lease, protected workspace grant, settlement or provider
deletion was demonstrated. The user-created CVM was retained; service expiry
does not stop provider billing. See the
[acceptance plan](compute-hardware-acceptance.md) and
[hardware release boundaries](compute-hardware-attestation.md) before any
production trust change.
