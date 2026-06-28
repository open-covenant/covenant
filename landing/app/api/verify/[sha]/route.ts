// /api/verify/[sha] — server-side witness check for a Covenant-author commit.
// Resolves commit metadata, then reports four independent witnesses (commit
// memo, audit hash chain, settlement anchor, verifier signature). Each anchor
// reads yellow until its artifact is published — a witness is never green
// before it has actually been checked.

import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot } from "@/lib/agentStream.mjs";
import { redactAuthor } from "@/lib/verify/author";
import { checkAnchor2AuditChain } from "@/lib/verify/auditChain";
import { checkAnchor1CommitMemo } from "@/lib/verify/commitMemo";
import { checkAnchor3Solana } from "@/lib/verify/settlement";
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
