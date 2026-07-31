# Witness Loop v0.2 — Historical Architecture Note

This document records a design and its prototype surfaces. It is not a claim that every component is deployed or that the witness mediates an agent runtime. The full historical plan lives outside the repo at `~/covenant-witness-loop-plan-v0.2.md`; this is the in-tree summary.

The current public surface checks structural properties of supplied repository artifacts: hashes, one self-published-key signature, and event ordering. Its Solana cards read publisher-authored manifests; they do not currently query or decode RPC state. It cannot establish semantic correctness, evidence completeness, runtime mediation, or W009/W011 enforcement.

## The bet

The design makes specific artifact relationships checkable: a commit identifier, supplied event lines, hash-chain root, configured on-chain observations, and signatures. Those checks support provenance analysis; they do not certify that autonomous work was correct, complete, policy-compliant, or exclusively produced by the claimed runtime.

## Three concentric rings

### Ring 1 — Multi-artifact witness

The design associates up to four artifacts with a commit:
- **Anchor 1: Solana commit memo** (Spike 2 Option-D pivot from gitsign/Rekor). Per-commit Memo-program tx posted by the witness-anchor daemon. Payload: `covenant-commit-v1:<sha>:<audit_root_hex>:<unix_ms>`. Signed by operator authority (`id.json` today, multisig at v0.3). Memo program `MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr` — same primitive the constitution anchor uses, different payload schema. Cluster: devnet for v0.2, mainnet at launch. Verification: query memo program txs by authority pubkey, find one referencing this sha, confirm signature + finality on Solana. No Sigstore Fulcio OIDC dependency; trust root is the operator authority on Solana.
- **Anchor 2: Local hash chain.** Per-commit `Step` records (model + tokens + durations + trace_id, schema lifted from Letta StepManager) Merkle-rooted into a per-commit `audit_root_hex`. Stored as `attestations/<sha>.json` next to the commit. The same `audit_root_hex` is the second field of the Anchor 1 memo payload, so Anchors 1 and 2 are cryptographically linked.
- **Anchor 3: Solana settlement-program anchor.** Merkle root of N commits batched and posted via `anchor_receipt_batch` on the existing settlement program at `cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y`. ReceiptBatch PDA carries the root. Coexists with Anchor 1 on Solana; Anchor 1 is per-commit and Anchor 3 batches commitments. Both are configured chain observations, not evidence that a runtime enforced the recorded policy.
- **Anchor 4: Self-published verifier statement.** The v2 statement signs a closed canonical envelope containing the root, event count, exact pass/refute verdict, refutations, and verifier key. The key is stored beside the artifact, so signature validity proves only byte consistency and possession of that bundled key; attribution requires an external key pin. Replacing the repository key and statement remains possible. The separate v3 reference harness uses a root-signed role document, but it is not wired into this legacy page.

Per-commit Merkle leaf format:
```
leaf = sha256(commit_sha || audit_root_hex || verifier_verdict_hash || transcript_cid)
```

Batches anchor every ~10 commits OR 15 min, whichever first.

### Ring 2 — Constrained-Surface Self-Improvement

The design scopes Modifier writes with `covenant_permissions::scaffold_mutation_scope`. Intended paths:
- `agent-os/agents/tools/**`
- `agent-os/autonomy/tasks/**`

The intended mutation allowlist excludes supervisor scripts, verifier code, the constitution, and `/verify`/api routes. A recorded allowlist decision is not proof that a separate OS boundary mediated every write.

