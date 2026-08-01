// /api/agents/[asset]/verify returns bounded structural observations from the
// configured DAS and RPC providers. A matching record envelope is not an
// authenticated Core proof and does not establish claim truth or agent safety.

import { NextResponse } from "next/server";
import { PublicKey } from "@solana/web3.js";
import { COVENANT_DATA_AUTHORITY } from "@/app/agents/_registry";
import {
  appData,
  findValidationRecords,
  inspectValidationRecord,
} from "@/app/agents/_attest";
import { readAuditGate } from "@/app/agents/_gate";
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

  let das: Record<string, unknown>;
  try {
    das = (await rpc("getAsset", { id: asset })) as Record<string, unknown>;
  } catch (e) {
    if (isAssetNotFound(e)) {
      return NextResponse.json({ error: "no asset at this address" }, { status: 404 });
    }
    return NextResponse.json({ error: "DAS endpoint unavailable" }, { status: 502 });
  }
  if (!das || das["interface"] !== "MplCoreAsset") {
    return NextResponse.json({ error: "not an MPL Core asset" }, { status: 404 });
  }

  // This verdict embeds the live gate state and is the call a directory or CDN
  // is most likely to cache, so it must never be served stale.
  const noStore = { headers: { "Cache-Control": "no-store" } };

  // If the asset itself carries AppData, inspect the reported envelope.
  if (appData(das)) {
    return NextResponse.json(
      {
        kind: "record_observation",
        ...inspectValidationRecord(das, COVENANT_DATA_AUTHORITY),
      },
      noStore,
    );
  }

  // Otherwise look for DAS-reported records whose envelope names this asset.
  let records;
  try {
    records = await findValidationRecords(rpc, asset, COVENANT_DATA_AUTHORITY);
  } catch {
    return NextResponse.json({ error: "DAS endpoint unavailable" }, { status: 502 });
  }

  // Enforcement: is this agent's lifecycle gated on its live audit verdict?
  const gate = await readAuditGate(
    rpc,
    objects(das["external_plugins"]),
    assetPk,
  );

  return NextResponse.json(
    {
      kind: "agent_observation",
      evidenceSource: "configured_das_and_rpc",
      agent: asset,
      hasMatchingRecord: records.hasMatchingRecord,
      recordCount: records.count,
      latest: records.latest,
      gate,
    },
    noStore,
  );
}
