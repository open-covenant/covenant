import type { Metadata } from "next";
import Link from "next/link";
import { pageMetadata } from "@/app/_metadata";
import { FieldSeam } from "../FieldSeam";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { SiteMotion } from "../SiteMotion";
import { RoadmapMap } from "./RoadmapMap";
import { RoadmapNav } from "./RoadmapNav";
import { StateChip, StateMark } from "./_marks";
import { STATE_LABEL, TRACKS, groupsOf, itemId, orderedItems, tally, type Track } from "./_roadmap";

export const metadata: Metadata = pageMetadata("/roadmap", { title: "Roadmap" }, {
  title: "Development Roadmap: Covenant",
  description:
    "Covenant's roadmap: what is live today (including Covenant Compute on Solana mainnet), what we are working on now, and the milestones on the way to the 1.0 release.",
  alternates: { canonical: "/roadmap" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/roadmap",
    title: "Covenant Roadmap: What Is Live and What Comes Next",
    description:
      "Covenant Compute is live on Solana mainnet. See what we are working on now and the milestones on the way to the 1.0 release.",
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant Roadmap: What Is Live and What Comes Next",
    description:
      "Covenant Compute is live on Solana mainnet. See what we are working on now and the milestones on the way to the 1.0 release.",
  },
});

const eyebrow = "font-mono text-[10px] font-medium uppercase tracking-[0.28em] text-muted";
const kicker = "font-mono text-[10px] font-medium uppercase tracking-[0.24em] text-muted-2";
const trackTitle = "type-display text-[clamp(1.35rem,2.1vw,1.75rem)] text-fg";
const trackTitleLong = "type-display text-[clamp(1.2rem,1.7vw,1.4rem)] text-fg";

const FOLLOW = [
  {
    title: "Changelog",
    body: "What shipped in each tagged release, with signed artifacts.",
    href: "/changelog",
  },
  {
    title: "Live build",
    body: "Each commit to Covenant as it lands, made under a signed permission grant and recorded with a receipt.",
    href: "/live",
  },
  {
    title: "Source",
    body: "The source code on GitHub, licensed under Apache 2.0.",
    href: "https://github.com/open-covenant/covenant",
  },
];

