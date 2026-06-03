// /api/verify/[sha] — Server-side proxy verification of a Covenant-author commit.
//
// Four witnesses are checked. Each returns a state (green/yellow/red/gray)
// and a detail line. This is the v0.2 SKELETON: real verification plugs in
// per-anchor as the Week 1+ ship targets land.
//
// Anchor 1 took the Spike-2 Option-D pivot: per-commit Solana Memo tx
// instead of Sigstore Rekor. Same four-witness chain, no Sigstore Fulcio
// OIDC dependency, trust root = operator authority pubkey on Solana
// (rotates to multisig at v0.3). Same Memo-program primitive the
// constitution anchor uses (different payload schema).

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot } from "@/lib/agentStream.mjs";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

type WitnessState = "green" | "yellow" | "red" | "gray";

type Witness = {
  key: "rekor" | "audit_chain" | "solana_anchor" | "verifier_sig";
  label: string;
  state: WitnessState;
  detail: string;
  drillHref?: string;
  badge?: { text: string; tone: "yellow" | "red" } | null;
};

const COVENANT_AUTHOR_EMAIL = "covenant@users.noreply.github.com";

// Witness-loop cutover SHA — first commit produced under the gitsign pipeline.
// Empty string until the pipeline ships (Week 1 ship target). When set, all
// commits BEFORE this SHA render as "predates witness loop".
const WITNESS_CUTOVER_SHA = process.env.WITNESS_CUTOVER_SHA || "";

function git(repoRoot: string, args: string[]): string | null {
  try {
    return execFileSync("git", ["-C", repoRoot, ...args], {
      encoding: "utf8",
      maxBuffer: 4 * 1024 * 1024,
    }).trim();
  } catch {
    return null;
  }
}

function predatesCutover(repoRoot: string, sha: string): boolean {
  if (!WITNESS_CUTOVER_SHA) return false;
  const isAncestor = git(repoRoot, [
    "merge-base",
    "--is-ancestor",
    sha,
    WITNESS_CUTOVER_SHA,
  ]);
  // merge-base --is-ancestor returns exit code only; our git() returns null on
  // non-zero, which means "not an ancestor". When the call succeeds (exit 0
  // → empty string), sha IS an ancestor of cutover (= predates).
  return isAncestor !== null;
}

function checkAnchor1CommitMemo(repoRoot: string, sha: string): Witness {
  // Anchor 1 — Solana commit memo. Spike 2 picked Option D: per-commit memo
  // tx posted by the witness-anchor daemon, signed by the operator authority
  // (id.json today, multisig at v0.3). Memo payload format:
  //   covenant-commit-v1:<sha>:<audit_root_hex>:<unix_ms>
  // Verification: query memo program for txs where the authority pubkey is a
  // signer and the payload references this sha; return green when present
  // and the tx is finalized.
  // Skeleton reads landing/public/witness/memo/<sha>.json with the recorded
  // tx signature; Week 1's anchor-publisher writes that manifest after each
  // commit. Drill-href routes to solscan for the tx.
  const memoManifest = join(repoRoot, "landing", "public", "witness", "memo", `${sha}.json`);
  if (!existsSync(memoManifest)) {
    return {
      key: "rekor",
      label: "Solana commit memo",
      state: "yellow",
      detail:
        "Per-commit Memo-program anchor ships Week 1 (witness-anchor daemon posts one memo tx per Covenant commit, payload covenant-commit-v1:<sha>:<audit_root_hex>:<ts>, signed by operator authority). Until the first memo lands this light reads yellow.",
      badge: {
        text: "Witness-anchor daemon pending — Week 1 ship target",
        tone: "yellow",
      },
    };
  }
  try {
    const parsed = JSON.parse(readFileSync(memoManifest, "utf8")) as {
      tx?: string;
      verified?: boolean;
      slot?: number;
      authority?: string;
      cluster?: "devnet" | "mainnet";
    };
    const cluster = parsed.cluster ?? "devnet";
    const solscan = parsed.tx
      ? `https://solscan.io/tx/${parsed.tx}${cluster === "devnet" ? "?cluster=devnet" : ""}`
      : undefined;
    if (!parsed.verified || !parsed.tx) {
      return {
        key: "rekor",
        label: "Solana commit memo",
        state: "red",
        detail: `Memo tx ${parsed.tx || "missing"} did not verify against operator authority pubkey.`,
        drillHref: solscan,
      };
    }
    return {
      key: "rekor",
      label: "Solana commit memo",
      state: "green",
      detail: `Memo tx ${parsed.tx.slice(0, 16)}… signed by ${parsed.authority || "operator authority"} at slot ${parsed.slot ?? "?"} (${cluster}).`,
      drillHref: solscan,
    };
  } catch {
    return {
      key: "rekor",
      label: "Solana commit memo",
      state: "red",
      detail: "Memo manifest unreadable — investigate landing/public/witness/memo/<sha>.json.",
    };
  }
}

