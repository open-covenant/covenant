import type { Metadata } from "next";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { GITHUB_URL } from "../_brand";

const DESCRIPTION =
  "Claude and Grok compete to provably improve Covenant's own code: same verified behavior, less compute. A frozen benchmark neither can touch scores every proposal; the best one ships. Live scoreboard, every verdict public.";

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
  handle: string | null;
  reason: string | null;
};

type Arena = {
  updatedAt: string;
  baselineFuel: number;
  incumbent: { scalar: number; fuelCutPct: number };
  tally: { Claude: number; Grok: number; Codex: number; rejectedRounds: number };
  curve: { round: number | string; scalar: number; proposer?: string }[];
  solo: { promotions: number; rejected: number; finalScalar: number };
  community: { ships: number };
  rounds: { round: string; display: string; era: string; entries: Entry[] }[];
};

const RAW_URL =
  "https://raw.githubusercontent.com/open-covenant/covenant/feat/self-improvement/landing/public/arena.json";
const LOOP_RAW_URL =
  "https://raw.githubusercontent.com/open-covenant/covenant/feat/self-improvement/landing/public/loop.json";

type LoopData = {
  updatedAt: string;
  branch: string;
  totals: { events: number; tasks: number; integrated: number };
  throughput: { last24h: number; last7d: number };
  states: Record<string, number>;
  inFlight: { task: string; state: string; since: string } | null;
  recent: { at: string; task: string; note: string }[];
};

const ACCENT: Record<string, string> = {
  Claude: "#e8927c",
  Grok: "#8ab4f8",
  Codex: "#7ee0a8",
};
const NEUTRAL = "#737373";

async function loadArena(): Promise<Arena> {
  try {
    const res = await fetch(RAW_URL, { next: { revalidate: 60 } });
    if (res.ok) return (await res.json()) as Arena;
  } catch {}
  return JSON.parse(
    readFileSync(join(process.cwd(), "public", "arena.json"), "utf8"),
  ) as Arena;
}

async function loadLoop(): Promise<LoopData | null> {
  try {
    const res = await fetch(LOOP_RAW_URL, { next: { revalidate: 60 } });
    if (res.ok) return (await res.json()) as LoopData;
  } catch {}
  try {
    return JSON.parse(
      readFileSync(join(process.cwd(), "public", "loop.json"), "utf8"),
    ) as LoopData;
  } catch {}
  return null;
}

