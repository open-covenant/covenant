// /api/agents/[asset]: server-side observations for an agent record.
//
// Three provider-backed observations are collected for each request:
//   1. the Core asset as reported by the configured DAS provider,
//   2. the 014 Registry binding (the ["agent_identity", asset] PDA under
//      the registry program, derived here, fetched over plain RPC),
//   3. Covenant validation-record AppData as reported by the configured DAS.
// These are configured-provider observations, not account proofs or a
// recomputed witness chain.

import { NextResponse } from "next/server";
import { PublicKey } from "@solana/web3.js";
import {
  AGENT_IDENTITY_PROGRAM,
  COVENANT_COLLECTION,
  COVENANT_DATA_AUTHORITY,
} from "@/app/agents/_registry";
import { readAuditGate, type AuditGate } from "@/app/agents/_gate";
import {
  appData,
  findValidationRecords,
  inspectValidationRecord,
  type RecordObservation,
  type ValidationRecordLookup,
} from "@/app/agents/_attest";
import { rpc, isAssetNotFound } from "@/app/agents/_rpc";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function objects(value: unknown): Array<Record<string, unknown>> {
  return Array.isArray(value)
    ? value.filter(
        (item): item is Record<string, unknown> =>
          Boolean(item) && typeof item === "object",
      )
    : [];
}

function hasRegistryBinding(info: unknown, asset: PublicKey): boolean {
  if (!info || typeof info !== "object") return false;
  const value = (info as { value?: unknown }).value;
  if (!value || typeof value !== "object") return false;
  const account = value as { owner?: unknown; data?: unknown };
  if (account.owner !== AGENT_IDENTITY_PROGRAM || !Array.isArray(account.data))
    return false;
  const [encoded, encoding] = account.data;
  if (typeof encoded !== "string" || encoding !== "base64") return false;
  const bytes = Buffer.from(encoded, "base64");
  return bytes.length === 40 && bytes.subarray(8).equals(asset.toBuffer());
}

export type AgentRecordResponse = {
  asset: {
    id: string;
    name: string;
    uri: string;
    owner: string;
    authority: string;
    inCovenantCollection: boolean;
    collection: string | null;
    burnt: boolean;
  };
  registry: {
    pda: string;
    /** null means the configured RPC could not answer. */
    registered: boolean | null;
    /** AgentIdentity external plugin found on the asset itself. */
    identityPlugin: boolean;
    registrationUri: string | null;
  };
  /** Structural observation when this asset is itself a validation record. */
  attestation: RecordObservation | null;
  /** Registration documents are not server-fetched because their URI is untrusted. */
  doc: {
    name: string;
    image: string | null;
    description: string | null;
    listsThisAsset: boolean;
  } | null;
  /** Covenant audit gate (Core Oracle plugin), when the asset carries one. */
  gate: AuditGate | null;
  /** DAS-reported validation records that name this asset as subject. */
  records: ValidationRecordLookup | null;
};

export async function GET(
  _req: Request,
  { params }: { params: Promise<{ asset: string }> },
) {
  const { asset } = await params;

  let assetPk: PublicKey;
  try {
    assetPk = new PublicKey(asset);
  } catch {
    return NextResponse.json({ error: "not a valid Solana address" }, { status: 400 });
  }

  // 1. The asset, via DAS.
  let das: Record<string, unknown>;
  try {
    das = (await rpc("getAsset", { id: assetPk.toBase58() })) as Record<string, unknown>;
  } catch (e) {
    if (isAssetNotFound(e)) {
      return NextResponse.json({ error: "no asset at this address" }, { status: 404 });
    }
    return NextResponse.json({ error: "asset lookup failed: DAS endpoint unavailable" }, { status: 502 });
  }
  if (!das || das["interface"] !== "MplCoreAsset") {
    return NextResponse.json(
      { error: "not an MPL Core asset: the 014 Registry binds Core assets only" },
      { status: 404 },
    );
  }

  const content = (das["content"] ?? {}) as Record<string, unknown>;
  const metadata = (content["metadata"] ?? {}) as Record<string, unknown>;
  const ownership = (das["ownership"] ?? {}) as Record<string, unknown>;
  const authorities = objects(das["authorities"]);
  const grouping = objects(das["grouping"]);
  const externalPlugins = objects(das["external_plugins"]);

  const collection =
    (grouping.find((g) => g["group_key"] === "collection")?.["group_value"] as
      | string
      | undefined) ?? null;

  // The 014 Registry PDA derives synchronously from the asset.
  const [pda] = PublicKey.findProgramAddressSync(
    [Buffer.from("agent_identity"), assetPk.toBytes()],
    new PublicKey(AGENT_IDENTITY_PROGRAM),
  );

  const identityPlugin = externalPlugins.find(
    (plugin) => plugin["type"] === "AgentIdentity",
  );
  const adapterConfig = identityPlugin?.["adapter_config"];
  const reportedUri =
    adapterConfig && typeof adapterConfig === "object"
      ? (adapterConfig as Record<string, unknown>)["uri"]
      : null;
  const registrationUri = typeof reportedUri === "string" ? reportedUri : null;

  // If the asset itself carries AppData, inspect the DAS-reported envelope.
  // Agents normally have no AppData on themselves; their records are separate
  // assets surfaced via the records lookup below.
  const attestation = appData(das)
    ? inspectValidationRecord(das, COVENANT_DATA_AUTHORITY)
    : null;

  const jsonUri = (content["json_uri"] as string | undefined) ?? "";
  // Registration URIs are controlled by on-chain input. Do not fetch them from
  // this server: even HTTPS URLs and redirects can target private infrastructure.
  const [registered, gate, records] = await Promise.all([
    rpc("getAccountInfo", [pda.toBase58(), { encoding: "base64" }])
      .then((info) => hasRegistryBinding(info, assetPk))
      .catch(() => null as boolean | null),
    readAuditGate(rpc, externalPlugins, assetPk),
    findValidationRecords(
      rpc,
      assetPk.toBase58(),
      COVENANT_DATA_AUTHORITY,
    ).catch(() => null as ValidationRecordLookup | null),
  ]);

  const record: AgentRecordResponse = {
    asset: {
      id: assetPk.toBase58(),
      name: String(metadata["name"] ?? ""),
      uri: jsonUri,
      owner: String(ownership["owner"] ?? ""),
      authority: String(authorities[0]?.["address"] ?? ""),
      inCovenantCollection: collection === COVENANT_COLLECTION,
      collection,
      burnt: das["burnt"] === true,
    },
    registry: {
      pda: pda.toBase58(),
      registered,
      identityPlugin: Boolean(identityPlugin),
      registrationUri,
    },
    attestation,
    doc: null,
    gate,
    records,
  };

  // Includes current provider observations; do not serve a stale response.
  return NextResponse.json(record, {
    headers: { "Cache-Control": "no-store" },
  });
}