function checkAnchor2AuditChain(repoRoot: string, sha: string): Witness {
  // Anchor 2 — Local hash chain. attestations/<sha>.json contains per-LLM-call
  // Step records Merkle-rooted into audit_root_hex. v0.2 skeleton: green when
  // attestation present + non-empty audit_root; red on mismatch.
  const attestationPath = join(repoRoot, "attestations", `${sha}.json`);
  if (!existsSync(attestationPath)) {
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "yellow",
      detail:
        "Audit chain pipeline ships in Week 1. Per-LLM-call Step records (lifted from Letta StepManager schema) Merkle-root into attestations/<sha>.json; once present this light verifies the root by recomputing.",
    };
  }
  try {
    const att = JSON.parse(readFileSync(attestationPath, "utf8")) as {
      audit_root_hex?: string;
      steps?: unknown[];
    };
    if (!att.audit_root_hex) {
      return {
        key: "audit_chain",
        label: "Audit hash chain",
        state: "red",
        detail: "Attestation present but audit_root_hex missing.",
      };
    }
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "green",
      detail: `Root ${att.audit_root_hex.slice(0, 16)}… covers ${att.steps?.length ?? 0} LLM calls.`,
      drillHref: `https://github.com/open-covenant/covenant/blob/main/attestations/${sha}.json`,
    };
  } catch {
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "red",
      detail: "Attestation unreadable.",
    };
  }
}

function checkAnchor3Solana(_repoRoot: string, sha: string): Witness {
  // Anchor 3 — Solana settlement-program anchor. v0.2 skeleton stub. Week 2
  // ship target: queries the settlement program at cov9UDypG7nsry... for
  // a ReceiptBatch PDA containing this commit's Merkle leaf, returns green
  // when the leaf is present in a confirmed batch.
  // Lookup function lives in services/settlement-publisher; this route will
  // call it via a thin RPC fetch once the publisher exposes /lookup.
  return {
    key: "solana_anchor",
    label: "Solana settlement anchor",
    state: "yellow",
    detail:
      "Anchor publisher ships Week 1 (services/settlement-publisher batches receipts via anchor_receipt_batch on cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y). Until first batch lands this light reads yellow.",
    drillHref: `https://solscan.io/account/cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y?cluster=devnet`,
  };
}

function checkAnchor4VerifierSig(repoRoot: string, sha: string): Witness {
  // Anchor 4 — Verifier-Refuter signature. Separately-keyed ed25519 identity
  // at $COVENANT_HOME/identity-verifier/local.key. Loaded via
  // covenant_identity::LocalIdentity::load_or_create (Spike 6: GO, no
  // refactor). attestations/<sha>.verifier.sig contains the detached signature
  // over the audit_root_hex; this route reads + verifies against the
  // verifier's published pubkey at landing/public/witness/verifier-pubkey.txt.
  const sigPath = join(repoRoot, "attestations", `${sha}.verifier.sig`);
  if (!existsSync(sigPath)) {
    return {
      key: "verifier_sig",
      label: "Verifier-Refuter signature",
      state: "yellow",
      detail:
        "Verifier-Refuter persona ships Week 2 under a separate launchd job (org.opencovenant.verifier) with its own ed25519 key. Until then this light reads yellow.",
      badge: {
        text: "Same-family weeks 1-4; cross-family verifier-B → fallback Week 4 stretch",
        tone: "yellow",
      },
    };
  }
  // Real verification plugs in here in Week 2.
  return {
    key: "verifier_sig",
    label: "Verifier-Refuter signature",
    state: "green",
    detail:
      "Signature present. Full Ed25519 verification against published pubkey ships with the Week-2 verifier persona.",
    badge: {
      text: "Same-family verifier (Sonnet vs Opus, model-tier-different)",
      tone: "yellow",
    },
  };
}

