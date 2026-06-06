// /skills — the Covenant skill catalog.
//
// Renders the daemon's `covenant skill list --json` shape (skill_list_json)
// from the committed snapshot at landing/public/skills.json, regenerated at
// prebuild by scripts/gen-skills.mjs. Each entry is content-addressed by the
// same digest covenant-skills pins at install, so the hash shown here is the
// one a daemon verifies — not a placeholder. Devnet-first, pre-release.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { Metadata } from "next";
import Link from "next/link";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export const metadata: Metadata = {
  title: "Skill Catalog: Content-Addressed Solana Agent Skills",
  description:
    "The Covenant skill catalog — Solana Agent Skills pinned by content digest, with declared capabilities, on-chain programs, and immutable source coordinates. Devnet-first.",
  alternates: { canonical: "/skills" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/skills",
    title: "Skill Catalog: Content-Addressed Solana Agent Skills",
    description:
      "Solana Agent Skills pinned by content digest, with declared capabilities and immutable source coordinates.",
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "Skill Catalog: Content-Addressed Solana Agent Skills",
    description:
      "Solana Agent Skills pinned by content digest, with immutable source coordinates.",
    images: "/twitter-image.jpg",
  },
};

type SkillSource = { url: string; tag: string; commit: string };
type Skill = {
  name: string;
  version: string;
  description: string;
  digest: string;
  source: SkillSource;
  references: string[];
  declared_capabilities: string[];
  declared_programs: string[];
  sends_tx: boolean;
};

const strList = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((s): s is string => typeof s === "string") : [];

function coerce(raw: unknown): Skill | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.name !== "string" || !r.name) return null;
  const src = (r.source ?? {}) as Record<string, unknown>;
  return {
    name: r.name,
    version: typeof r.version === "string" ? r.version : "",
    description: typeof r.description === "string" ? r.description : "",
    digest: typeof r.digest === "string" ? r.digest : "",
    source: {
      url: typeof src.url === "string" ? src.url : "",
      tag: typeof src.tag === "string" ? src.tag : "",
      commit: typeof src.commit === "string" ? src.commit : "",
    },
    references: strList(r.references),
    declared_capabilities: strList(r.declared_capabilities),
    declared_programs: strList(r.declared_programs),
    sends_tx: r.sends_tx === true,
  };
}

function readCatalog(): Skill[] {
  let raw: unknown;
  try {
    raw = JSON.parse(readFileSync(join(process.cwd(), "public", "skills.json"), "utf8"));
  } catch {
    return [];
  }
  const skills = (raw as { skills?: unknown })?.skills;
  if (!Array.isArray(skills)) return [];
  return skills.map(coerce).filter((s): s is Skill => s !== null);
}

function sourceLabel(s: SkillSource): string {
  if (s.tag) return s.tag;
  if (s.commit) return `@${s.commit}`;
  return "unreleased";
}

function Tags({ items }: { items: string[] }) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {items.map((t) => (
        <span
          key={t}
          className="border border-neutral-800 bg-neutral-900/60 px-2 py-0.5 font-mono text-[11px] text-neutral-300"
        >
          {t}
        </span>
      ))}
    </div>
  );
}

