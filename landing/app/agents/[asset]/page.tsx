// /agents/[asset] renders bounded configured-provider observations about a
// 014 Registry asset. A matching structure is not proof of operator identity,
// claim truth, record completeness, or runtime behavior.

import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { SiteFooter } from "@/app/SiteFooter";
import { SiteHeader } from "@/app/SiteHeader";
import { internalSiteUrl } from "@/lib/internalSiteUrl";
import {
  COVENANT_DATA_AUTHORITY,
  metaplexAgentUrl,
  osecVerifyUrl,
  solscanAccountUrl,
} from "@/app/agents/_registry";
import type { AgentRecordResponse } from "@/app/api/agents/[asset]/route";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export const metadata: Metadata = {
  title: "Agent record: Covenant",
  description:
    "Inspect configured DAS/RPC observations for a 014 Registry asset and related Covenant records.",
};

function safeHttpsUrl(value: string | null): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    return url.protocol === "https:" ? url.toString() : undefined;
  } catch {
    return undefined;
  }
}

async function fetchRecord(asset: string): Promise<AgentRecordResponse | null> {
  // Never derive a server-side fetch target from Host or forwarded headers.
  const url = internalSiteUrl(`/api/agents/${encodeURIComponent(asset)}`);
  const res = await fetch(url, {
    cache: "no-store",
  });
  if (res.status === 404 || res.status === 400) return null;
  if (!res.ok) throw new Error(`agents api ${res.status}`);
  return (await res.json()) as AgentRecordResponse;
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
  const p = await fetchRecord(asset);
  if (!p) notFound();

  const isAgent = p.registry.registered === true || p.registry.identityPlugin;
  const title = p.asset.name || p.asset.id;
  const registrationHref = safeHttpsUrl(p.registry.registrationUri);

  const registryState: State =
    p.registry.registered === true
      ? "green"
      : p.registry.registered === null || isAgent
        ? "yellow"
        : "gray";
  const registryDetail =
    p.registry.registered === true
      ? `The configured RPC reports that the ["agent_identity", asset] account exists at the derived address, is owned by the 014 Registry program, and names this asset in its 40-byte state. ${p.registry.identityPlugin ? "The configured DAS response also includes an AgentIdentity plugin." : ""}`
      : p.registry.registered === null
        ? "The configured RPC could not answer, so the registry binding is unknown."
        : isAgent
          ? "The asset carries an AgentIdentity plugin, but its registry account could not be confirmed just now."
          : "The configured RPC returned no matching 014 Registry account for this asset.";

  const recordAuthorityMatches =
    p.attestation?.authority === COVENANT_DATA_AUTHORITY;
  const authorityState: State = p.attestation
    ? recordAuthorityMatches
      ? "yellow"
      : "red"
    : p.asset.inCovenantCollection
      ? "yellow"
      : "gray";
  const authorityDetail = p.attestation
    ? recordAuthorityMatches
      ? `The configured DAS provider reports Covenant's signer (${COVENANT_DATA_AUTHORITY.slice(0, 8)}…) as this record's AppData authority. This page does not independently authenticate the underlying Core account.`
      : `The configured DAS response reports ${p.attestation.authority ?? "no authority"}, not Covenant's expected signer.`
    : p.asset.inCovenantCollection
      ? "The configured DAS provider reports that this asset belongs to the Covenant Agents collection."
      : "The configured DAS response includes no Covenant AppData on this asset. Any matching validation record is a separate asset.";

  return (
    <main id="main-content" className="min-h-[100dvh] bg-[#030303] text-neutral-200">
      <SiteHeader />

      <div className="mx-auto max-w-5xl px-6 pb-24 pt-28">
        <div className="mb-10 flex flex-col gap-2">
          <p className="text-[11px] uppercase tracking-[3px] text-neutral-500">
            <Link href="/agents" className="hover:text-neutral-300">
              Agent record
            </Link>
          </p>
          <h1 className="text-2xl font-light tracking-tight text-white sm:text-3xl">
            {title}
          </h1>
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
                href={registrationHref}
              />
            )}
            {p.gate?.gated && (
              <Field
                label="Gating program (pinned)"
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
              state="gray"
              label="Registration URI"
              detail="This URI comes from untrusted on-chain or DAS input. Covenant displays an HTTPS reference when present but does not fetch or validate the document server-side."
              evidenceHref={registrationHref}
              evidenceLabel="Open untrusted document"
            />
          )}
          {isAgent && (
            <Check
              state={
                p.records == null
                  ? "yellow"
                  : p.records.hasMatchingRecord
                    ? "yellow"
                    : p.records.truncated
                      ? "yellow"
                      : "gray"
              }
              label="Publisher-reported records"
              detail={
                p.records == null
                  ? "The historical AppData commitment lookup could not complete just now."
                  : p.records.hasMatchingRecord
                    ? `The configured DAS provider reports ${p.records.count} record${p.records.count === 1 ? "" : "s"} with the expected Covenant envelope naming this asset${p.records.latest?.recordedAt ? `, latest dated ${new Date(p.records.latest.recordedAt * 1000).toISOString().slice(0, 10)}` : ""}. This structural match does not prove the claim, completeness of the record set, or agent safety.`
                    : p.records.truncated
                      ? "The bounded DAS lookup hit its page cap, so a matching record may exist outside the scanned result set."
                      : "The bounded configured-DAS lookup found no record with the expected Covenant envelope naming this asset."
              }
              evidenceHref={
                p.records?.latest?.asset
                  ? solscanAccountUrl(p.records.latest.asset)
                  : undefined
              }
              evidenceLabel="Historical AppData commitment"
            />
          )}
          {p.gate?.gated && (
            <Check
              state={p.gate.inPolicy === true ? "green" : p.gate.inPolicy === false ? "red" : "yellow"}
              label="Audit gate"
              detail={
                p.gate.inPolicy === true
                  ? `The configured providers report a Core Oracle plugin for ${p.gate.gatedEvents.join(" / ")} and an in-policy Covenant oracle value. If those reports and the deployed configuration are correct, Core applies that rule to the listed events.`
                  : p.gate.inPolicy === false
                    ? `The configured providers report a Core Oracle plugin for ${p.gate.gatedEvents.join(" / ")} and an out-of-policy Covenant oracle value. If those reports and the deployed configuration are correct, Core rejects the listed events.`
                    : `The configured DAS response reports a Core Oracle plugin for ${p.gate.gatedEvents.join(" / ")}, but the current oracle value could not be read.`
              }
              evidenceHref={osecVerifyUrl(p.gate.programId)}
              evidenceLabel="Pinned gating program"
            />
          )}
        </div>

        {p.attestation && (
          <div className="mt-6 border border-neutral-800 bg-neutral-950/60 p-5">
            <div className="flex items-center gap-2.5">
              <StateDot
                state={p.attestation.matchesExpectedEnvelope ? "yellow" : "red"}
              />
              <span className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
                Historical AppData commitment
              </span>
            </div>
            <p className="mt-3 text-[13px] font-light leading-relaxed text-neutral-400">
              {p.attestation.matchesExpectedEnvelope
                ? "The configured DAS provider reports AppData with the expected Covenant authority, historical application type tag, schema, and response-hash shape. This is a structural match over indexer output, not proof that the claim is true."
                : `The reported AppData does not match the expected Covenant record envelope: ${p.attestation.reasons.join("; ")}.`}
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
                  label="Configured data authority"
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
          Registration, matching records, and an oracle value are different
          observations. A colored state means the configured DAS or RPC response
          matched the implemented check—not that the agent is trustworthy or
          safe. These providers remain dependencies until the underlying account
          data or a cryptographic proof is checked independently.
        </p>
      </div>

      <SiteFooter />
    </main>
  );
}
