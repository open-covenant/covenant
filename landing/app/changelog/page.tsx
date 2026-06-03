import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const DESCRIPTION =
  "Covenant release notes. What shipped in each tagged version, with signed artifacts and the full changelog on GitHub.";

export const metadata: Metadata = {
  title: "Changelog: Covenant",
  description: DESCRIPTION,
  alternates: { canonical: "/changelog" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/changelog",
    title: "Covenant changelog",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Covenant changelog",
    description: DESCRIPTION,
    images: "/twitter-image.jpg",
  },
};

type Release = {
  version: string;
  date: string;
  dateISO: string;
  href: string;
  summary: string;
  groups: { label: string; items: string[] }[];
};

const RELEASES: Release[] = [
  {
    version: "0.1.0-alpha.1",
    date: "May 28, 2026",
    dateISO: "2026-05-28",
    href: "https://github.com/open-covenant/covenant/releases/tag/v0.1.0-alpha.1",
    summary:
      "First tagged release. The daemon, CLI, and operator console are usable end to end, and every release artifact is signed in CI with cosign keyless OIDC.",
    groups: [
      {
        label: "Added",
        items: [
          "covenant bootstrap grants the capabilities a fresh install needs, so the first intent works out of the box.",
          "Operator console overhaul: tasks, permissions, memory, messages, agents, spending, and the activity log rewritten for non-technical operators, with a command palette.",
          "Plain-English capability titles across the console and CLI.",
          "Source installer with dry-run, upgrade preflight, and rollback.",
          "Multi-platform release workflow (macOS arm64 and x86_64, Linux x86_64) with SHA-256 checksums and cosign signatures.",
        ],
      },
      {
        label: "Changed",
        items: [
          "Getting-started and demo docs rewritten around the current CLI; alpha framing replaced with versioned releases.",
          "First-task errors point users at covenant bootstrap instead of granting each capability by hand.",
        ],
      },
      {
        label: "Security",
        items: [
          "Capability trust root enforced at every verify callsite; the daemon refuses self-granted operator capabilities.",
          "Constant-time peer-token comparison; the audit chain refuses to rebuild on a length mismatch.",
          "CI hardening: per-job timeouts, no persisted credentials, CodeQL, and cargo-audit integrity checks.",
        ],
      },
    ],
  },
];

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const groupLabel = "text-[11px] uppercase tracking-[0.25em] text-neutral-400";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const linkClass =
  "text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50";

export default function ChangelogPage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <h1 className="mb-10 text-[11px] uppercase tracking-[0.4em] text-neutral-400 sm:mb-14">
          Changelog
        </h1>

        <h2 className="max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          What shipped, version by version
        </h2>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300">
          Tagged releases with signed artifacts. The full, technical changelog
          and the live commit stream both live in the open.
        </p>

        <ol className="relative mt-16 sm:mt-24">
          {RELEASES.map((r, i) => {
            const isLast = i === RELEASES.length - 1;
            return (
              <li
                key={r.version}
                className={`relative border-l border-neutral-800/80 pl-8 sm:pl-12 ${isLast ? "" : "pb-14 sm:pb-16"}`}
              >
                <span
                  aria-hidden="true"
                  className="absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full bg-neutral-100"
                />
                <div className="mb-4 flex flex-wrap items-baseline gap-x-4 gap-y-1">
                  <span className={eyebrow}>{r.version}</span>
                  <time dateTime={r.dateISO} className="text-[11px] uppercase tracking-[0.25em] text-neutral-400">
                    {r.date}
                  </time>
                  <a
                    href={r.href}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="ml-auto text-[11px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
                  >
                    Signed release →
                  </a>
                </div>
                <p className={`mb-6 max-w-2xl ${paragraph}`}>{r.summary}</p>
                <div className="space-y-5">
                  {r.groups.map((g) => (
                    <div key={g.label}>
                      <div className={`mb-2 ${groupLabel}`}>{g.label}</div>
                      <ul className="space-y-2">
                        {g.items.map((it) => (
                          <li
                            key={it}
                            className="flex gap-3 text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]"
                          >
                            <span aria-hidden="true" className="select-none text-neutral-600">
                              ·
                            </span>
                            <span className="max-w-2xl">{it}</span>
                          </li>
                        ))}
                      </ul>
                    </div>
                  ))}
                </div>
              </li>
            );
          })}
        </ol>

        <div className="mt-16 flex flex-wrap gap-x-6 gap-y-3 border-t border-neutral-800/80 pt-8 sm:mt-24">
          <a
            href="https://github.com/open-covenant/covenant/blob/main/CHANGELOG.md"
            target="_blank"
            rel="noopener noreferrer"
            className={linkClass}
          >
            Full changelog →
          </a>
          <a
            href="https://github.com/open-covenant/covenant/releases"
            target="_blank"
            rel="noopener noreferrer"
            className={linkClass}
          >
            All releases →
          </a>
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
