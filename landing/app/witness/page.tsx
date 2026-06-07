// /witness — feed of skill runs and the verifier's signed verdicts, read from
// landing/public/witness/skill/<sha>.json. A run is in-flight until the
// separately-keyed verifier signs a verdict (pass/refute); an unsigned verdict
// renders as a warning, never as a decision.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Metadata } from "next";
import Link from "next/link";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export const metadata: Metadata = {
  title: "Witness Feed: Signed Verdicts on Solana Skill Runs",
  description:
    "Live feed of Covenant skill runs and the separately-keyed Verifier's signed pass/refute verdicts. An unsigned verdict is never a decision. Devnet-first.",
  alternates: { canonical: "/witness" },
};

type RunState = "in_flight" | "verified" | "refuted" | "unsigned";

type Verdict = {
  decision: "pass" | "refute";
  verifierPubkey: string;
  signature: string | null;
  ts: string;
};

type FeedEntry = {
  sha: string;
  skill: { name: string; digest: string };
  verdict: Verdict | null;
  state: RunState;
};

function deriveState(verdict: Verdict | null): RunState {
  if (!verdict) return "in_flight";
  if (!verdict.signature) return "unsigned";
  return verdict.decision === "refute" ? "refuted" : "verified";
}

function coerce(sha: string, raw: Record<string, unknown>): FeedEntry | null {
  const skill = (raw.skill ?? {}) as Record<string, unknown>;
  const name = typeof skill.name === "string" ? skill.name : "";
  const digest = typeof skill.digest === "string" ? skill.digest : "";
  if (!name || !digest) return null;

  let verdict: Verdict | null = null;
  const v = raw.verdict as Record<string, unknown> | undefined;
  if (v && (v.decision === "pass" || v.decision === "refute")) {
    verdict = {
      decision: v.decision,
      verifierPubkey: typeof v.verifier_pubkey === "string" ? v.verifier_pubkey : "",
      signature: typeof v.signature === "string" && v.signature ? v.signature : null,
      ts: typeof v.ts === "string" ? v.ts : "",
    };
  }
  return { sha, skill: { name, digest }, verdict, state: deriveState(verdict) };
}

const STATE_WEIGHT: Record<RunState, number> = {
  in_flight: 0,
  unsigned: 1,
  refuted: 2,
  verified: 3,
};

function readFeed(): FeedEntry[] {
  const dir = join(process.cwd(), "public", "witness", "skill");
  let names: string[];
  try {
    names = readdirSync(dir);
  } catch {
    return [];
  }
  const out: FeedEntry[] = [];
  for (const name of names) {
    const m = name.match(/^([0-9a-f]{7,64})\.json$/i);
    if (!m) continue;
    try {
      const entry = coerce(m[1], JSON.parse(readFileSync(join(dir, name), "utf8")));
      if (entry) out.push(entry);
    } catch {
      // skip malformed manifest
    }
  }
  return out.sort((a, b) => {
    if (STATE_WEIGHT[a.state] !== STATE_WEIGHT[b.state]) {
      return STATE_WEIGHT[a.state] - STATE_WEIGHT[b.state];
    }
    const at = a.verdict?.ts ?? "";
    const bt = b.verdict?.ts ?? "";
    if (at !== bt) return at < bt ? 1 : -1;
    return a.sha < b.sha ? 1 : -1;
  });
}

function readVerifierPubkey(): string | null {
  try {
    const key = readFileSync(join(process.cwd(), "public", "witness", "verifier-pubkey.txt"), "utf8").trim();
    return key || null;
  } catch {
    return null;
  }
}

const STATE_META: Record<RunState, { dot: string; pulse: boolean; label: string }> = {
  in_flight: { dot: "bg-amber-400", pulse: true, label: "In flight" },
  unsigned: { dot: "bg-amber-400", pulse: false, label: "Unsigned verdict" },
  refuted: { dot: "bg-rose-500", pulse: false, label: "Refuted" },
  verified: { dot: "bg-emerald-400", pulse: false, label: "Verified" },
};

function Circle({ state }: { state: RunState }) {
  const meta = STATE_META[state];
  return (
    <span className="relative inline-flex h-3 w-3 shrink-0">
      {meta.pulse && (
        <span className={`absolute inline-flex h-full w-full animate-ping rounded-full ${meta.dot} opacity-60`} />
      )}
      <span className={`relative inline-flex h-3 w-3 rounded-full ${meta.dot}`} />
    </span>
  );
}