Recovery is a pure-functional 4-state machine (lifted from letta-code's turn-recovery-policy):
- `resolve_approval_pending` — pause for human signal
- `retry_conversation_busy` — backoff + reissue
- `retry_transient` — wait for transient (rate limit, RPC blip) to clear
- `rethrow` — hard fail, escalate to sprint-end

Playbook stored as Letta ToolRule DSL in `agent-os/autonomy/playbook.rules.toml` (replaces prose `PLAYBOOK.md` over time). Self-improvement = sleeptime reflector (separate launchd `com.covenant.reflector`) edits rules. Modifier doesn't edit its own gate.

Pre-run checkpoint (lifted from deer-flow `_rollback_to_pre_run_checkpoint`):
```
agent-os/autonomy/checkpoints/<run_id>.json = {
  head_sha,
  porcelain_hash,
  events_jsonl_byte_offset
}
```

Loop-detection (lifted from deer-flow LoopDetectionMiddleware): MD5(tool_name, args) at 3/5 threshold + per-tool freq at 30/50. Soft warn injects reminder; hard strip rolls back via checkpoint.

### Ring 3 — Public witness surface

Designed routes on opencovenant.org:
- `/verify/[sha]` — four structural evidence checks plus a permanent yellow "Code Quality (Not Witnessed)" status
- `/lineage` — evolutionary tree of accepted scaffold revisions
- `/witness` — live feed of in-flight verifier circles

Current checks when the corresponding artifacts are present:
- Anchor 1 light: displays the transaction, authority, cluster, slot, and `verified` fields claimed by `landing/public/witness/memo/<sha>.json`. It stays yellow because the page does not fetch or decode the transaction, bind the memo to the commit/root, authenticate the authority, or establish finality.
- Anchor 2 light: audit hash chain root matches recomputed root from `attestations/<sha>.json`. The recomputed root must equal the `audit_root_hex` field in Anchor 1's memo payload — that linkage means a tampered audit chain breaks Anchor 1 too.
- Anchor 3 light: compares the settlement manifest root with the run-file root and displays the claimed PDA, transaction, cluster, and slot. It stays yellow because no PDA or transaction is fetched or decoded and finality is not established.
- Anchor 4 light: rejects legacy root-only signatures and any v2 artifact without `landing/public/witness/verifier-keys/<sha>.txt`, then verifies the closed statement over the root, event count, exact verdict, refutations, and matching commit-scoped key. A signed refutation is red; a valid pass stays yellow because that key is self-published beside the artifact. `verifier-pubkey.txt` is latest-only compatibility metadata and is not used as the historical key. Existing v1 root-only artifacts intentionally render red.

Fifth Status Line: permanent yellow "Code Quality (Not Witnessed)" — links to `/lineage/mutation-quality` showing the cargo-mutants nightly trend (Spike 13). Honest UX discipline: the chain does NOT prove semantic correctness; the fifth line explicitly states this.

The historical SAME-FAMILY badge called out correlated verifier and modifier models. Using a different model family can reduce one correlation, but it still does not establish organizational, infrastructure, input, code, or runtime independence.

## Memory layer (three-tier per Letta naming)

- **Recall (already exists):** `agent-os/autonomy/events.jsonl` — append-only transition ledger
- **Core (new):** `agent-os/autonomy/blocks/playbook.md`, `blocks/lessons.md` — Letta block model, git-backed, auto-anchored by rcommit + cosign
- **Archival (new):** `covenant-memory/index/` — sqlite-vec + tantivy fts5, 9-step hybrid retrieval (semantic + BM25 + entity boost) with RRF + sigmoid-attenuation scoring (lifted from mem0)

Fact extraction: mem0 ADDITIVE_EXTRACTION_PROMPT v3, single LLM call per sprint, ADD-only with `linked_memory_ids[]`, md5-hash dedup, UUID→int anti-hallucination remap. Lands in `facts.jsonl` + sqlite-vec index.

Sleeptime reflector (separate launchd `com.covenant.reflector`) runs every N sprints, emits CRUD ops over `blocks/`.

## Planned ConstitutionVerifier checks

The design evaluates the following checks over supplied repository and run data. An on-chain commitment can make later mutation visible; it does not prove that these checks mediated every runtime action.
- Identity-token regex (banned strings never appear)
- Autonomous-author check (`Covenant <covenant@users.noreply.github.com>` for autonomous commits)
- `scaffold_mutation_scope` path allowlist
- No supervisor/handover/launchd-plist modification
- No centralized-API additions
- No test deletion (extend OK; remove or weaken: reject)

## Skipped / explicitly out of scope for v0.2

- Cross-family verifier as DEFAULT (Spike 8 confirmed OpenAI route viable, but the v0.2 ship treats cross-family as fallback per investor stress-test; OpenAI primary cross-family in Week 4 if shadow mode is clean Week 3)
- ZK proofs of computation (v0.3 roadmap card)
- TEE attestation (v0.3+ roadmap card)
- Bundle-format browser-side cosign (Path 2 in Spike 7; v0.2.x)
- Multi-agent economy with $CVNT-staked bidding (v0.3+)
- Decentralized constitution governance (v0.4)

## $CVNT wiring (Week 2-3 ship targets)

Three layers:
1. **Anchor-batch fees (Week 2, committed).** `anchor_receipt_batch` routes a $CVNT-denominated fee from operator treasury to reward vault per batch. On-chain footprint carries $CVNT.
2. **Scaffold-rev bonds (Week 3).** Modifier posts $CVNT bond when proposing scaffold rev; slashed if Verifier-Refuter rejects.
3. **Verifier-circle staking (v0.3).** Verifier-Refuter stakes $CVNT to sign verdicts; slashed by ClaimDivergence resolutions.

## Decentralization roadmap

- **v0.2 (launch):** operator hardware wallet keystone for constitution + upgrade authority
- **v0.3.0 (week 10):** 2-of-3 multisig replaces operator wallet
- **v0.4:** $CVNT lock-weighted governance over constitution updates

## Where to look

- Plan (full, ships local): `~/covenant-witness-loop-plan-v0.2.md`
- Mining steal list (ships local): `~/covenant-witness-loop-steal-list-v0.2.md`
- Day 0 spike log (append-only): `~/covenant-day-0-spike-log.md`
- This overview: `docs/provenance/witness-loop-overview.md`
- Constitution anchor: `docs/provenance/constitution-anchoring.md`
- Mutants nightly: `docs/provenance/covenant-mutants-nightly.md`
- Witness verifier UI: `landing/app/verify/[sha]/page.tsx`
- Server-side verification proxy: `landing/app/api/verify/[sha]/route.ts`
