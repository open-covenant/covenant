# Native Phala no-KMS acceptance — 2026-10-20–21 UTC

**The coordinator integration is deployed at `6598fe15`, but both authorized
paid attempts failed production qualification.** Each buyer admission and real
client upload passed. The first attempt exceeded its 60-second deletion gate;
the separately authorized October 21 retry exceeded its fixed 300-second gate
by 18.926 seconds. Both original VMs were subsequently confirmed absent, and
both failed receipts charged zero. Buyer Compute credit is fully restored.
Both on-chain escrow returns finalized. The October 21 record below separates
that reconciliation from the subsequent local, undeployed poll-only deletion repair.
No third attempt or standing production capacity is authorized.

This is separate from the earlier [isolated no-KMS acceptance](compute-ephemeral-phala-acceptance.md).
The served trust ceiling remains `Open`; neither managed-KMS qualification nor
persistent-secret custody is claimed.

## October 20 candidate and deployment

| Item | Verified value |
| --- | --- |
| Coordinator revision | `da00ccf7541d94cfb97c2a43dd9dd96f6719f499` |
| Coordinator image | `ghcr.io/open-covenant/covenant/compute-coordinator-public@sha256:795fdd1dce0536b5a6526b1ad6fde732f14541751cb54486a4dcfb4054679140` |
| Node image qualified | `ghcr.io/open-covenant/covenant/compute-node-public@sha256:1f7398191a6d0457ec42a489ea048f7f65a54c50fdffb38a78d070b94b373d25` |
| Local buyer/node implementation | `ee37d807aa4b2327a7b813ff44e35f7e8acec548`, immutable locally built binaries |
| Guest image | `sha256:925122b9c68fe8f385784b3d926685f6e08f59467c5a76fbfe71faa701d81932` |
| Model/profile | `phala-cpu-ephemeral-dstack061` / `phala_ephemeral_dstack061` |
| Render deployment | `dep-dbbusarl9sls739bdq4g`, live at 22:42:18 UTC |

The difference from `ee37d807` to `da00ccf7` fixes a test-clock race and adds a
development-only Tokio feature; production implementation is unchanged. The
exact deployed revision passed push/PR CI, release builds and image builds.
Every coordinator and node image layer was downloaded anonymously and checked
against its content digest. Local and CI validation covers the independent
attestation, native buyer, relay, Phala lifecycle and settlement paths.

Only hardware lease pins, the ephemeral relay, and its public origin were added
to production configuration. All 82 existing environment values, the existing
disk and service configuration were preserved. The native journal retained all
119 pre-test jobs and the same idle financial counters. Startup compacts that
journal, so this does not claim byte equality or a captured comparison of every
historical job field. The older unresolved canary obligation and its reservation
were preserved; this acceptance did not resolve that separate obligation.

Relay and hardware state lock files were confirmed under the persistent `/data`
mount in the running service. Unsigned relay requests returned 401. These
checks establish initial persistence configuration, not a completed relay
restart acceptance.

## October 20 identities and bounded spend

| Item | Value |
| --- | --- |
| Job | `23b95fa0-d813-4b6d-805b-970ddf6d05d7` |
| Buyer | `7dd9Lzn4K1MokMqKyn3C4cKjVzQ2YZffVSFthasZ7xFZ` |
| Operator | `6iB1M8xKhMWHxRrZjy2NYqop9iCQ74ARP3H1zdmZKY21` |
| Fresh provider VM | `7fe4c48b-7757-4102-9d90-62e0de8639d1` |
| Lease maximum | 900 seconds at 34 micro-USDC/second; 30,600 micro-USDC total |
| Provider allowance | One new VM; $0.05 planning reservation, not an invoice cap |