function VerdictLine({ entry }: { entry: FeedEntry }) {
  const { state, verdict } = entry;
  if (state === "in_flight") {
    return <span className="text-amber-300/80">awaiting Verifier verdict</span>;
  }
  if (state === "unsigned") {
    return (
      <span className="text-amber-300/80">
        verdict <span className="font-mono">{verdict?.decision}</span> present but unsigned — not a
        decision
      </span>
    );
  }
  const who = verdict?.verifierPubkey ? `${verdict.verifierPubkey.slice(0, 8)}…` : "Verifier";
  return (
    <span className={state === "refuted" ? "text-rose-300/90" : "text-emerald-300/90"}>
      {verdict?.decision === "refute" ? "refuted" : "passed"} · signed by{" "}
      <span className="font-mono">{who}</span>
      {verdict?.signature && (
        <span className="text-neutral-500"> · sig {verdict.signature.slice(0, 10)}…</span>
      )}
    </span>
  );
}

export default function WitnessPage() {
  const feed = readFeed();
  const verifierPubkey = readVerifierPubkey();

  return (
    <main id="main-content" className="min-h-[100dvh] bg-[#030303] text-neutral-200">
      <SiteHeader />

      <div className="mx-auto max-w-5xl px-6 pb-24 pt-28">
        <div className="mb-10 flex flex-col gap-2">
          <p className="text-[11px] uppercase tracking-[3px] text-neutral-500">/witness</p>
          <h1 className="text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white">
            Witness Feed
          </h1>
          <p className="mt-2 max-w-2xl text-[13px] font-light leading-relaxed text-neutral-400">
            Every skill run is judged by a <span className="text-neutral-200">separately-keyed
            Verifier</span> that Covenant cannot sign for. A run stays in flight until the Verifier
            signs a pass or refute verdict over the run&apos;s audit-root; an unsigned verdict is
            never treated as a decision. Devnet-first.
          </p>
          <p className="mt-1 text-[12px] font-light text-neutral-500">
            Verifier key:{" "}
            {verifierPubkey ? (
              <span className="font-mono text-neutral-300">{verifierPubkey.slice(0, 16)}…</span>
            ) : (
              <span className="text-amber-300/80">pending — ships with the Week-2 verifier persona</span>
            )}
          </p>
        </div>

        <div className="mb-6 flex flex-wrap gap-x-6 gap-y-2 text-[11px] uppercase tracking-[1.5px] text-neutral-500">
          {(["in_flight", "verified", "refuted", "unsigned"] as RunState[]).map((s) => (
            <span key={s} className="inline-flex items-center gap-2">
              <Circle state={s} />
              {STATE_META[s].label}
            </span>
          ))}
        </div>

        {!feed.length ? (
          <div className="border border-amber-800/60 bg-amber-950/20 p-5">
            <p className="text-[13px] font-light leading-relaxed text-amber-200/80">
              No skill runs witnessed yet. Each run writes{" "}
              <code className="text-amber-100">landing/public/witness/skill/&lt;sha&gt;.json</code>;
              the Verifier persona signs its verdict and the run appears here. Until the first signed
              run lands, this feed is empty.
            </p>
          </div>
        ) : (
          <ul className="flex flex-col divide-y divide-neutral-900 border border-neutral-800 bg-neutral-950/60">
            {feed.map((entry) => (
              <li key={entry.sha} className="flex items-start gap-4 p-5">
                <span className="mt-1.5">
                  <Circle state={entry.state} />
                </span>
                <div className="flex min-w-0 flex-col gap-1">
                  <div className="flex flex-wrap items-baseline gap-x-3">
                    <span className="font-mono text-[14px] text-white">{entry.skill.name}</span>
                    <Link
                      href={`/verify/${entry.sha}`}
                      className="font-mono text-[11px] text-neutral-500 underline-offset-4 hover:text-neutral-300 hover:underline"
                    >
                      {entry.sha.slice(0, 12)}
                    </Link>
                  </div>
                  <span className="break-all font-mono text-[11px] text-neutral-500">
                    {entry.skill.digest}
                  </span>
                  <span className="text-[12px] font-light">
                    <VerdictLine entry={entry} />
                  </span>
                </div>
              </li>
            ))}
          </ul>
        )}

        <div className="mt-10 border-t border-neutral-900 pt-6 text-[11px] uppercase tracking-[1.5px] text-neutral-500">
          Separately-keyed Verifier &middot; Devnet-first &middot;{" "}
          <Link href="/skills" className="hover:text-neutral-300">
            Skill catalog
          </Link>
        </div>
      </div>

      <SiteFooter />
    </main>
  );
}
