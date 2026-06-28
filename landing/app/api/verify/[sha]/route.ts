// /api/verify/[sha] — server-side witness check for a Covenant-author commit.
// Resolves commit metadata, then reports four independent witnesses (commit
// memo, audit hash chain, settlement anchor, verifier signature). Each anchor
// reads yellow until its artifact is published — a witness is never green
// before it has actually been checked.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot } from "@/lib/agentStream.mjs";
import { redactAuthor } from "@/lib/verify/author";
import { checkAnchor2AuditChain } from "@/lib/verify/auditChain";
import { checkAnchor1CommitMemo } from "@/lib/verify/commitMemo";
import { checkSkillRun } from "@/lib/verify/skillRun";
import type { Witness } from "@/lib/verify/types";
import { checkAnchor4VerifierSig } from "@/lib/verify/verifierSig";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

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