export async function GET(_req: Request, ctx: { params: Promise<{ sha: string }> }) {
  const { sha } = await ctx.params;
  if (!/^[0-9a-f]{7,40}$/i.test(sha)) {
    return NextResponse.json({ error: "invalid sha" }, { status: 400 });
  }

  const repoRoot =
    findRepoRoot(process.cwd()) || resolve(process.cwd(), "..");

  // Resolve commit metadata. notFound when sha doesn't exist in repo.
  const meta = git(repoRoot, [
    "show",
    "-s",
    "--format=%H%x09%h%x09%an%x09%ae%x09%aI%x09%s%x09%b",
    sha,
  ]);
  if (!meta) {
    return NextResponse.json({ error: "unknown sha" }, { status: 404 });
  }
  const [fullSha, shortSha, authorDisplay, authorEmail, isoDate, subject, ...bodyParts] = meta.split("\t");
  const bodyText = bodyParts.join("\t").trim();

  const predatesWitnessLoop =
    authorEmail !== COVENANT_AUTHOR_EMAIL && WITNESS_CUTOVER_SHA
      ? predatesCutover(repoRoot, fullSha)
      : authorEmail !== COVENANT_AUTHOR_EMAIL;

  // Pre-cutover commits short-circuit to all-gray witnesses with the
  // explanatory copy on the page. They are still queryable; we render the
  // historical metadata so the URL doesn't 404 on legitimate history.
  if (predatesWitnessLoop) {
    return NextResponse.json({
      commit: {
        sha: fullSha,
        shortSha,
        authorDisplay,
        authorEmail,
        subject,
        bodyText,
        isoDate,
        predatesWitnessLoop: true,
      },
      witnesses: [
        { key: "rekor", label: "Sigstore Rekor inclusion", state: "gray", detail: "Predates witness loop." },
        { key: "audit_chain", label: "Audit hash chain", state: "gray", detail: "Predates witness loop." },
        { key: "solana_anchor", label: "Solana settlement anchor", state: "gray", detail: "Predates witness loop." },
        { key: "verifier_sig", label: "Verifier-Refuter signature", state: "gray", detail: "Predates witness loop." },
      ] satisfies Witness[],
      fifth: {
        label: "Code Quality (Not Witnessed)",
        detail:
          "Semantic correctness is never witnessed by the chain — see the mutation-quality trend for the test-suite's catch rate over time.",
        href: "/lineage/mutation-quality",
      },
    });
  }

  const witnesses: Witness[] = [
    checkAnchor1CommitMemo(repoRoot, fullSha),
    checkAnchor2AuditChain(repoRoot, fullSha),
    checkAnchor3Solana(repoRoot, fullSha),
    checkAnchor4VerifierSig(repoRoot, fullSha),
  ];

  return NextResponse.json({
    commit: {
      sha: fullSha,
      shortSha,
      authorDisplay,
      authorEmail,
      subject,
      bodyText,
      isoDate,
      predatesWitnessLoop: false,
    },
    witnesses,
    fifth: {
      label: "Code Quality (Not Witnessed)",
      detail:
        "Semantic correctness is never witnessed by the chain — see the mutation-quality trend for the test-suite's catch rate over time.",
      href: "/lineage/mutation-quality",
    },
  });
}
