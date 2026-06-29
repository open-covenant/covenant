// /agents/[asset] — the agent passport. Renders the independently checkable
// facts about a 014 Registry asset (registry binding, Covenant authority,
// witness-chain reproducibility, and — for gated agents — the live audit gate)
// in the same witness language as /verify/[sha]. Registered and proven are
// different states here, on purpose; the gap between them is the page's
// whole argument.

import type { Metadata } from "next";
import Link from "next/link";
import { headers } from "next/headers";
import { notFound } from "next/navigation";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";
import {
  COVENANT_DATA_AUTHORITY,
  metaplexAgentUrl,
  osecVerifyUrl,
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

  const recordAuthored = p.attestation?.authority === COVENANT_DATA_AUTHORITY;
  const authorityState: State = p.attestation
    ? recordAuthored
      ? "green"
      : "red"
    : p.asset.inCovenantCollection
      ? "green"
      : "gray";
  const authorityDetail = p.attestation
    ? recordAuthored
      ? `Only the AppData authority can write this record, and the on-chain authority is Covenant's signer (${COVENANT_DATA_AUTHORITY.slice(0, 8)}…). MPL Core enforced that at write time — authorship is a chain fact, not a claim.`
      : `This record's AppData authority is ${p.attestation.authority ?? "unknown"}, which is NOT Covenant's signer. Treat it as foreign.`
    : p.asset.inCovenantCollection
      ? "The asset sits in the Covenant Agents collection, whose update authority is Covenant's signer."
      : "This asset carries no Covenant AppData of its own (an agent's record lives on a separate asset — see accountability).";

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
            {p.gate?.gated && (
              <Field
                label="Gating program (verified)"
                value={p.gate.programId}
                href={osecVerifyUrl(p.gate.programId)}
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
          {isAgent && (
            <Check
              state={
                p.accountability == null ? "yellow" : p.accountability.accountable ? "green" : "gray"
              }
              label="Accountability"
              detail={
                p.accountability == null
                  ? "The validation-record lookup could not complete just now."
                  : p.accountability.accountable
                    ? `A Covenant validator has minted ${p.accountability.count} verified validation record${p.accountability.count === 1 ? "" : "s"} naming this agent as subject${p.accountability.latest?.recordedAt ? `, latest recorded ${new Date(p.accountability.latest.recordedAt * 1000).toISOString().slice(0, 10)}` : ""}. Each record's on-chain AppData authority is the Covenant validator — Core enforced that at write time — and it carries the ERC-8004 validation type, schema, and a 64-hex response hash. Anyone can recheck it over DAS with no Covenant infrastructure in the path.`
                    : "No Covenant validation record names this agent as subject yet. Registration proves identity; a validation record is what makes the agent accountable."
              }
              evidenceHref={
                p.accountability?.latest?.asset
                  ? solscanAccountUrl(p.accountability.latest.asset)
                  : undefined
              }
              evidenceLabel="Validation record"
            />
          )}
          {p.gate?.gated && (
            <Check
              state={p.gate.inPolicy === true ? "green" : p.gate.inPolicy === false ? "red" : "yellow"}
              label="Audit gate"
              detail={
                p.gate.inPolicy === true
                  ? `This agent's ${p.gate.gatedEvents.join(" / ")} is gated on its live Covenant audit verdict by the Core Oracle plugin. The verdict is in policy, so Core allows it. Flip the audit out of policy and Core vetoes the event on chain — the rule is enforced by Core, not by us. The gating program is source-verified: the deployed bytes match the published source.`
                  : p.gate.inPolicy === false
                    ? `The Covenant audit verdict is out of policy, so MPL Core is vetoing this agent's ${p.gate.gatedEvents.join(" / ")} right now. It stays blocked until the audit is back in policy. The gating program is source-verified: the deployed bytes match the published source.`
                    : `This agent's ${p.gate.gatedEvents.join(" / ")} is gated by the Core Oracle plugin on its Covenant audit verdict; the current verdict could not be read just now. The gating program is source-verified on chain.`
              }
              evidenceHref={osecVerifyUrl(p.gate.programId)}
              evidenceLabel="Verified gating program"
            />
          )}
        </div>

        {p.attestation && (
          <div className="mt-6 border border-neutral-800 bg-neutral-950/60 p-5">
            <div className="flex items-center gap-2.5">
              <StateDot state={p.attestation.verified ? "green" : "red"} />
              <span className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
                Validation record
              </span>
            </div>
            <p className="mt-3 text-[13px] font-light leading-relaxed text-neutral-400">
              {p.attestation.verified
                ? "This asset is a Covenant validation record. Its on-chain AppData authority is the Covenant validator (Core enforced it at write time), and it carries the ERC-8004 validation type, the expected schema, and a 64-hex response hash."
                : `This asset carries AppData but does not verify as a Covenant validation record: ${p.attestation.reasons.join("; ")}.`}
            </p>
            <dl className="mt-4 grid gap-4 sm:grid-cols-2">
              {p.attestation.subjectAsset && (
                <Field
                  label="Subject agent"
                  value={p.attestation.subjectAsset}
                  href={`/agents/${p.attestation.subjectAsset}`}
                />
              )}
              {p.attestation.authority && (
                <Field
                  label="Validator"
                  value={p.attestation.authority}
                  href={solscanAccountUrl(p.attestation.authority)}
                />
              )}
              {p.attestation.responseHash && (
                <Field label="Response hash" value={p.attestation.responseHash} />
              )}
              {p.attestation.recordedAt && (
                <Field
                  label="Recorded at"
                  value={new Date(p.attestation.recordedAt * 1000).toISOString()}
                />
              )}
            </dl>
          </div>
        )}

        <p className="mt-10 max-w-3xl text-[12px] font-light leading-relaxed text-neutral-500">
          Registered, accountable, and in policy are different states. Registration binds an asset
          to the 014 Registry; a validation record makes the agent accountable; the audit gate
          enforces the verdict on chain. This page never asks a server which one you are looking at
          — each check is recomputed against the chain this request.
        </p>
      </div>

      <SiteFooter />
    </main>
  );
}
