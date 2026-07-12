// /verify/[sha] — four-witness state for a Covenant commit, plus the skill-run
// panel when the commit has an associated skill run. Verification runs in the
// /api/verify/[sha] route handler this page reads.

import Link from "next/link";
import { headers } from "next/headers";
import { notFound } from "next/navigation";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";
import { redactAuthor } from "@/lib/verify/author";

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

type SkillRunTx = { sig: string; cluster: "devnet" | "mainnet"; slot: number | null };
type SkillRun = {
  skill: { name: string; digest: string };
  capabilities: string[];
  tx: SkillRunTx | null;
};

type VerifyPayload = {
  commit: CommitMeta;
  witnesses: Witness[];
  skillRun: SkillRun | null;
  fifth: {
    label: string;
    detail: string;
    href: string;
  };
};

async function fetchWitness(sha: string): Promise<VerifyPayload | null> {
  // Resolve the API on the host this request arrived on — the app is reachable
  // there whatever the port or deploy config, which avoids a wrong-port
  // self-fetch (e.g. localhost:3000) on hosts that aren't Vercel.
  const h = await headers();
  const reqHost = h.get("x-forwarded-host") || h.get("host");
  const proto = h.get("x-forwarded-proto") || "https";
  const base = reqHost
    ? `${proto}://${reqHost}`
    : process.env.VERCEL_URL
      ? `https://${process.env.VERCEL_URL}`
      : process.env.NEXT_PUBLIC_SITE_URL || "http://localhost:3000";
  const res = await fetch(`${base}/api/verify/${encodeURIComponent(sha)}`, { cache: "no-store" });
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

function SkillRunPanel({ run }: { run: SkillRun }) {
  const solscan = run.tx
    ? `https://solscan.io/tx/${run.tx.sig}${run.tx.cluster === "devnet" ? "?cluster=devnet" : ""}`
    : null;
  return (
    <div className="mb-6 border border-neutral-800 bg-neutral-950/60 p-5">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h2 className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
          Skill run
        </h2>
        <span className="font-mono text-[13px] text-white">{run.skill.name}</span>
      </div>

      <dl className="mt-4 grid gap-4 sm:grid-cols-2">
        <div className="sm:col-span-2">
          <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">Skill digest</dt>
          <dd className="mt-1.5 break-all font-mono text-[12px] text-emerald-300">
            {run.skill.digest}
          </dd>
        </div>

        <div className="sm:col-span-2">
          <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">
            Capabilities exercised
          </dt>
          <dd className="mt-1.5">
            {run.capabilities.length ? (
              <div className="flex flex-wrap gap-1.5">
                {run.capabilities.map((c) => (
                  <span
                    key={c}
                    className="border border-neutral-800 bg-neutral-900/60 px-2 py-0.5 font-mono text-[11px] text-neutral-300"
                  >
                    {c}
                  </span>
                ))}
              </div>
            ) : (
              <span className="text-[12px] text-neutral-600">none recorded</span>
            )}
          </dd>
        </div>

        <div className="sm:col-span-2">
          <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">
            On-chain transaction
          </dt>
          <dd className="mt-1.5 text-[12px]">
            {solscan && run.tx ? (
              <a
                href={solscan}
                target="_blank"
                rel="noopener noreferrer"
                className="font-mono text-neutral-300 underline-offset-4 hover:text-neutral-100 hover:underline"
              >
                {run.tx.sig.slice(0, 16)}… ({run.tx.cluster})
              </a>
            ) : (
              <span className="text-neutral-600">
                pending — no on-chain transaction anchored for this run
              </span>
            )}
          </dd>
        </div>
      </dl>

      <p className="mt-4 text-[12px] font-light leading-relaxed text-neutral-500">
        When the anchors below land, the same witnesses that attest the commit also bind this
        skill&apos;s content digest and signed actions — not a separate trust path.
      </p>
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

  const { commit, witnesses, skillRun, fifth } = payload;
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
              This commit predates the witness loop. Anchors below are gray because the commit-memo + audit-chain + on-chain + verifier-signature pipeline was not yet active when this commit landed. Treat as historical record only.
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

        {skillRun && <SkillRunPanel run={skillRun} />}

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
              Witness independence
            </h2>
            <p className="mt-3 max-w-2xl text-[13px] font-light leading-relaxed text-neutral-400">
              The audit hash chain is the only witness Covenant operates locally. The other three
              are external — a Solana commit memo, the on-chain settlement anchor, and a
              separately-keyed verifier — so tampering with the local chain cannot forge them.
              Each anchor is checked on its own evidence, independently of the others.
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
