// /api/verify/[sha] — server-side witness check for a Covenant-author commit.
// Resolves commit metadata, then reports four independent witnesses (commit
// memo, audit hash chain, settlement anchor, verifier signature). Each anchor
// reads yellow until its artifact is published — a witness is never green
// before it has actually been checked.

import { execFileSync } from "node:child_process";
import { createPublicKey, verify as edVerify } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot } from "@/lib/agentStream.mjs";
import { recomputeAuditRoot } from "@/lib/audit/auditRoot";
import { redactAuthor } from "@/lib/verify/author";
import { checkSkillRun } from "@/lib/verify/skillRun";

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

// First commit produced under the witness pipeline. Commits before it render as
// historical (all anchors gray). Empty until the pipeline ships.
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
// recomputeAuditRoot (lib/audit/auditRoot.ts) replays the raw event lines the
// same way the daemon and standalone verifier do, so this is a real independent
// check of the published root, not a trust.
function checkAnchor2AuditChain(repoRoot: string, sha: string): Witness {
  const attestationPath = join(repoRoot, "attestations", `${sha}.json`);
  if (!existsSync(attestationPath)) {
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "yellow",
      detail:
        "No audit chain published for this commit yet. A run's hash-chained events recompute into attestations/<sha>.json; once present this light recomputes the root and checks it.",
    };
  }
  try {
    const att = JSON.parse(readFileSync(attestationPath, "utf8")) as {
      audit_root_hex?: string;
      steps?: unknown;
    };
    const steps = Array.isArray(att.steps) ? att.steps : [];
    if (!att.audit_root_hex || !steps.length || !steps.every((s) => typeof s === "string")) {
      return {
        key: "audit_chain",
        label: "Audit hash chain",
        state: "red",
        detail: "Attestation present but missing a root or its canonical steps.",
      };
    }
    const recomputed = recomputeAuditRoot(steps as string[]);
    if (recomputed !== att.audit_root_hex) {
      return {
        key: "audit_chain",
        label: "Audit hash chain",
        state: "red",
        detail: `Chain tampered: recomputed root ${recomputed.slice(0, 12)}… does not match the published ${att.audit_root_hex.slice(0, 12)}….`,
      };
    }
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "green",
      detail: `Root ${att.audit_root_hex.slice(0, 16)}… recomputed from ${steps.length} hash-chained events and matches.`,
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
function checkAnchor3Solana(repoRoot: string, sha: string): Witness {
  const manifestPath = join(repoRoot, "landing", "public", "witness", "settlement", `${sha}.json`);
  if (!existsSync(manifestPath)) {
    return {
      key: "solana_anchor",
      label: "Solana settlement anchor",
      state: "yellow",
      detail:
        "No settlement batch anchored for this commit yet. A receipt batch on cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y commits the audit root on-chain; until it lands this light reads yellow.",
      drillHref: `https://solscan.io/account/cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y?cluster=devnet`,
    };
  }
  try {
    const m = JSON.parse(readFileSync(manifestPath, "utf8")) as {
      tx?: string;
      batch_pda?: string;
      merkle_root?: string;
      slot?: number;
      cluster?: string;
    };
    const attPath = join(repoRoot, "attestations", `${sha}.json`);
    const auditRoot = existsSync(attPath)
      ? (JSON.parse(readFileSync(attPath, "utf8")) as { audit_root_hex?: string }).audit_root_hex
      : undefined;
    const cluster = m.cluster ?? "devnet";
    const drillHref = m.batch_pda
      ? `https://solscan.io/account/${m.batch_pda}?cluster=${cluster}`
      : undefined;
    if (!m.batch_pda || !m.tx || !m.merkle_root || m.merkle_root !== auditRoot) {
      return {
        key: "solana_anchor",
        label: "Solana settlement anchor",
        state: "red",
        detail: "Settlement batch present but its committed root does not match the run's audit root.",
        drillHref,
      };
    }
    return {
      key: "solana_anchor",
      label: "Solana settlement anchor",
      state: "green",
      detail: `Receipt batch ${m.batch_pda.slice(0, 12)}… on devnet commits audit root ${m.merkle_root.slice(0, 12)}… at slot ${m.slot ?? "?"}. Decode the PDA on-chain to check.`,
      drillHref,
    };
  } catch {
    return {
      key: "solana_anchor",
      label: "Solana settlement anchor",
      state: "red",
      detail: "Settlement manifest unreadable.",
    };
  }
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
  try {
    const att = JSON.parse(readFileSync(join(repoRoot, "attestations", `${sha}.json`), "utf8")) as {
      audit_root_hex?: string;
      verdict?: string;
      domain?: string;
    };
    const pubkeyPath = join(repoRoot, "landing", "public", "witness", "verifier-pubkey.txt");
    if (!att.audit_root_hex || !existsSync(pubkeyPath)) {
      return {
        key: "verifier_sig",
        label: "Verifier-Refuter signature",
        state: "yellow",
        detail: "Verifier signature present but the published pubkey or audit root is missing.",
      };
    }
    const pubkey = readFileSync(pubkeyPath, "utf8").trim();
    const sig = readFileSync(sigPath, "utf8").trim();
    const domain = att.domain || "covenant.witness.v1";
    const message = Buffer.from(`${domain}\n${att.audit_root_hex}`, "utf8");
    const key = createPublicKey({ format: "jwk", key: { kty: "OKP", crv: "Ed25519", x: pubkey } });
    if (!edVerify(null, message, key, Buffer.from(sig, "base64url"))) {
      return {
        key: "verifier_sig",
        label: "Verifier-Refuter signature",
        state: "red",
        detail: "Verifier signature did not verify against the published verifier pubkey.",
      };
    }
    if (att.verdict === "refute") {
      return {
        key: "verifier_sig",
        label: "Verifier-Refuter signature",
        state: "red",
        detail: `Verifier refuted this run (signed by ${pubkey.slice(0, 12)}…): a signed action causally followed untrusted on-chain input.`,
      };
    }
    return {
      key: "verifier_sig",
      label: "Verifier-Refuter signature",
      state: "green",
      detail: `Audit root signed by an independent verifier (${pubkey.slice(0, 12)}…), no refutation. Check it yourself against landing/public/witness/verifier-pubkey.txt.`,
    };
  } catch {
    return {
      key: "verifier_sig",
      label: "Verifier-Refuter signature",
      state: "red",
      detail: "Verifier signature or pubkey unreadable.",
    };
  }
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

  let commit: {
    sha: string;
    shortSha: string;
    authorDisplay: string;
    authorEmail: string;
    subject: string;
    bodyText: string;
    isoDate: string;
    predatesWitnessLoop: boolean;
  };
  let predatesWitnessLoop: boolean;

  if (meta) {
    const [fullSha, shortSha, rawAuthorDisplay, rawAuthorEmail, isoDate, subject, ...bodyParts] =
      meta.split("\t");
    const bodyText = bodyParts.join("\t").trim();
    const author = redactAuthor(rawAuthorDisplay, rawAuthorEmail);
    predatesWitnessLoop =
      rawAuthorEmail !== COVENANT_AUTHOR_EMAIL && WITNESS_CUTOVER_SHA
        ? predatesCutover(repoRoot, fullSha)
        : rawAuthorEmail !== COVENANT_AUTHOR_EMAIL;
    commit = {
      sha: fullSha,
      shortSha,
      authorDisplay: author.display,
      authorEmail: author.email,
      subject,
      bodyText,
      isoDate,
      predatesWitnessLoop,
    };
  } else {
    // Git history is unavailable (a shallow deploy may carry only the HEAD
    // commit). Verification reads committed files, not git, so still render any
    // sha that has witness artifacts; just skip the git-only commit header.
    if (!existsSync(join(repoRoot, "attestations", `${sha}.json`))) {
      return NextResponse.json({ error: "unknown sha" }, { status: 404 });
    }
    predatesWitnessLoop = false;
    commit = {
      sha,
      shortSha: sha.slice(0, 12),
      authorDisplay: "Covenant",
      authorEmail: COVENANT_AUTHOR_EMAIL,
      subject: "Witnessed devnet run",
      bodyText: "",
      isoDate: "",
      predatesWitnessLoop: false,
    };
  }

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
      checkAnchor1CommitMemo(repoRoot, commit.sha),
      checkAnchor2AuditChain(repoRoot, commit.sha),
      checkAnchor3Solana(repoRoot, commit.sha),
      checkAnchor4VerifierSig(repoRoot, commit.sha),
    ],
    skillRun: checkSkillRun(repoRoot, commit.sha),
    fifth,
  });
}