export default function SkillsPage() {
  const skills = readCatalog();

  return (
    <main id="main-content" className="min-h-[100dvh] bg-[#030303] text-neutral-200">
      <SiteHeader />

      <div className="mx-auto max-w-5xl px-6 pb-24 pt-28">
        <div className="mb-10 flex flex-col gap-2">
          <p className="text-[11px] uppercase tracking-[3px] text-neutral-500">/skills</p>
          <h1 className="text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white">
            Skill Catalog
          </h1>
          <p className="mt-2 max-w-2xl text-[13px] font-light leading-relaxed text-neutral-400">
            The Solana Agent Skills Covenant can run under verifiable execution. Each
            skill is pinned by a content <span className="text-neutral-200">digest</span> — the
            same hash a daemon recomputes at load and refuses to run under if the tree was
            swapped after approval. Capabilities are granted by signed token at install, never
            trusted from the manifest. Devnet-first; mainnet promotion is a separate, gated step.
          </p>
        </div>

        {!skills.length && (
          <div className="border border-amber-800/60 bg-amber-950/20 p-5">
            <p className="text-[13px] font-light leading-relaxed text-amber-200/80">
              No skills in the catalog snapshot yet. This page renders the output of{" "}
              <code className="text-amber-100">covenant skill list --json</code>, captured to{" "}
              <code className="text-amber-100">landing/public/skills.json</code> at build. When a
              skill is registered with <code className="text-amber-100">covenant skill add</code>,
              its name, version, content digest, and source coordinates appear here.
            </p>
          </div>
        )}

        {skills.length > 0 && (
          <div className="flex flex-col gap-5">
            {skills.map((s) => (
              <article key={s.name} className="border border-neutral-800 bg-neutral-950/60 p-6">
                <div className="flex flex-wrap items-baseline justify-between gap-3">
                  <h2 className="font-mono text-[15px] text-white">{s.name}</h2>
                  <div className="flex items-center gap-2 text-[11px] uppercase tracking-[1.5px]">
                    <span className="text-neutral-400">v{s.version || "—"}</span>
                    {s.sends_tx && (
                      <span className="border border-amber-700/60 bg-amber-950/30 px-2 py-0.5 text-amber-300">
                        sends tx
                      </span>
                    )}
                  </div>
                </div>

                {s.description && (
                  <p className="mt-3 max-w-3xl text-[13px] font-light leading-relaxed text-neutral-400">
                    {s.description}
                  </p>
                )}

                <dl className="mt-5 grid gap-5 sm:grid-cols-2">
                  <div className="sm:col-span-2">
                    <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">
                      Content digest
                    </dt>
                    <dd className="mt-1.5 break-all font-mono text-[12px] text-emerald-300">
                      {s.digest || "—"}
                    </dd>
                  </div>

                  <div>
                    <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">Source</dt>
                    <dd className="mt-1.5 text-[12px]">
                      {s.source.url ? (
                        <Link
                          href={s.source.url}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="font-mono text-neutral-300 underline-offset-4 hover:text-neutral-100 hover:underline"
                        >
                          {sourceLabel(s.source)}
                        </Link>
                      ) : (
                        <span className="font-mono text-neutral-500">unreleased</span>
                      )}
                    </dd>
                  </div>

                  <div>
                    <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">
                      References
                    </dt>
                    <dd className="mt-1.5 text-[12px] text-neutral-400">
                      {s.references.length ? (
                        <span className="font-mono">{s.references.length} pinned</span>
                      ) : (
                        <span className="text-neutral-600">none</span>
                      )}
                    </dd>
                  </div>

                  <div className="sm:col-span-2">
                    <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">
                      Declared capabilities
                    </dt>
                    <dd className="mt-1.5">
                      {s.declared_capabilities.length ? (
                        <Tags items={s.declared_capabilities} />
                      ) : (
                        <span className="text-[12px] text-neutral-600">
                          none pinned — granted by signed token at install
                        </span>
                      )}
                    </dd>
                  </div>

                  {s.declared_programs.length > 0 && (
                    <div className="sm:col-span-2">
                      <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">
                        On-chain programs
                      </dt>
                      <dd className="mt-1.5">
                        <Tags items={s.declared_programs} />
                      </dd>
                    </div>
                  )}
                </dl>
              </article>
            ))}
          </div>
        )}

        <div className="mt-10 border-t border-neutral-900 pt-6 text-[11px] uppercase tracking-[1.5px] text-neutral-500">
          Content-addressed &middot; Devnet-first &middot;{" "}
          <Link href="/docs" className="hover:text-neutral-300">
            How verification works
          </Link>
        </div>
      </div>

      <SiteFooter />
    </main>
  );
}
