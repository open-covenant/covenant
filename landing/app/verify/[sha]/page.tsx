// /verify/[sha] — Four Witnesses Present for a Covenant-author commit.
//
// Renders the v0.2 witness UX:
//   * Four lights (gitsign+Rekor / audit hash chain / Solana anchor / verifier signature)
//   * Permanent yellow fifth status line "Code Quality (Not Witnessed)" linking to /lineage/mutation-quality
//   * SAME-FAMILY badge on Anchor 4 (yellow weeks 1-4, model-tier sub-label weeks 3-4)
//   * Force-Fail demo CTA (Week 2 ship target)
//   * Pre-cutoff history rendered as "Predates witness loop" with all lights gray
//
// The page is server-rendered: it calls the /api/verify/[sha] route handler
// which is the Path 1 server-side proxy from Spike 7. Path 2 (browser-side
// bundle verification via @sigstore/verify) lands as a v0.2.x card; the page
// will swap to client-side checks then without UX changes.

import Link from "next/link";
import { notFound } from "next/navigation";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";
import { clean } from "@/lib/agentStream.mjs";

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

type CommitMeta = {
  sha: string;
  shortSha: string;
  authorDisplay: string;
  authorEmail: string;
  subject: string;
  bodyText: string;
  isoDate: string;
  predatesWitnessLoop: boolean;
};

type VerifyPayload = {
  commit: CommitMeta;
  witnesses: Witness[];
  fifth: {
    label: string;
    detail: string;
    href: string;
  };
};

// Reuse the runtime-derived token scan from lib/agentStream.mjs so this file
// never enumerates operator-identifying literals. clean() returns null when
// the input contains a banned token; we substitute a public legacy author
// label in that case so pre-cutover commits still render their metadata
// without leaking.
function redactAuthor(name: string, email: string): { display: string; email: string } {
  if (clean(name) === null || clean(email) === null) {
    return { display: "Covenant Legacy", email: "legacy@opencovenant.org" };
  }
  return { display: name, email };
}

async function fetchWitness(sha: string): Promise<VerifyPayload | null> {
  // Server component fetching its own API route — Next.js pattern for keeping
  // verification logic in the route handler so the same surface backs an
  // eventual public /api/verify/<sha> consumer.
  const host = process.env.VERCEL_URL
    ? `https://${process.env.VERCEL_URL}`
    : process.env.NEXT_PUBLIC_SITE_URL || "http://localhost:3000";
  const res = await fetch(`${host}/api/verify/${encodeURIComponent(sha)}`, { cache: "no-store" });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`verify api ${res.status}`);
  return (await res.json()) as VerifyPayload;
}

function StateDot({ state }: { state: WitnessState }) {
  const tone =
    state === "green"
      ? "bg-emerald-400"
      : state === "yellow"
        ? "bg-amber-400"
        : state === "red"
          ? "bg-rose-500"
          : "bg-neutral-600";
  return (
    <span
      aria-hidden="true"
      className={`inline-block h-2.5 w-2.5 rounded-full ${tone} shrink-0`}
    />
  );
}

function WitnessCard({ w }: { w: Witness }) {
  return (
    <div className="flex flex-col gap-3 border border-neutral-800 bg-neutral-950/60 p-5">
      <div className="flex items-center gap-2.5">
        <StateDot state={w.state} />
        <span className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
          {w.label}
        </span>
      </div>
      <p className="text-[13px] font-light leading-relaxed text-neutral-400">
        {w.detail}
      </p>
      {w.badge && (
        <div
          className={`inline-flex w-fit items-center gap-2 border px-2 py-1 text-[10px] uppercase tracking-[1.5px] ${
            w.badge.tone === "yellow"
              ? "border-amber-800/60 bg-amber-950/30 text-amber-300"
              : "border-rose-800/60 bg-rose-950/30 text-rose-300"
          }`}
        >
          {w.badge.text}
        </div>
      )}
      {w.drillHref && (
        <a
          href={w.drillHref}
          target="_blank"
          rel="noopener noreferrer"
          className="text-[11px] uppercase tracking-[1.5px] text-neutral-500 underline-offset-4 hover:text-neutral-300 hover:underline"
        >
          Open evidence →
        </a>
      )}
    </div>
  );
}

