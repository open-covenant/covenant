// /agents/[asset] — the agent passport. Renders the three independently
// checkable facts about a 014 Registry asset (registry binding, Covenant
// authority, witness-chain reproducibility) in the same witness language
// as /verify/[sha]. Registered and proven are different states here, on
// purpose — the gap between them is the page's whole argument.

import type { Metadata } from "next";
import Link from "next/link";
import { headers } from "next/headers";
import { notFound } from "next/navigation";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";
import { ChainVerifier } from "@/app/agents/ChainVerifier";
import {
  COVENANT_DATA_AUTHORITY,
  metaplexAgentUrl,
  solscanAccountUrl,
} from "@/app/agents/_registry";
import type { AgentPassport } from "@/app/api/agents/[asset]/route";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export const metadata: Metadata = {
  title: "Agent passport — Covenant",
  description:
    "Verify a 014 Registry agent: on-chain identity binding, Covenant attestation authority, and an audit root recomputed in your browser.",
};

async function fetchPassport(asset: string): Promise<AgentPassport | null> {
  const h = await headers();
  const reqHost = h.get("x-forwarded-host") || h.get("host");
  const proto = h.get("x-forwarded-proto") || "https";
  const base = reqHost
    ? `${proto}://${reqHost}`
    : process.env.NEXT_PUBLIC_SITE_URL || "http://localhost:3000";
  const res = await fetch(`${base}/api/agents/${encodeURIComponent(asset)}`, {
    cache: "no-store",
  });
  if (res.status === 404 || res.status === 400) return null;
  if (!res.ok) throw new Error(`agents api ${res.status}`);
  return (await res.json()) as AgentPassport;
}

type State = "green" | "yellow" | "red" | "gray";

function StateDot({ state }: { state: State }) {
  const tone =
    state === "green"
      ? "bg-emerald-400"
      : state === "yellow"
        ? "bg-amber-400"
        : state === "red"
          ? "bg-rose-400"
          : "bg-neutral-600";
  return (
    <span aria-hidden="true" className={`inline-block h-2.5 w-2.5 rounded-full ${tone} shrink-0`} />
  );
}

function Check({
  state,
  label,
  detail,
  evidenceHref,
  evidenceLabel,
}: {
  state: State;
  label: string;
  detail: string;
  evidenceHref?: string;
  evidenceLabel?: string;
}) {
  return (
    <div className="flex flex-col gap-3 border border-neutral-800 bg-neutral-950/60 p-5">
      <div className="flex items-center gap-2.5">
        <StateDot state={state} />
        <span className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
          {label}
        </span>
      </div>
      <p className="text-[13px] font-light leading-relaxed text-neutral-400">{detail}</p>
      {evidenceHref && (
        <a
          href={evidenceHref}
          target="_blank"
          rel="noopener noreferrer"
          className="text-[11px] uppercase tracking-[1.5px] text-neutral-500 underline-offset-4 hover:text-neutral-300 hover:underline"
        >
          {evidenceLabel ?? "Open evidence"} →
        </a>
      )}
    </div>
  );
}

function Field({ label, value, href }: { label: string; value: string; href?: string }) {
  return (
    <div>
      <dt className="text-[10px] uppercase tracking-[2px] text-neutral-500">{label}</dt>
      <dd className="mt-1.5 break-all font-mono text-[12px] text-neutral-300">
        {href ? (
          <a
            href={href}
            target="_blank"
            rel="noopener noreferrer"
            className="underline-offset-4 hover:text-neutral-100 hover:underline"
          >
            {value}
          </a>
        ) : (
          value
        )}
      </dd>
    </div>
  );
}