The authorized 1,000,000-CVNT stake finalized before admission. Its transaction
is [kQ1QpfVL…](https://explorer.solana.com/tx/kQ1QpfVLbsnHTLmUDCKacpcmzJNmgQC5XAqNa51gD9LS1vprvNve5PKMgKaocYHBtuhFqWcGTqym3NRDvewtezw),
with lock expiry 2026-10-27 22:48:30 UTC. The one-use provider allowance pins
this exact buyer and job. Deletion does not replenish that allowance. The
pre-existing diagnostic VM was not adopted or deleted.
After the failed lease, finalized reads still showed the full 1,000,000 CVNT in
the original position and vault, with unchanged lock expiry and no deduction.

## October 20 live result

1. The native node allocated one fresh VM/disk with the no-KMS profile. Operator
   and coordinator evidence agreed on the independently verified TDX app/boot
   identity and original buyer, job, workload, host key and lease-expiry scope.
   Each quote bound its own separate fresh challenge and nonce.
2. The buyer independently verified admission and opened the original signed
   relay session within its quote deadline. It uploaded **81,920 bytes** from
   the client to `/workspace/native-acceptance.bin` as unprivileged UID 10001.
   The returned file hash matched the client source:
   `d980aa7bb22cd97d3d80d5c68cdb9fc4f09bb63c18819cbbb8abb40597025ca6`.
   This is a native client upload, not a file generated inside the guest.
3. Explicit lease close succeeded at 22:50:16.056 UTC. The node requested
   deletion of the original owned VM and retained the cleanup intent while
   Phala continued returning that VM.
4. The 60-second cleanup deadline expired. The signed, verified terminal error
   was `ephemeral Phala deletion is unconfirmed; original resource and cleanup
   intent remain durable; successful receipt refused`.
5. The recovery pass stamped 22:51:38.480 UTC reached durable `deleted`, about
   82 seconds after close; that stamp is not an exact fsync-completion time.
   A separate authenticated read of that exact VM returned 404
   at 22:52:50.614 UTC. Late deletion did not change the failed job into a
   successful lease.

The node's one-use allowance is consumed and the operator is not offered as
production capacity. The deployed coordinator capability remains available for
explicitly authorized qualification; standing production creation is not
enabled. The operator process exited cleanly after confirmed provider deletion
and finalized escrow return. A public status read confirmed `offline`,
`live=false`, `matchable=false`, and no Phala entry in advertised capacity.
The failed attempt is retained in operator reputation as one fault; no stake
deduction was observed.

## October 20 final financial reconciliation

| Item | Final result |
| --- | --- |
| Native job | `failed`, verified receipt, `execution_failed` |
| Buyer charge / operator payout / marketplace fee | 0 / 0 / 0 micro-USDC |
| Buyer available Compute credit | 30,600 micro-USDC |
| Local budget | Pending 0; settled 0; remaining 30,600 micro-USDC |
| Original on-chain renter escrow | Full 30,600 micro-USDC returned, finalized |
| Original escrow vault | Absent |
| Provider VM | Durable `deleted` plus independent exact-UUID 404 |

The original lease terms were voided by finalized
[4iDeKeDit…](https://explorer.solana.com/tx/4iDeKeDitRsXttVURqgiANBu6xUpFBUdyTQJaFFsbs2NvHLuh7SePNuNGeCnVb3tCByx7TC6UTNgcdb3eePKU6RL).
Finalized [29wpMntra…](https://explorer.solana.com/tx/29wpMntraAoDD7HM7J4MhoD5FeiLnHCbWSjBSoSG6YjKwCdG6HYThzmEvJ49vwFY7vVh21CYR6zqN4zC8AJNErnw)
returned exactly 30,600 USDC base units to the coordinator renter wallet.
Both original terms payment flags are set and the vault is absent. The failed
path retains an unconcluded meter owned by the delegation program. Its retained
L1 state does not establish the final active rollup meter state, and this is not
a claim that every on-chain account was closed. Restored buyer credit remains
Compute credit, not a withdrawal to an external buyer wallet. Provider billing,
transaction fees and the CVNT lock are separate from the zero Compute charge.

## First repair, deployed for the October 21 retry

The first follow-up source provides 300 seconds for deletion and 60 seconds for result
delivery in new signed ephemeral orders, without extending guest access, signed
lease expiry or metered runtime. Raising only the cleanup timeout would be
insufficient for a lease that runs to expiry, because the generic result slack
is also 60 seconds. The fixed first-close cutoff is durable, capped by the
original result deadline minus delivery reserve, and shared across initial
close and final confirmation with an in-process monotonic timer. Deletion is
timestamped after the absence GET completes. Late confirmation is refused;
historical rows without the new metadata remain cleanup-only.

Focused regressions exercise the observed 82-second deletion, a 301-second
timeout, an absence GET returning after the cutoff, restart/legacy state and
clock boundaries. Coordinator tests deliver success 270 seconds after early
close and 100 seconds after guest expiry without increasing native or recorded
on-chain gross; duplicate result pays once, and delivery past the original
signed deadline refunds and voids instead. These regression tests preceded the
separately authorized October 21 retry. A separate HTTP regression rejects a new order
with the old 60-second slack before any hold or debit, while the stored original
failed envelope still returns its unchanged historical result. That correction
was subsequently deployed at `6598fe15663ad88fe256ad739ded703d05e1ad43`, as
recorded below; the October 20 evidence remains unchanged.

The original failed envelope, resource, receipt and reservations remain in the
record. The retry used a distinct job and one-use allowance after fresh approval;
it did not reopen the October 20 envelope or replenish its consumed allowance.

## October 20 private evidence hashes

Raw quotes, collateral, management responses, signed requests and credentials
remain private. Selected retained SHA-256 hashes:

| Evidence | SHA-256 |
| --- | --- |
| Exact-head CI summary | `fd43c7183ee0a2f424edbb4bfa6f76b28f77d75000e465a906f0e76537438a2a` |
| Deployment preservation validation | `85a47438bd7f4bc942f181d71de9d6dc12431b19ca62957f4e48b7a1275f7e4d` |
| Initial persistent feature paths | `5111fb7afaf641725d861364e7cda35c9e3a35777d600773e6f73b0230060cba` |
| Verified native client upload | `5118c61bcddb540dda2bd05483aabbd7ebcf09ea75ad6becc9ef33fdfc1e7b3f` |
| Independently confirmed provider deletion | `14d5c7ee3b0ddd793a15b8740e1f3fbcfc646de3079c648faf20bb89ce5a81b8` |
| Failed receipt and finalized escrow reconciliation | `a99c8712d584fcd3589cac0629238d4d569269664927e105cab0d629b54d3790` |
| Public operator/capacity status | `6a9be1b22397169715adb86a1c4c429d3cdd5d0d3a7c9cd3df5ffbfd616ae2ca` |

## October 21 repair deployment and authorized retry

| Item | Verified value |
| --- | --- |
| Coordinator and buyer/node source | `6598fe15663ad88fe256ad739ded703d05e1ad43` |
| Coordinator image | `ghcr.io/open-covenant/covenant/compute-coordinator-public@sha256:c33f18c4ffb87af8068c67432de7825e8ae6c9f95cc271735cc23304f74eb978` |
| Node image qualified | `ghcr.io/open-covenant/covenant/compute-node-public@sha256:fe7bf4051967385d1a228e09b05a3c479a09f150b77e4042afdf90e87d811d15` |
| Render deployment | `dep-dbc655ui0phs73ahbn1g` |
| Retry job | `ede12c80-7a4b-43ed-9c83-4f16d0c4ad34` |
| Fresh provider VM | `b6baaa76-9ee1-4db8-85c6-a49dc5f4b254` |
| Buyer, operator, guest image and profile | Same identities and pins as above; a new guest boot and SSH key |
| Lease maximum | 900 seconds at 34 micro-USDC/second; 30,600 micro-USDC total |
| Provider allowance | A separate one-use allowance for this job; $0.05 planning reservation, not an invoice cap |

The exact source passed CI, release and image qualification before the
image-only deployment. Post-deployment validation confirmed all 85 environment
values, the service and persistent disk, all 120 existing native jobs and their
financial facts, and exact relay/hardware snapshots were preserved. The original
failed/refunded Phala job and older unresolved canary hold remained intact.
The canary preservation check does not resolve that separate historical hold.

1. The node created one fresh no-KMS VM/disk. Independent operator,
   coordinator and buyer verification accepted the original signed workload,
   fresh TDX boot identity and bound SSH key. The native buyer uploaded
   **81,920 bytes** to `/workspace/native-acceptance.bin` as UID 10001;
   the client and guest SHA-256 again matched
   `d980aa7bb22cd97d3d80d5c68cdb9fc4f09bb63c18819cbbb8abb40597025ca6`.
2. Explicit close completed at **07:03:07.225 UTC** and stopped the meter.
   The node retained the original cleanup obligation and fixed the cutoff at
   **07:08:07.890 UTC** (`1792566487890`). Guest access, the signed expiry,
   price ceiling and result deadline were not extended.
3. Provider absence was still unconfirmed at the cutoff, so the native result
   was a verified execution failure. Recovery continued for the same UUID.
   The journal recorded **101 DELETE attempts** before reaching durable
   `deleted`, with absence confirmation stamped **07:08:26.816 UTC**
   (`1792566506816`), **18.926 seconds too late** to qualify success.
4. A separate authenticated exact-UUID GET returned **404 at
   07:14:02.187 UTC**. No replacement VM was created, and no node pause or
   deadline reset was used. Eventual deletion does not revise the failed receipt.

The failed receipt verifies as `no_payout_due`. Native buyer charge, operator
payout and marketplace fee are all zero; available Compute credit is 30,600
micro-USDC, with budget pending 0 and settled 0. This is restored Compute
credit, not an external buyer-wallet withdrawal.

Independent finalized reads confirmed the original terms were voided by
[2LbBbxx1…](https://explorer.solana.com/tx/2LbBbxx1jCGHUXzhppt79jN26GKoPqSVLcmza5FkxULqGezi992i8MCkzAW9Lcjg4rkUYJQvccVcDdfKh4uz2ZGm).
The original renter escrow returned exactly **30,600 micro-USDC (0.0306 USDC)**
in [4SptxeQB…](https://explorer.solana.com/tx/4SptxeQBXYDwUBLE928bFN7xctUdBGHjaM2uW6U7ZQVSnEafwZNn9FkkAhhcB4UotrV61KsTUE9HgxqZBVDaHLKv),
and the vault is absent. The terms retain their voided/paid flags. The failed
path's meter remains delegation-owned and unconcluded with retained L1
`metered_ms=0`; this does not prove the final active rollup state or closure of
every chain account. The original failed receipt is unchanged. Finalized stake
position and vault reads still contain the full 1,000,000 CVNT with the original
2026-10-27 22:48:30 UTC lock expiry and no deduction.

After confirmed VM absence and the finalized audit, the test node stopped
cleanly. Public status showed `offline`, `live=false`, `matchable=false`, and
no Phala capacity entry. Both failed attempts remain in reputation as two
faults; neither accounting history nor reputation was reset.

## Current repair and production boundary

The deployed node retried DELETE while GET still returned the VM. Phala's API
describes HTTP 204 as deletion initiated, rather than completed. The repeated
requests may have contributed to delay, but this test does not prove that
Phala restarted deletion on each request. Provider latency remains a possible
contributor. See the [provider deletion contract](https://docs.phala.com/api-reference/cvms/delete-cvm).

The local follow-up repair durably records an unknown outcome before the first
DELETE, records acceptance after success, and then polls GET without reissuing
DELETE. Existing rows with prior attempts and provider responses already showing
deletion in progress also become poll-only after exact ownership checks.
Timeout, lost acknowledgement or restart cannot cause a blind resend; even a
definite refusal retains the original obligation for operator reconciliation.
Only an authenticated exact-UUID absence observation can confirm deletion.
Original ownership, the fixed cleanup cutoff, late-confirmation refusal and
all financial bounds remain unchanged. The repair passed 52 Phala tests,
13 shared lease tests, four admission integration tests, strict node Clippy,
formatting and 224 repository guards. This repair is **local and not deployed**;
these checks do not establish successful paid acceptance.

Both one-use creation allowances are consumed. There is no standing Phala
production capacity or authorization for a third paid attempt. Successful
operator payout and complete paid settlement qualification remain unproven.
General activation requires the repaired complete path to pass a separately
approved bounded acceptance and an explicit standing operator policy. The
served ceiling stays `Open`, and managed-KMS qualification remains rejected.

## October 21 private evidence hashes

| Evidence | SHA-256 |
| --- | --- |
| Image-only deployment preservation validation | `7b0666568997a76270be90f3531aac7354c439f4cf355416c1cfd7a4b8bdaa9c` |
| Verified native client upload | `a633ac3f2e643f765e619a14c21aa5fc231f04636af56d59ece78b70c9c3e056` |
| Independent exact-UUID deletion confirmation | `f7362d8d290df9cc3f4509b9f98a24dedcc72374e6f6e59017cf75196ac9ecf9` |
| Native no-payout verification | `9b39f313eca65e26bdfcd1845425e5bba0c3f02f03c56acdc0a9e66e37c29ae0` |
| Restored native buyer credit | `e6765402476b5604cb8cf70ca1ca8926050572fd40fbd8ea2457ef19bc665b77` |
| Reconciled buyer budget | `b7c90fcb460505bd452ca6cf18f840f282470ad0c7b70c542ba2b2b9cfe57754` |
| Independent failed-receipt, finalized escrow and stake audit | `81109004f5e74b3ef85df944aaf29a4782c30c00ec946fa0e7c43a14a7f57da2` |
| Public post-stop operator/capacity status | `6c678c8f323207d648445fdd34d6b592df10a51d3425274f6232e19e0115baec` |
