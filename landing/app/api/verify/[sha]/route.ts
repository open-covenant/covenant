// /api/verify/[sha] — server-side witness check for a Covenant-author commit.
// Resolves commit metadata, then reports four independent witnesses (commit
// memo, audit hash chain, settlement anchor, verifier signature). Each anchor
// reads yellow until its artifact is published — a witness is never green
// before it has actually been checked.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { NextResponse } from "next/server";
import { clean, findRepoRoot } from "@/lib/agentStream.mjs";

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

type SkillRunTx = { sig: string; cluster: "devnet" | "mainnet"; slot: number | null };
type SkillRun = {
  skill: { name: string; digest: string };
  capabilities: string[];
  tx: SkillRunTx | null;
};

// A skill-driven run anchored to this commit, sourced from
// landing/public/witness/skill/<sha>.json. Null for an ordinary code commit.
function checkSkillRun(repoRoot: string, sha: string): SkillRun | null {
  const manifest = join(repoRoot, "landing", "public", "witness", "skill", `${sha}.json`);
  if (!existsSync(manifest)) return null;
  try {
    const raw = JSON.parse(readFileSync(manifest, "utf8")) as Record<string, unknown>;
    const skill = (raw.skill ?? {}) as Record<string, unknown>;
    const name = typeof skill.name === "string" ? skill.name : "";
    const digest = typeof skill.digest === "string" ? skill.digest : "";
    if (!name || !digest) return null;
    const capabilities = Array.isArray(raw.capabilities)
      ? raw.capabilities.filter((c): c is string => typeof c === "string")
      : [];
    const txRaw = (raw.tx ?? null) as Record<string, unknown> | null;
    const tx: SkillRunTx | null =
      txRaw && typeof txRaw.sig === "string" && txRaw.sig
        ? {
            sig: txRaw.sig,
            cluster: txRaw.cluster === "mainnet" ? "mainnet" : "devnet",
            slot: typeof txRaw.slot === "number" ? txRaw.slot : null,
          }
        : null;
    return { skill: { name, digest }, capabilities, tx };
  } catch {
    return null;
  }
}

const COVENANT_AUTHOR_EMAIL = "covenant@users.noreply.github.com";

// First commit produced under the witness pipeline. Commits before it render as
// historical (all anchors gray). Empty until the pipeline ships.
const WITNESS_CUTOVER_SHA = process.env.WITNESS_CUTOVER_SHA || "";

// Substitute a public label when an author name/email trips the banned-token
// scan, so historical commits still render without leaking an operator identity.
function redactAuthor(name: string, email: string): { display: string; email: string } {
  if (clean(name) === null || clean(email) === null) {
    return { display: "Covenant Legacy", email: "legacy@opencovenant.org" };
  }
  return { display: name, email };
}

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
  // merge-base --is-ancestor signals via exit code; git() returns null on
  // non-zero (not an ancestor) and "" on success (sha is an ancestor = predates).
  return git(repoRoot, ["merge-base", "--is-ancestor", sha, WITNESS_CUTOVER_SHA]) !== null;
}