export default async function AgentPassportPage({
  params,
}: {
  params: Promise<{ asset: string }>;
}) {
  const { asset } = await params;
  const p = await fetchPassport(asset);
  if (!p) notFound();

  const isAgent = p.registry.registered || p.registry.identityPlugin;
  const title = p.doc?.name || p.asset.name || p.asset.id;

  const registryState: State = p.registry.registered ? "green" : isAgent ? "yellow" : "gray";
  const registryDetail = p.registry.registered
    ? `The ["agent_identity", asset] account exists at the derived address and is owned by the 014 Registry program — a tamper-evident binding between this asset and its registration. ${p.registry.identityPlugin ? "The asset carries the matching AgentIdentity plugin." : ""}`
    : isAgent
      ? "The asset carries an AgentIdentity plugin, but its registry account could not be confirmed just now."
      : "No 014 Registry record exists for this asset — it is a Core asset, not a registered agent.";

  const authorityState: State = p.attestation
    ? p.attestation.covenantAuthored
      ? "green"
      : "red"
    : p.asset.inCovenantCollection
      ? "green"
      : "gray";
  const authorityDetail = p.attestation
    ? p.attestation.covenantAuthored
      ? `Only the AppData authority can write this attestation, and the on-chain authority is Covenant's signer (${COVENANT_DATA_AUTHORITY.slice(0, 8)}…). MPL Core enforced that at write time — authorship is a chain fact, not a claim.`
      : `This attestation's AppData authority is ${p.attestation.authority ?? "unknown"}, which is NOT Covenant's signer. Treat the payload as foreign.`
    : p.asset.inCovenantCollection
      ? "The asset sits in the Covenant Agents collection, whose update authority is Covenant's signer."
      : "This asset carries no Covenant attestation payload.";

  return (
    <main id="main-content" className="min-h-[100dvh] bg-[#030303] text-neutral-200">
      <SiteHeader />

      <div className="mx-auto max-w-5xl px-6 pb-24 pt-28">
        <div className="mb-10 flex flex-col gap-2">
          <p className="text-[11px] uppercase tracking-[3px] text-neutral-500">
            <Link href="/agents" className="hover:text-neutral-300">
              Agent passport
            </Link>
          </p>
          <h1 className="text-2xl font-light tracking-tight text-white sm:text-3xl">{title}</h1>
          {p.doc?.description && (
            <p className="max-w-3xl text-[13px] font-light leading-relaxed text-neutral-400">
              {p.doc.description}
            </p>
          )}
        </div>

        <div className="mb-6 border border-neutral-800 bg-neutral-950/60 p-5">
          <dl className="grid gap-4 sm:grid-cols-2">
            <Field label="Core asset" value={p.asset.id} href={solscanAccountUrl(p.asset.id)} />
            <Field
              label="Registry account"
              value={p.registry.pda}
              href={solscanAccountUrl(p.registry.pda)}
            />
            {p.asset.collection && (
              <Field
                label="Collection"
                value={p.asset.collection}
                href={solscanAccountUrl(p.asset.collection)}
              />
            )}
            {p.registry.registrationUri && (
              <Field
                label="Registration document"
                value={p.registry.registrationUri}
                href={p.registry.registrationUri}
              />
            )}
          </dl>
          {isAgent && (
            <p className="mt-4 text-[11px] uppercase tracking-[1.5px]">
              <a
                href={metaplexAgentUrl(p.asset.id)}
                target="_blank"
                rel="noopener noreferrer"
                className="text-neutral-500 underline-offset-4 hover:text-neutral-300 hover:underline"
              >
                View on the Metaplex agent directory →
              </a>
            </p>
          )}
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          <Check
            state={registryState}
            label="014 Registry binding"
            detail={registryDetail}
            evidenceHref={solscanAccountUrl(p.registry.pda)}
            evidenceLabel="Registry account"
          />
          <Check
            state={authorityState}
            label="Covenant authority"
            detail={authorityDetail}
            evidenceHref={solscanAccountUrl(p.asset.id)}
            evidenceLabel="Asset account"
          />
          {isAgent && (
            <Check
              state={p.doc ? (p.doc.listsThisAsset ? "green" : "yellow") : "yellow"}
              label="Registration document"
              detail={
                p.doc
                  ? p.doc.listsThisAsset
                    ? "The hosted ERC-8004 document lists this exact asset under registrations — the off-chain identity points back at the on-chain one."
                    : "The hosted document loads but does not (yet) list this asset under registrations."
                  : "The registration document could not be fetched just now."
              }
              evidenceHref={p.registry.registrationUri ?? undefined}
              evidenceLabel="Document"
            />
          )}
        </div>

        {p.attestation && (
          <div className="mt-6 flex flex-col gap-4">
            <div className="border border-neutral-800 bg-neutral-950/60 p-5">
              <h2 className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
                Attestation payload
              </h2>
              <dl className="mt-4 grid gap-4 sm:grid-cols-2">
                <Field label="Schema" value={p.attestation.payload.schema} />
                <Field
                  label="Recorded at"
                  value={new Date(p.attestation.payload.recordedAt * 1000).toISOString()}
                />
                <Field label="Target" value={p.attestation.payload.releaseTarget} />
                <Field label="Subject" value={p.attestation.payload.releaseSubject} />
              </dl>
            </div>
            <ChainVerifier rootHex={p.attestation.payload.rootHashHex} />
          </div>
        )}

        <p className="mt-10 max-w-3xl text-[12px] font-light leading-relaxed text-neutral-500">
          Registered and proven are different states. Registration binds an asset to the 014
          Registry; proof is an audit root that reproduces, hash by hash, from the published
          witness chain. This page never asks a server which one you are looking at — it checks.
        </p>
      </div>

      <SiteFooter />
    </main>
  );
}
