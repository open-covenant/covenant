// /agents: proof of agent. The index explains the trust model in three
// moves (registered, authored, reproducible) and hands the reader the
// same verifier we use ourselves. The only color on these pages is earned
// by a check passing.

import type { Metadata } from "next";
import Link from "next/link";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";
import { LookupForm } from "@/app/agents/LookupForm";
import {
  FEATURED_AGENT_ASSET,
  FEATURED_ATTESTATION_ASSET,
  metaplexAgentUrl,
} from "@/app/agents/_registry";

export const metadata: Metadata = {
  title: "Proof of agent: Covenant",
  description:
    "Covenant agents register on Metaplex's 014 Registry and anchor their audit roots as MPL Core attestations. Verify any of it in your browser.",
};

const MOVES = [
  {
    term: "Registered",
    gloss:
      "A tamper-evident PDA under the 014 Registry program binds the agent's Core asset to its registration. Anyone can derive the address and check it.",
  },
  {
    term: "Authored",
    gloss:
      "Attestations live in MPL Core AppData that only Covenant's signer may write. MPL Core enforces the authority at write time. Authorship is a chain fact.",
  },
  {
    term: "Reproducible",
    gloss:
      "Each attested root recomputes, SHA-256 by SHA-256, from the published witness chain, in your browser, with no server verdict involved.",
  },
] as const;

export default function AgentsPage() {
  return (
    <main id="main-content" className="min-h-[100dvh] bg-ground text-fg-2">
      <SiteHeader />

      <div className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <div className="mb-12 flex flex-col gap-3">
          <p className="text-[11px] uppercase tracking-[3px] text-muted-2">Agent registry</p>
          <h1 className="page-title text-fg">
            Proof of agent
          </h1>
          <p className="max-w-3xl text-[14px] leading-relaxed text-muted">
            Covenant agents register on Metaplex&apos;s 014 Registry and anchor their
            audit roots as MPL Core attestations, indexed by DAS and checkable by anyone.
            A registry tells you an agent exists. Proof tells you what it did. This page
            does both, and never asks you to take a server&apos;s word for it.
          </p>
        </div>

        <div className="mb-12 grid gap-4 sm:grid-cols-3">
          {MOVES.map((m, i) => (
            <div key={m.term} className="border border-line bg-raised/60 p-5">
              <div className="flex items-baseline gap-3">
                <span className="font-mono text-[11px] text-faint">0{i + 1}</span>
                <h2 className="type-display text-[11px] text-fg-soft">
                  {m.term}
                </h2>
              </div>
              <p className="mt-3 text-[13px] leading-relaxed text-muted">
                {m.gloss}
              </p>
            </div>
          ))}
        </div>

        <div className="mb-12 grid gap-4 sm:grid-cols-2">
          <div className="flex flex-col gap-3 border border-line bg-raised/60 p-5">
            <p className="text-[10px] uppercase tracking-[2px] text-muted-2">
              Registered production agent
            </p>
            <p className="text-[15px] text-fg">Covenant Foundation Agent</p>
            <p className="break-all font-mono text-[12px] text-muted">
              {FEATURED_AGENT_ASSET}
            </p>
            <div className="mt-auto flex flex-wrap gap-x-5 gap-y-1.5 pt-2">
              <Link
                href={`/agents/${FEATURED_AGENT_ASSET}`}
                className="text-[11px] uppercase tracking-[1.5px] text-fg-soft underline-offset-4 hover:text-fg hover:underline"
              >
                Open passport →
              </Link>
              <a
                href={metaplexAgentUrl(FEATURED_AGENT_ASSET)}
                target="_blank"
                rel="noopener noreferrer"
                className="text-[11px] uppercase tracking-[1.5px] text-muted-2 underline-offset-4 hover:text-fg-soft hover:underline"
              >
                Metaplex directory →
              </a>
            </div>
          </div>
          <div className="flex flex-col gap-3 border border-line bg-raised/60 p-5">
            <p className="text-[10px] uppercase tracking-[2px] text-muted-2">
              Latest anchored audit root
            </p>
            <p className="text-[15px] text-fg">Witness-loop attestation</p>
            <p className="break-all font-mono text-[12px] text-muted">
              {FEATURED_ATTESTATION_ASSET}
            </p>
            <div className="mt-auto pt-2">
              <Link
                href={`/agents/${FEATURED_ATTESTATION_ASSET}`}
                className="text-[11px] uppercase tracking-[1.5px] text-fg-soft underline-offset-4 hover:text-fg hover:underline"
              >
                Verify it in your browser →
              </Link>
            </div>
          </div>
        </div>

        <div className="mb-4 flex flex-col gap-2">
          <h2 className="type-display text-[11px] text-fg-soft">
            Verify any agent
          </h2>
          <p className="text-[13px] text-muted-2">
            Works for every asset bound to the 014 Registry, not just ours.
          </p>
        </div>
        <LookupForm />

        <p className="mt-10 max-w-3xl text-[12px] leading-relaxed text-muted-2">
          Built on{" "}
          <a
            href="https://www.metaplex.com/docs/smart-contracts/mpl-agent"
            target="_blank"
            rel="noopener noreferrer"
            className="underline-offset-4 hover:text-fg-soft hover:underline"
          >
            MPL Agent
          </a>{" "}
          and MPL Core AppData. The attestation schema is published in the{" "}
          <a
            href="https://github.com/open-covenant/covenant/blob/main/docs/metaplex-integration.md"
            target="_blank"
            rel="noopener noreferrer"
            className="underline-offset-4 hover:text-fg-soft hover:underline"
          >
            integration docs
          </a>
          .
        </p>
      </div>

      <SiteFooter />
    </main>
  );
}