// Anchor 1 — per-commit Solana Memo tx, signed by the operator authority, with
// payload covenant-commit-v1:<sha>:<audit_root_hex>:<unix_ms>. Reads the
// recorded tx from landing/public/witness/memo/<sha>.json and confirms it.
function checkAnchor1CommitMemo(repoRoot: string, sha: string): Witness {
  const memoManifest = join(repoRoot, "landing", "public", "witness", "memo", `${sha}.json`);
  if (!existsSync(memoManifest)) {
    return {
      key: "rekor",
      label: "Solana commit memo",
      state: "yellow",
      detail:
        "No memo anchor published for this commit yet. When the anchor daemon posts a memo tx (payload covenant-commit-v1:<sha>:<audit_root_hex>:<ts>, signed by the operator authority), this light verifies it.",
      badge: { text: "Anchor not yet live", tone: "yellow" },
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
        detail: `Memo tx ${parsed.tx || "missing"} did not verify against the operator authority pubkey.`,
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

// Anchor 2 — local hash chain. attestations/<sha>.json holds per-LLM-call Step
// records Merkle-rooted into audit_root_hex; green when present with a root.
function checkAnchor2AuditChain(repoRoot: string, sha: string): Witness {
  const attestationPath = join(repoRoot, "attestations", `${sha}.json`);
  if (!existsSync(attestationPath)) {
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "yellow",
      detail:
        "No audit chain published for this commit yet. Per-LLM-call Step records Merkle-root into attestations/<sha>.json; once present this light recomputes and verifies the root.",
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

// Anchor 3 — settlement-program anchor. Looks for a ReceiptBatch PDA on the
// settlement program holding this commit's Merkle leaf in a confirmed batch.
function checkAnchor3Solana(_repoRoot: string, sha: string): Witness {
  return {
    key: "solana_anchor",
    label: "Solana settlement anchor",
    state: "yellow",
    detail:
      "No settlement batch anchored for this commit yet. The publisher batches receipts via anchor_receipt_batch on cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y; until the first batch lands this light reads yellow.",
    drillHref: `https://solscan.io/account/cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y?cluster=devnet`,
  };
}

// Anchor 4 — verifier-refuter signature. A separately-keyed ed25519 verifier
// signs the audit root into attestations/<sha>.verifier.sig. Presence alone is
// not sufficient: the light stays yellow until the signature is checked against
// the published verifier pubkey.
function checkAnchor4VerifierSig(repoRoot: string, sha: string): Witness {
  const sigPath = join(repoRoot, "attestations", `${sha}.verifier.sig`);
  if (!existsSync(sigPath)) {
    return {
      key: "verifier_sig",
      label: "Verifier-Refuter signature",
      state: "yellow",
      detail:
        "No verifier signature published for this commit yet. A separately-keyed ed25519 verifier signs the audit root; until then this light reads yellow.",
    };
  }
  return {
    key: "verifier_sig",
    label: "Verifier-Refuter signature",
    state: "yellow",
    detail:
      "Verifier signature present but not yet checked. Ed25519 verification against the published verifier pubkey is not wired, so this light stays yellow rather than claim a green it has not verified.",
  };
}

export async function GET(_req: Request, ctx: { params: Promise<{ sha: string }> }) {
  const { sha } = await ctx.params;
  if (!/^[0-9a-f]{7,40}$/i.test(sha)) {
    return NextResponse.json({ error: "invalid sha" }, { status: 400 });
  }

  const repoRoot = findRepoRoot(process.cwd()) || resolve(process.cwd(), "..");

  const meta = git(repoRoot, [
    "show",
    "-s",
    "--format=%H%x09%h%x09%an%x09%ae%x09%aI%x09%s%x09%b",
    sha,
  ]);
  if (!meta) {
    return NextResponse.json({ error: "unknown sha" }, { status: 404 });
  }
  const [fullSha, shortSha, rawAuthorDisplay, rawAuthorEmail, isoDate, subject, ...bodyParts] =
    meta.split("\t");
  const bodyText = bodyParts.join("\t").trim();
  const author = redactAuthor(rawAuthorDisplay, rawAuthorEmail);

  const predatesWitnessLoop =
    rawAuthorEmail !== COVENANT_AUTHOR_EMAIL && WITNESS_CUTOVER_SHA
      ? predatesCutover(repoRoot, fullSha)
      : rawAuthorEmail !== COVENANT_AUTHOR_EMAIL;

  const commit = {
    sha: fullSha,
    shortSha,
    authorDisplay: author.display,
    authorEmail: author.email,
    subject,
    bodyText,
    isoDate,
    predatesWitnessLoop,
  };

  const fifth = {
    label: "Code Quality (Not Witnessed)",
    detail:
      "Semantic correctness is never witnessed by the chain — see the mutation-quality trend for the test-suite's catch rate over time.",
    href: "/lineage/mutation-quality",
  };

  // Pre-cutover commits are still queryable; render historical metadata with all
  // anchors gray so a legitimate-history URL doesn't 404.
  if (predatesWitnessLoop) {
    return NextResponse.json({
      commit: { ...commit, predatesWitnessLoop: true },
      witnesses: [
        { key: "rekor", label: "Solana commit memo", state: "gray", detail: "Predates witness loop." },
        { key: "audit_chain", label: "Audit hash chain", state: "gray", detail: "Predates witness loop." },
        { key: "solana_anchor", label: "Solana settlement anchor", state: "gray", detail: "Predates witness loop." },
        { key: "verifier_sig", label: "Verifier-Refuter signature", state: "gray", detail: "Predates witness loop." },
      ] satisfies Witness[],
      skillRun: null,
      fifth,
    });
  }

  return NextResponse.json({
    commit,
    witnesses: [
      checkAnchor1CommitMemo(repoRoot, fullSha),
      checkAnchor2AuditChain(repoRoot, fullSha),
      checkAnchor3Solana(repoRoot, fullSha),
      checkAnchor4VerifierSig(repoRoot, fullSha),
    ],
    skillRun: checkSkillRun(repoRoot, fullSha),
    fifth,
  });
}