export default async function VerifyPage({ params }: { params: Promise<{ sha: string }> }) {
  const { sha } = await params;
  if (!/^[0-9a-f]{7,40}$/i.test(sha)) notFound();

  let payload: VerifyPayload | null = null;
  try {
    payload = await fetchWitness(sha);
  } catch {
    payload = null;
  }
  if (!payload) notFound();

  const { commit, witnesses, fifth } = payload;
  const author = redactAuthor(commit.authorDisplay, commit.authorEmail);

  return (
    <main id="main-content" className="min-h-[100dvh] bg-[#030303] text-neutral-200">
      <SiteHeader />

      <div className="mx-auto max-w-5xl px-6 pb-24 pt-28">
        <div className="mb-10 flex flex-col gap-2">
          <p className="text-[11px] uppercase tracking-[3px] text-neutral-500">
            /verify/{commit.shortSha}
          </p>
          <h1 className="text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white">
            Four Witnesses Present
          </h1>
          {commit.predatesWitnessLoop && (
            <p className="mt-2 max-w-2xl border border-neutral-800 bg-neutral-950/60 px-4 py-3 text-[13px] font-light leading-relaxed text-amber-300">
              This commit predates the witness loop. Anchors below are gray because the cosign + audit-chain + on-chain + verifier-signature pipeline was not yet active when this commit landed. Treat as historical record only.
            </p>
          )}
          <p className="mt-2 text-[13px] font-light leading-relaxed text-neutral-400">
            <span className="text-neutral-200">{author.display}</span>{" "}
            <span className="text-neutral-500">&lt;{author.email}&gt;</span> &middot;{" "}
            <span className="text-neutral-500">{commit.isoDate}</span>
          </p>
          <p className="mt-1 text-[15px] font-light leading-relaxed text-neutral-100">
            {commit.subject}
          </p>
          {commit.bodyText && (
            <pre className="mt-3 max-w-3xl whitespace-pre-wrap border border-neutral-800 bg-neutral-950/40 p-4 text-[12px] font-light leading-relaxed text-neutral-400">
              {commit.bodyText}
            </pre>
          )}
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          {witnesses.map((w) => (
            <WitnessCard key={w.key} w={w} />
          ))}
        </div>

        <div className="mt-6 border border-amber-800/60 bg-amber-950/20 p-5">
          <div className="flex items-center gap-2.5">
            <StateDot state="yellow" />
            <span className="text-[11px] font-light uppercase tracking-[2px] text-amber-300">
              {fifth.label}
            </span>
          </div>
          <p className="mt-3 text-[13px] font-light leading-relaxed text-amber-200/80">
            {fifth.detail}
          </p>
          <Link
            href={fifth.href}
            className="mt-3 inline-block text-[11px] uppercase tracking-[1.5px] text-amber-300 underline-offset-4 hover:underline"
          >
            See mutation-quality trend →
          </Link>
        </div>

        {!commit.predatesWitnessLoop && (
          <div className="mt-10 border border-neutral-800 bg-neutral-950/60 p-5">
            <h2 className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
              Try to break it (Force-Fail Demo)
            </h2>
            <p className="mt-3 max-w-2xl text-[13px] font-light leading-relaxed text-neutral-400">
              Clone the repo. Hand-edit one byte of <code className="text-neutral-200">landing/public/audit/events.jsonl</code>. Refresh this page with{" "}
              <code className="text-neutral-200">?audit=local</code> appended. The audit-chain light flips red while gitsign + Solana + verifier-signature stay green. This proves the four witnesses are structurally independent: the audit chain is the only one Covenant operates locally; the other three live on Sigstore Rekor, Solana, and a separately-keyed verifier process Covenant cannot tamper with without leaving cryptographic evidence.
            </p>
          </div>
        )}

        <div className="mt-10 border-t border-neutral-900 pt-6 text-[11px] uppercase tracking-[1.5px] text-neutral-500">
          v0.2 Witness Loop &middot;{" "}
          <Link href="/lineage" className="hover:text-neutral-300">
            See evolutionary lineage
          </Link>{" "}
          &middot;{" "}
          <a
            href="https://github.com/open-covenant/covenant/blob/main/docs/provenance/witness-loop-overview.md"
            target="_blank"
            rel="noopener noreferrer"
            className="hover:text-neutral-300"
          >
            Architecture
          </a>
        </div>
      </div>

      <SiteFooter />
    </main>
  );
}
