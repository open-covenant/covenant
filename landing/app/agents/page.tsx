// /agents reports structural observations from the configured DAS/RPC
// sources. It does not turn those observations into a global trust verdict.

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
  title: "Agent records: Covenant",
  description:
    "Inspect configured DAS/RPC observations for Covenant identity and AppData records. Structural checks do not prove claim truth or runtime enforcement.",
};

const MOVES = [
  {
    term: "Registered",
    gloss:
      "The configured registry program and asset derive to a PDA whose owner and bytes can be observed over RPC. This checks structure, not agent behavior.",
  },
  {
    term: "Authority match",
    gloss:
      "The supplied AppData view reports an authority that is compared with Covenant's configured key. A match attributes the observed bytes; it does not make their claim true.",
  },
  {
    term: "Record match",
    gloss:
      "A bounded DAS query compares reported AppData fields with Covenant's expected record envelope and subject. It does not authenticate the account or prove the claim.",
  },
] as const;

export default function AgentsPage() {
  return (
    <main id="main-content" className="min-h-[100dvh] bg-[#030303] text-neutral-200">
      <SiteHeader />

      <div className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <div className="mb-12 flex flex-col gap-3">
          <p className="text-[11px] uppercase tracking-[3px] text-neutral-500">Agent registry</p>
          <h1 className="text-3xl font-light tracking-tight text-white sm:text-4xl">
            Agent records
          </h1>
          <p className="max-w-3xl text-[14px] font-light leading-relaxed text-neutral-400">
            This page reports configured DAS/RPC observations for Metaplex 014
            Registry identities and MPL Core AppData commitments. The checks
            cover reported account structure, authority matches, and reported
            record envelopes. They do not prove semantic correctness, log
            completeness, runtime mediation, or W009/W011 enforcement.
          </p>
        </div>

        <div className="mb-12 grid gap-4 sm:grid-cols-3">
          {MOVES.map((m, i) => (
            <div key={m.term} className="border border-neutral-800 bg-neutral-950/60 p-5">
              <div className="flex items-baseline gap-3">
                <span className="font-mono text-[11px] text-neutral-600">0{i + 1}</span>
                <h2 className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
                  {m.term}
                </h2>
              </div>
              <p className="mt-3 text-[13px] font-light leading-relaxed text-neutral-400">
                {m.gloss}
              </p>
            </div>
          ))}
        </div>

        <div className="mb-12 grid gap-4 sm:grid-cols-2">
          <div className="flex flex-col gap-3 border border-neutral-800 bg-neutral-950/60 p-5">
            <p className="text-[10px] uppercase tracking-[2px] text-neutral-500">
              Featured mainnet identity record
            </p>
            <p className="text-[15px] font-light text-white">Covenant Foundation Agent</p>
            <p className="break-all font-mono text-[12px] text-neutral-400">
              {FEATURED_AGENT_ASSET}
            </p>
            <div className="mt-auto flex flex-wrap gap-x-5 gap-y-1.5 pt-2">
              <Link
                href={`/agents/${FEATURED_AGENT_ASSET}`}
                className="text-[11px] uppercase tracking-[1.5px] text-neutral-300 underline-offset-4 hover:text-white hover:underline"
              >
                Inspect record →
              </Link>
              <a
                href={metaplexAgentUrl(FEATURED_AGENT_ASSET)}
                target="_blank"
                rel="noopener noreferrer"
                className="text-[11px] uppercase tracking-[1.5px] text-neutral-500 underline-offset-4 hover:text-neutral-300 hover:underline"
              >
                Metaplex directory →
              </a>
            </div>
          </div>
          <div className="flex flex-col gap-3 border border-neutral-800 bg-neutral-950/60 p-5">
            <p className="text-[10px] uppercase tracking-[2px] text-neutral-500">
              Featured AppData commitment
            </p>
            <p className="text-[15px] font-light text-white">
              Recorded audit-root commitment
            </p>
            <p className="break-all font-mono text-[12px] text-neutral-400">
              {FEATURED_ATTESTATION_ASSET}
            </p>
            <div className="mt-auto pt-2">
              <Link
                href={`/agents/${FEATURED_ATTESTATION_ASSET}`}
                className="text-[11px] uppercase tracking-[1.5px] text-neutral-300 underline-offset-4 hover:text-white hover:underline"
              >
                Inspect supplied evidence →
              </Link>
            </div>
          </div>
        </div>

        <div className="mb-4 flex flex-col gap-2">
          <h2 className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
            Inspect a registered asset
          </h2>
          <p className="text-[13px] font-light text-neutral-500">
            Queries the configured DAS/RPC sources for assets bound to the 014
            Registry.
          </p>
        </div>
        <LookupForm />

        <p className="mt-10 max-w-3xl text-[12px] font-light leading-relaxed text-neutral-500">
          Built on{" "}
          <a
            href="https://www.metaplex.com/docs/smart-contracts/mpl-agent"
            target="_blank"
            rel="noopener noreferrer"
            className="underline-offset-4 hover:text-neutral-300 hover:underline"
          >
            MPL Agent
          </a>{" "}
          and MPL Core AppData. The attestation schema is published in the{" "}
          <a
            href="https://github.com/open-covenant/covenant/blob/main/docs/metaplex-integration.md"
            target="_blank"
            rel="noopener noreferrer"
            className="underline-offset-4 hover:text-neutral-300 hover:underline"
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
