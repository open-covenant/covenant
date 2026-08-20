// /api/agents/[asset]/verify. "Covenant Verified": is this a valid Covenant
// validation record, or an accountable agent? DAS-only, no Covenant
// infrastructure in the trust path, the same check the Rust
// `covenant_metaplex::verify` engine runs, ported so a browser, the Metaplex
// agent directory, or anyone can call it over HTTP.
//
// Trust anchor: MPL Core only lets the AppData `data_authority` write the data,
// so authorship is a chain fact. The record check lives in `_attest.ts`; the
// optional on-chain enforcement read lives in `_gate.ts`.

import { NextResponse } from "next/server";
import { PublicKey } from "@solana/web3.js";
import { COVENANT_DATA_AUTHORITY } from "@/app/agents/_registry";
import { appData, findAccountability, verifyAttestation } from "@/app/agents/_attest";
import { readAuditGate } from "@/app/agents/_gate";
import { rpc, isAssetNotFound } from "@/app/agents/_rpc";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

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

  // If the asset itself carries our record AppData, verify it directly.
  if (appData(das)) {
    return NextResponse.json({ kind: "attestation", ...verifyAttestation(das, COVENANT_DATA_AUTHORITY) }, noStore);
  }

  // Otherwise treat it as an agent: accountable iff the Covenant validator has
  // minted a verified record whose subject is this asset.
  let accountability;
  try {
    accountability = await findAccountability(rpc, asset, COVENANT_DATA_AUTHORITY);
  } catch {
    return NextResponse.json({ error: "DAS endpoint unavailable" }, { status: 502 });
  }

  // Enforcement: is this agent's lifecycle gated on its live audit verdict?
  const gate = await readAuditGate(
    rpc,
    (das["external_plugins"] ?? []) as Array<Record<string, unknown>>,
    assetPk,
  );

  return NextResponse.json({
    kind: "agent",
    agent: asset,
    accountable: accountability.accountable,
    attestationCount: accountability.count,
    latest: accountability.latest,
    gate,
  }, noStore);
}