// One track: its heading, state and progress on the left (sticky from xl up),
// its items on the right in a hairline grid, grouped by state when a track has
// items in more than one.
function TrackSection({ track: t }: { track: Track }) {
  const groups = groupsOf(t);
  return (
    <section
      id={t.id}
      data-track={t.id}
      aria-labelledby={`${t.id}-title`}
      className="scroll-mt-[calc(var(--header-h)+64px)] border-t border-line py-12 first:border-t-0 md:py-16 lg:scroll-mt-[calc(var(--header-h)+16px)]"
    >
      <div className="grid gap-9 xl:grid-cols-[minmax(0,17rem)_minmax(0,1fr)] xl:gap-12">
        <header className="xl:sticky xl:top-[calc(var(--header-h)+40px)] xl:self-start">
          <p className={kicker}>
            {t.code ? (
              <>
                <span className="tabular-nums text-faint">Milestone {t.code.slice(1)}</span>
                <span aria-hidden> · </span>
              </>
            ) : null}
            {t.kicker}
          </p>
          <h2
            id={`${t.id}-title`}
            className={`${t.title.length > 30 ? trackTitleLong : trackTitle} mt-3`}
          >
            {t.title}
          </h2>
          <StateChip state={t.state} className="mt-5" />
          <p className="mt-5 max-w-[34rem] text-[12.5px] leading-[1.75] text-fg-soft">{t.intro}</p>
          <div className="mt-7 flex max-w-[17rem] flex-wrap gap-[5px]" aria-hidden>
            {orderedItems(t).map(({ item, state }) => (
              <StateMark key={item.name} state={state} size="md" />
            ))}
          </div>
          <p className="mt-3 font-mono text-[10px] uppercase tracking-[0.2em] text-muted-2">{tally(t)}</p>
        </header>

        <div className="min-w-0">
          {groups.map((g, gi) => (
            <div key={g.state} className={gi > 0 ? "mt-12" : ""}>
              {groups.length > 1 ? (
                <p className="mb-4 flex items-center gap-2.5 font-mono text-[10px] font-medium uppercase tracking-[0.22em] text-fg-2">
                  <StateMark state={g.state} size="sm" />
                  {STATE_LABEL[g.state]}
                  <span className="tabular-nums text-faint">· {String(g.items.length).padStart(2, "0")}</span>
                </p>
              ) : null}
              <ol className="grid border-t border-line sm:grid-cols-2">
                {g.items.map(({ item, n, state }) => (
                  <li
                    key={item.name}
                    id={itemId(t, item)}
                    className="rm-item reveal scroll-mt-[calc(var(--header-h)+96px)] border-b border-line p-5 sm:p-6 sm:[&:nth-child(odd)]:border-r sm:[&:nth-child(odd):last-child]:border-r-0 lg:scroll-mt-[calc(var(--header-h)+40px)]"
                  >
                    <div className="flex items-center gap-3">
                      <StateMark state={state} size="md" />
                      <span className="font-mono text-[10px] tabular-nums text-faint">{String(n).padStart(2, "0")}</span>
                    </div>
                    <h3 className="mt-4 font-mono text-[11.5px] font-semibold uppercase leading-snug tracking-[0.16em] text-fg">
                      {item.href ? (
                        <a
                          href={item.href}
                          {...(item.href.startsWith("http") ? { target: "_blank", rel: "noopener noreferrer" } : {})}
                          className="group inline-flex items-baseline gap-2 underline decoration-line-strong decoration-1 underline-offset-[5px] transition-colors hover:decoration-mint"
                        >
                          {item.name}
                          <span aria-hidden className="text-faint transition-colors group-hover:text-mint">
                            {item.href.startsWith("http") ? "↗" : "→"}
                          </span>
                        </a>
                      ) : (
                        item.name
                      )}
                    </h3>
                    {item.body ? <p className="mt-2.5 text-[12.5px] leading-[1.7] text-muted">{item.body}</p> : null}
                  </li>
                ))}
              </ol>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}

export default function RoadmapPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-clip bg-ground">
      <SiteMotion />
      <SiteHeader />

      <div
        id="top"
        className="mx-auto w-full max-w-[90rem] px-5 pt-[calc(var(--header-h)+40px+env(safe-area-inset-top))] sm:px-8 lg:pt-[calc(var(--header-h)+72px)]"
      >
        <header className="grid gap-8 md:grid-cols-[minmax(0,1fr)_minmax(280px,30rem)] md:items-end md:gap-16">
          <div>
            <p className={eyebrow}>Built in the open</p>
            <h1 className="page-title mt-5 text-fg">Roadmap</h1>
          </div>
          <p className="max-w-[46rem] text-[14px] leading-[1.7] text-fg-soft md:pb-1">
            Covenant spans everything from the software that runs on your own machine to a compute network on Solana
            mainnet. Here is where each part stands today. No item is marked Live until it has shipped.
          </p>
        </header>
        <RoadmapMap />
      </div>

      <FieldSeam seed={2.2} className="mt-14 md:mt-20" />

      <div className="mx-auto w-full max-w-[90rem] px-5 sm:px-8 lg:grid lg:grid-cols-[14rem_minmax(0,1fr)] lg:gap-14 xl:gap-20">
        <RoadmapNav />
        <div className="min-w-0 pb-24 lg:pt-4">
          {TRACKS.map((t) => (
            <TrackSection key={t.id} track={t} />
          ))}

          <section aria-labelledby="follow-title" className="border-t border-line pt-12 md:pt-16">
            <p id="follow-title" className={eyebrow}>
              Follow the build
            </p>
            <ul className="mt-6 grid gap-3 md:grid-cols-3">
              {FOLLOW.map((f) => {
                const external = f.href.startsWith("http");
                const inner = (
                  <>
                    <span className="flex items-center justify-between font-mono text-[11.5px] font-semibold uppercase tracking-[0.16em] text-fg">
                      {f.title}
                      <span aria-hidden className="text-faint transition-colors group-hover:text-mint">
                        {external ? "↗" : "→"}
                      </span>
                    </span>
                    <span className="mt-3 block text-[12.5px] leading-[1.7] text-muted">{f.body}</span>
                  </>
                );
                const cls =
                  "group block h-full rounded-[4px] border border-line bg-raised/50 p-6 transition-colors hover:border-line-strong hover:bg-raised";
                return (
                  <li key={f.href}>
                    {external ? (
                      <a href={f.href} target="_blank" rel="noopener noreferrer" className={cls}>
                        {inner}
                      </a>
                    ) : (
                      <Link href={f.href} className={cls}>
                        {inner}
                      </Link>
                    )}
                  </li>
                );
              })}
            </ul>
          </section>
        </div>
      </div>

      <SiteFooter />
    </main>
  );
}
