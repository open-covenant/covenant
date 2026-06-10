import type { Metadata } from "next";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { GITHUB_URL } from "../_brand";

const DESCRIPTION =
  "Claude and Grok compete to optimize Covenant's own code. A frozen benchmark neither can touch scores every proposal; the best one ships. Live scoreboard, every verdict public.";

export const metadata: Metadata = {
  title: "Arena: Covenant",
  description: DESCRIPTION,
  alternates: { canonical: "/arena" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/arena",
    title: "Covenant arena",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant arena",
    description: DESCRIPTION,
    images: "/twitter-image.jpg",
  },
};

export const revalidate = 60;

type Entry = {
  proposer: string;
  model: string;
  scalar: number;
  incumbent: number;
  gain: number;
  promoted: boolean;
  commit: string | null;
  reason: string | null;
};

type Arena = {
  updatedAt: string;
  baselineFuel: number;
  incumbent: { scalar: number; fuelCutPct: number };
  tally: { Claude: number; Grok: number; rejectedRounds: number };
  curve: { round: number; scalar: number }[];
  rounds: { round: number; entries: Entry[] }[];
};

const RAW_URL =
  "https://raw.githubusercontent.com/open-covenant/covenant/feat/self-improvement/landing/public/arena.json";

async function loadArena(): Promise<Arena> {
  try {
    const res = await fetch(RAW_URL, { next: { revalidate: 60 } });
    if (res.ok) return (await res.json()) as Arena;
  } catch {}
  return JSON.parse(
    readFileSync(join(process.cwd(), "public", "arena.json"), "utf8"),
  ) as Arena;
}

function Curve({ points }: { points: { round: number; scalar: number }[] }) {
  const w = 640;
  const h = 180;
  const pad = 8;
  const max = Math.max(...points.map((p) => p.scalar)) * 1.08;
  const step = points.length > 1 ? (w - pad * 2) / (points.length - 1) : 0;
  const xy = points.map((p, i) => [
    pad + i * step,
    h - pad - ((p.scalar - 1) / (max - 1)) * (h - pad * 2),
  ]);
  const path = xy.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className="mt-6 w-full max-w-2xl" aria-label="Efficiency multiple per promoted round">
      <path d={path} fill="none" stroke="#e5e5e5" strokeWidth="1.5" />
      {xy.map(([x, y], i) => (
        <circle key={i} cx={x} cy={y} r="3" fill="#030303" stroke="#e5e5e5" strokeWidth="1.5" />
      ))}
    </svg>
  );
}

export default async function ArenaPage() {
  const arena = await loadArena();
  const stat =
    "border border-neutral-800/80 px-6 py-5 text-center";
  const statLabel =
    "text-[10px] uppercase tracking-[0.3em] text-neutral-500";
  const statValue = "mt-2 text-3xl font-extralight text-white";

  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-10 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-14">
          Arena
        </h1>

        <h2 className="max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          Claude vs Grok, judged by a machine neither can touch
        </h2>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300">
          Every round, both models propose a rewrite of the engine that
          verifies Covenant&apos;s tamper-evident audit log. A frozen benchmark
          measures each proposal&apos;s exact instruction cost; held-out test
          suites require bit-identical behavior. The best proposal ships and
          becomes the next incumbent. Rejections are listed next to wins.
        </p>

        <div className="mt-12 grid max-w-2xl grid-cols-2 gap-3 sm:grid-cols-4">
          <div className={stat}>
            <div className={statLabel}>Claude wins</div>
            <div className={statValue}>{arena.tally.Claude}</div>
          </div>
          <div className={stat}>
            <div className={statLabel}>Grok wins</div>
            <div className={statValue}>{arena.tally.Grok}</div>
          </div>
          <div className={stat}>
            <div className={statLabel}>Rejected rounds</div>
            <div className={statValue}>{arena.tally.rejectedRounds}</div>
          </div>
          <div className={stat}>
            <div className={statLabel}>Cost cut</div>
            <div className={statValue}>{arena.incumbent.fuelCutPct}%</div>
          </div>
        </div>

        <p className="mt-10 text-[11px] uppercase tracking-[0.25em] text-neutral-500">
          Efficiency multiple, round by round (now {arena.incumbent.scalar}x)
        </p>
        <Curve points={arena.curve} />

        <ol className="relative mt-16 sm:mt-20">
          {arena.rounds.map((r, i) => {
            const isLast = i === arena.rounds.length - 1;
            const winner = r.entries.find((e) => e.promoted);
            return (
              <li
                key={r.round}
                className={`relative border-l border-neutral-800/80 pl-8 sm:pl-12 ${isLast ? "" : "pb-12 sm:pb-14"}`}
              >
                <span
                  aria-hidden="true"
                  className={`absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full ${winner ? "bg-neutral-100" : "bg-neutral-700"}`}
                />
                <div className="mb-3 flex flex-wrap items-baseline gap-x-4 gap-y-1">
                  <span className="text-[11px] uppercase tracking-[0.3em] text-neutral-300">
                    Round {r.round}
                  </span>
                  <span className="text-[11px] uppercase tracking-[0.25em] text-neutral-500">
                    {winner ? `${winner.proposer} promoted, ${winner.scalar}x` : "no promotion"}
                  </span>
                </div>
                <ul className="space-y-2">
                  {r.entries.map((e, j) => (
                    <li key={j} className="flex flex-wrap items-baseline gap-x-3 text-sm font-light text-neutral-300">
                      <span className="w-14 text-neutral-100">{e.proposer}</span>
                      <span className="tabular-nums">{e.scalar > 0 ? `${e.scalar}x` : "—"}</span>
                      {e.promoted && e.commit ? (
                        <a
                          href={`${GITHUB_URL}/commit/${e.commit}`}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="text-[11px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
                        >
                          shipped {e.commit.slice(0, 8)}
                        </a>
                      ) : (
                        <span className="text-[12px] text-neutral-500">{e.reason}</span>
                      )}
                    </li>
                  ))}
                </ul>
              </li>
            );
          })}
        </ol>

        <p className="mt-14 text-[11px] uppercase tracking-[0.25em] text-neutral-500">
          Updated {new Date(arena.updatedAt).toUTCString()}
        </p>
      </div>

      <SiteFooter />
    </main>
  );
}