function Curve({ points }: { points: Arena["curve"] }) {
  const w = 640;
  const h = 200;
  const pad = 10;
  const max = Math.max(...points.map((p) => p.scalar)) * 1.08;
  const step = points.length > 1 ? (w - pad * 2) / (points.length - 1) : 0;
  const xy = points.map((p, i) => [
    pad + i * step,
    h - pad - ((p.scalar - 1) / (max - 1)) * (h - pad * 2),
  ]);
  const path = xy
    .map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`)
    .join(" ");
  return (
    <svg
      viewBox={`0 0 ${w} ${h}`}
      className="mt-6 w-full"
      aria-label="Efficiency multiple per promoted round"
    >
      <path
        d={path}
        fill="none"
        stroke="#d4d4d4"
        strokeWidth="1.5"
        pathLength={1}
        className="arena-draw"
      />
      {xy.map(([x, y], i) => (
        <circle
          key={i}
          cx={x}
          cy={y}
          r="3.5"
          fill="#030303"
          stroke={ACCENT[points[i].proposer ?? ""] ?? NEUTRAL}
          strokeWidth="2"
          className="arena-pop"
          style={{ animationDelay: `${0.25 + i * 0.12}s` }}
        >
          <title>{`round ${points[i].round}: ${points[i].scalar}x${points[i].proposer ? ` (${points[i].proposer})` : ""}`}</title>
        </circle>
      ))}
    </svg>
  );
}

export default async function ArenaPage() {
  const [arena, loop] = await Promise.all([loadArena(), loadLoop()]);
  const stat =
    "border border-neutral-800/80 px-5 py-5 text-center transition-colors duration-300 hover:border-neutral-600";
  const statLabel = "text-[10px] uppercase tracking-[0.3em] text-neutral-500";
  const statValue = "mt-2 text-3xl font-extralight tabular-nums";

  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <style>{`
        @keyframes arenaDraw { to { stroke-dashoffset: 0; } }
        @keyframes arenaPop { from { opacity: 0; } to { opacity: 1; } }
        @keyframes arenaRise { from { opacity: 0; transform: translateY(10px); } to { opacity: 1; transform: none; } }
        .arena-draw { stroke-dasharray: 1; stroke-dashoffset: 1; animation: arenaDraw 1.8s cubic-bezier(0.4, 0, 0.2, 1) 0.2s forwards; }
        .arena-pop { opacity: 0; animation: arenaPop 0.5s ease-out forwards; }
        .arena-rise { opacity: 0; animation: arenaRise 0.55s cubic-bezier(0.16, 1, 0.3, 1) forwards; }
        .arena-scroll { scrollbar-width: thin; scrollbar-color: #404040 transparent; }
        .arena-scroll::-webkit-scrollbar { width: 4px; }
        .arena-scroll::-webkit-scrollbar-thumb { background: #404040; border-radius: 2px; }
        .arena-scroll::-webkit-scrollbar-track { background: transparent; }
        @media (prefers-reduced-motion: reduce) {
          .arena-draw, .arena-pop, .arena-rise { animation: none; opacity: 1; stroke-dashoffset: 0; transform: none; }
        }
      `}</style>
      <SiteHeader />

      <div className="page-container">
        <div className="mb-10 flex items-center gap-4 sm:mb-14">
          <h1 className="text-[11px] uppercase tracking-[0.4em] text-neutral-400">
            Arena
          </h1>
          <span className="flex items-center gap-2 text-[10px] uppercase tracking-[0.3em] text-emerald-300/90">
            <span className="relative flex h-[7px] w-[7px]">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-60" />
              <span className="relative inline-flex h-[7px] w-[7px] rounded-full bg-emerald-400" />
            </span>
            Live
          </span>
        </div>

        <div className="lg:grid lg:grid-cols-2 lg:gap-14">
          <div>
            <div className="lg:sticky lg:top-24">
              <h2 className="text-balance text-[2.6rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[3rem]">
                <span style={{ color: ACCENT.Claude }}>Claude</span>{" "}
                <span className="text-neutral-500">vs</span>{" "}
                <span style={{ color: ACCENT.Grok }}>Grok</span>{" "}
                <span className="text-neutral-500">vs</span>{" "}
                <span style={{ color: ACCENT.Codex }}>Codex</span>
              </h2>

              <p className="mt-6 text-pretty text-lg font-light leading-relaxed text-neutral-300">
                Covenant is built by a recursive, self-improving loop: an
                autonomous agent that ships this codebase and then rewrites its
                own components to make them measurably better. The arena is
                where that happens in the open. Each round, three frontier models, Anthropic&apos;s Claude Fable 5, xAI&apos;s Grok 4.3, and
                OpenAI&apos;s GPT-5.5 Codex each propose a rewrite of live
                Covenant code. A frozen
                benchmark neither can touch measures exact instruction cost,
                held-out suites require bit-identical behavior, and the best
                proposal ships. Rejections are listed next to wins.
              </p>

              <div className="mt-10 grid grid-cols-3 gap-3">
                <div className={`${stat} arena-rise`} style={{ animationDelay: "0.05s" }}>
                  <div className={statLabel}>Claude</div>
                  <div className={statValue} style={{ color: ACCENT.Claude }}>
                    {arena.tally.Claude}
                  </div>
                </div>
                <div className={`${stat} arena-rise`} style={{ animationDelay: "0.1s" }}>
                  <div className={statLabel}>Grok</div>
                  <div className={statValue} style={{ color: ACCENT.Grok }}>
                    {arena.tally.Grok}
                  </div>
                </div>
                <div className={`${stat} arena-rise`} style={{ animationDelay: "0.15s" }}>
                  <div className={statLabel}>Codex</div>
                  <div className={statValue} style={{ color: ACCENT.Codex }}>
                    {arena.tally.Codex}
                  </div>
                </div>
              </div>
              <div className="mt-3 grid grid-cols-2 gap-3">
                <div className={`${stat} arena-rise`} style={{ animationDelay: "0.2s" }}>
                  <div className={statLabel}>Rejected rounds</div>
                  <div className={`${statValue} text-neutral-400`}>
                    {arena.tally.rejectedRounds}
                  </div>
                </div>
                <div className={`${stat} arena-rise`} style={{ animationDelay: "0.26s" }}>
                  <div className={statLabel}>Compute cut</div>
                  <div className={`${statValue} text-emerald-300`}>
                    {arena.incumbent.fuelCutPct}%
                  </div>
                </div>
                <div
                  className={`${stat} arena-rise col-span-2 sm:col-span-4 lg:col-span-2`}
                  style={{ animationDelay: "0.33s" }}
                >
                  <div className={statLabel}>Community challenge ships</div>
                  <div className="mt-2 flex items-baseline justify-center gap-3">
                    <span className={`${statValue} mt-0 text-white`}>{arena.community.ships}</span>
                    {arena.rounds.find((r) => r.era === "challenge")?.entries[0]?.proposer && (
                      <span className="text-sm font-light" style={{ color: ACCENT.Grok }}>
                        latest: {arena.rounds.find((r) => r.era === "challenge")?.entries[0]?.proposer}
                      </span>
                    )}
                  </div>
                </div>
              </div>

              <p className="mt-10 text-[11px] uppercase tracking-[0.25em] text-neutral-500">
                Same work, less compute: efficiency multiple (now{" "}
                <span className="text-neutral-200">{arena.incumbent.scalar}x</span>)
              </p>
              <Curve points={arena.curve} />

              <p className="mt-8 text-[12px] leading-relaxed text-neutral-300">
                Open challenge: beat the kernel, any function or the whole
                block. Humans, models, agents. Clear the margin and your code
                ships, attributed.{" "}
                <a
                  href={`${GITHUB_URL}/blob/feat/self-improvement/docs/arena-challenge.md`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-emerald-300 underline underline-offset-4 transition-colors hover:text-emerald-200"
                >
                  Enter
                </a>
              </p>

              <p className="mt-4 text-[11px] leading-relaxed tracking-[0.15em] text-neutral-600">
                Promotion margin +0.005 scalar since round 4 (was +0.02; the
                metric is deterministic, so any measured gain is real).{" "}
                <a
                  href={`${GITHUB_URL}/blob/feat/self-improvement/docs/arena-challenge.md`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="underline underline-offset-4 transition-colors hover:text-neutral-300"
                >
                  Rules and open challenge
                </a>
              </p>

              <p className="mt-4 hidden text-[11px] uppercase tracking-[0.25em] text-neutral-600 lg:block">
                Updated {new Date(arena.updatedAt).toUTCString()}
              </p>
            </div>
          </div>

          <div className="mt-16 lg:mt-0">
            <div className="arena-scroll lg:sticky lg:top-24 lg:max-h-[calc(100vh-8rem)] lg:overflow-y-auto lg:pr-4">
            <ol className="relative">
              {arena.rounds.map((r, i) => {
                const isLast = i === arena.rounds.length - 1;
                const winner = r.entries.find((e) => e.promoted);
                const winColor = winner ? (ACCENT[winner.proposer] ?? NEUTRAL) : "#404040";
                const eraBreak = r.era === "solo" && (i === 0 || arena.rounds[i - 1].era === "tournament");
                return (
                  <li
                    key={r.round}
                    className={`arena-rise relative border-l border-neutral-800/80 pl-8 sm:pl-10 ${isLast ? "" : "pb-10 sm:pb-12"}`}
                    style={{ animationDelay: `${0.1 + i * 0.07}s` }}
                  >
                    {eraBreak && (
                      <div className="mb-8 -ml-8 border-y border-neutral-800/60 py-4 pl-8 text-[11px] uppercase tracking-[0.25em] text-neutral-500 sm:-ml-10 sm:pl-10">
                        Before the tournament: the loop ran solo, Claude Fable 5
                        proposing alone. {arena.solo.promotions} promotions,{" "}
                        {arena.solo.rejected} rejected, {arena.solo.finalScalar}x.
                      </div>
                    )}
                    <span
                      aria-hidden="true"
                      className="absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full transition-transform duration-300"
                      style={{ backgroundColor: winColor }}
                    />
                    <div className="mb-3 flex flex-wrap items-baseline gap-x-4 gap-y-1">
                      <span className="text-[11px] uppercase tracking-[0.3em] text-neutral-300">
                        {r.display}
                      </span>
                      <span className="text-[11px] uppercase tracking-[0.25em] text-neutral-500">
                        {winner ? (
                          <>
                            <span style={{ color: winColor }}>{winner.proposer}</span>{" "}
                            promoted, {winner.scalar}x
                          </>
                        ) : (
                          "no promotion"
                        )}
                      </span>
                      {r.era === "challenge" && winner?.commit && (
                        <a
                          href={`${GITHUB_URL}/commit/${winner.commit}`}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="text-[11px] uppercase tracking-[0.2em] text-neutral-400 underline-offset-4 transition-colors hover:text-neutral-50 hover:underline"
                        >
                          shipped {winner.commit.slice(0, 8)}
                        </a>
                      )}
                    </div>
                    <ul className={r.era === "challenge" ? "hidden" : "space-y-2"}>
                      {r.entries.map((e, j) => (
                        <li
                          key={j}
                          className="group flex flex-wrap items-baseline gap-x-3 text-sm font-light text-neutral-300"
                        >
                          <span
                            className="min-w-14"
                            style={{ color: ACCENT[e.proposer] ?? "#d4d4d4" }}
                          >
                            {e.proposer}
                          </span>
                          <span className="tabular-nums text-neutral-200">
                            {e.scalar > 0 ? `${e.scalar}x` : "—"}
                          </span>
                          {e.promoted && e.commit ? (
                            <a
                              href={`${GITHUB_URL}/commit/${e.commit}`}
                              target="_blank"
                              rel="noopener noreferrer"
                              className="text-[11px] uppercase tracking-[0.2em] text-neutral-400 underline-offset-4 transition-colors hover:text-neutral-50 hover:underline"
                            >
                              shipped {e.commit.slice(0, 8)}
                            </a>
                          ) : (
                            <span className="text-[12px] text-neutral-500">
                              {e.reason}
                            </span>
                          )}
                        </li>
                      ))}
                    </ul>
                  </li>
                );
              })}
            </ol>

            <p className="mt-12 text-[11px] uppercase tracking-[0.25em] text-neutral-600 lg:hidden">
              Updated {new Date(arena.updatedAt).toUTCString()}
            </p>
            </div>
          </div>
        </div>
        {loop && (
          <div className="mt-24 border-t border-neutral-800/80 pt-14 sm:mt-28">
            <h2 className="text-balance text-[1.8rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2rem]">
              The loop
            </h2>
            <p className="mt-5 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300">
              The arena optimizes one kernel. This is the loop that builds the
              rest of Covenant: an autonomous agent working a task ledger
              through plan, review, validation and integration, around the
              clock. Live from its ledger.
            </p>

            <div className="mt-10 grid max-w-3xl grid-cols-2 gap-3 sm:grid-cols-4">
              <div className="border border-neutral-800/80 px-5 py-5 text-center">
                <div className="text-[10px] uppercase tracking-[0.3em] text-neutral-500">Tasks integrated</div>
                <div className="mt-2 text-3xl font-extralight tabular-nums text-white">{loop.totals.integrated}</div>
              </div>
              <div className="border border-neutral-800/80 px-5 py-5 text-center">
                <div className="text-[10px] uppercase tracking-[0.3em] text-neutral-500">Last 7 days</div>
                <div className="mt-2 text-3xl font-extralight tabular-nums text-white">{loop.throughput.last7d}</div>
              </div>
              <div className="border border-neutral-800/80 px-5 py-5 text-center">
                <div className="text-[10px] uppercase tracking-[0.3em] text-neutral-500">Ledger events</div>
                <div className="mt-2 text-3xl font-extralight tabular-nums text-white">{loop.totals.events}</div>
              </div>
              <div className="border border-neutral-800/80 px-5 py-5 text-center">
                <div className="text-[10px] uppercase tracking-[0.3em] text-neutral-500">In flight</div>
                <div className="mt-2 truncate text-sm font-light text-emerald-300" title={loop.inFlight?.task ?? "idle"}>
                  {loop.inFlight ? loop.inFlight.task : "idle"}
                </div>
                <div className="mt-1 text-[10px] uppercase tracking-[0.2em] text-neutral-500">
                  {loop.inFlight?.state.replace("_", " ") ?? ""}
                </div>
              </div>
            </div>

            <p className="mt-10 text-[11px] uppercase tracking-[0.25em] text-neutral-500">
              Recent integrations
            </p>
            <ul className="mt-4 max-w-3xl space-y-3">
              {loop.recent.map((r, i) => (
                <li key={i} className="flex flex-wrap items-baseline gap-x-3 text-sm font-light text-neutral-300">
                  <span className="text-neutral-100">{r.task}</span>
                  <span className="text-[11px] uppercase tracking-[0.2em] text-neutral-500">
                    {new Date(r.at).toUTCString().slice(5, 16)}
                  </span>
                  <span className="w-full text-[13px] leading-relaxed text-neutral-500">{r.note}…</span>
                </li>
              ))}
            </ul>

            <p className="mt-8 text-[11px] uppercase tracking-[0.25em] text-neutral-600">
              Snapshot {new Date(loop.updatedAt).toUTCString()} · sanitized
              aggregates from the loop&apos;s task ledger
            </p>
          </div>
        )}
      </div>

      <SiteFooter />
    </main>
  );
}
