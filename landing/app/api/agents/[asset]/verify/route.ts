// /api/verify/[asset] — "Covenant Verified": is this a valid Covenant
// audit-root attestation, or an accountable agent? DAS-only, no Covenant
// infrastructure in the trust path — the same check the Rust
// `covenant_metaplex::verify` engine runs, ported so a browser, the
// Metaplex agent directory, or anyone can call it over HTTP.
//
// Trust anchor: MPL Core only lets the AppData `data_authority` write the
// data, so authorship is a chain fact. We check that authority against the
// Covenant attestation authority, and that the payload `validator` mirrors
// it. The record must carry the ERC-8004 validation `type`, the v2 schema,
// a declared `hashAlg`, and a 64-hex `responseHash`.

import { NextResponse } from "next/server";
import { PublicKey } from "@solana/web3.js";
import {
  ATTESTATION_HASH_ALG,
  ATTESTATION_SCHEMA_V2,
  ATTESTATION_TYPE,
  COVENANT_DATA_AUTHORITY,
} from "@/app/agents/_registry";
import { readAuditGate } from "@/app/agents/_gate";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

type Verdict = {
  asset: string | null;
  verified: boolean;
  subjectAsset: string | null;
  authority: string | null;
  responseHash: string | null;
  recordedAt: number | null;
  reasons: string[];
};

function rpcUrl(): string {
  return (
    process.env.COVENANT_SOLANA_MAINNET_RPC_URL ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ||
    "https://api.mainnet-beta.solana.com"
  );
}

async function rpc(method: string, params: unknown): Promise<Record<string, unknown>> {
  const res = await fetch(rpcUrl(), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
    signal: AbortSignal.timeout(8000),
  });
  if (!res.ok) throw new Error(`rpc ${method} http ${res.status}`);
  const body = (await res.json()) as { result?: unknown; error?: { message?: string } };
  if (body.error) throw new Error(`rpc ${method}: ${body.error.message ?? "error"}`);
  return (body.result ?? {}) as Record<string, unknown>;
}

const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
// Helius re-cases on-chain camelCase to snake_case; accept either.
const field = (d: Record<string, unknown>, snake: string, camel: string): string | undefined =>
  str(d[snake]) ?? str(d[camel]);

function appData(asset: Record<string, unknown>): Record<string, unknown> | null {
  const plugins = (asset["external_plugins"] ?? []) as Array<Record<string, unknown>>;
  return plugins.find((p) => p["type"] === "AppData") ?? null;
}

const isHex64 = (s: string) => /^[0-9a-f]{64}$/.test(s);

/** Pure verification of one DAS asset as a Covenant attestation. */
function verifyAttestation(asset: Record<string, unknown>, authority: string): Verdict {
  const id = str(asset["id"]) ?? null;
  const plugin = appData(asset);
  if (!plugin) {
    return {
      asset: id,
      verified: false,
      subjectAsset: null,
      authority: null,
      responseHash: null,
      recordedAt: null,
      reasons: ["no AppData external plugin on this asset"],
    };
  }
  const data = (plugin["data"] ?? {}) as Record<string, unknown>;
  const dataAuthority =
    str((plugin["authority"] as Record<string, unknown> | undefined)?.["address"]) ?? null;
  const reasons: string[] = [];

  if (field(data, "type", "type") !== ATTESTATION_TYPE) reasons.push("type is not the ERC-8004 validation type");
  if (field(data, "schema", "schema") !== ATTESTATION_SCHEMA_V2) reasons.push(`schema is not ${ATTESTATION_SCHEMA_V2}`);
  if (field(data, "hash_alg", "hashAlg") !== ATTESTATION_HASH_ALG) reasons.push(`hashAlg is not ${ATTESTATION_HASH_ALG}`);

  const responseHash = field(data, "response_hash", "responseHash") ?? null;
  if (!responseHash) reasons.push("responseHash missing");
  else if (!isHex64(responseHash)) reasons.push("responseHash is not 64 lowercase hex");

  if (!dataAuthority) reasons.push("AppData has no write authority");
  else if (dataAuthority !== authority) reasons.push(`data authority ${dataAuthority} is not the Covenant authority ${authority}`);

  if (field(data, "validator", "validator") !== authority) reasons.push("validator field does not match the expected authority");

  const subject = (data["subject"] ?? {}) as Record<string, unknown>;
  const subjectAsset = field(subject, "asset", "asset") ?? null;
  const recordedRaw = data["recorded_at"] ?? data["recordedAt"];
  const recordedAt = typeof recordedRaw === "number" ? recordedRaw : null;

  return {
    asset: id,
    verified: reasons.length === 0,
    subjectAsset,
    authority: dataAuthority,
    responseHash,
    recordedAt,
    reasons,
  };
}

export async function GET(
  _req: Request,
  { params }: { params: Promise<{ asset: string }> },
) {
  const { asset } = await params;
  try {
    new PublicKey(asset);
  } catch {
    return NextResponse.json({ error: "not a valid Solana address" }, { status: 400 });
  }

  let das: Record<string, unknown>;
  try {
    das = await rpc("getAsset", { id: asset });
  } catch {
    return NextResponse.json({ error: "DAS endpoint unavailable" }, { status: 502 });
  }
  if (!das || das["interface"] !== "MplCoreAsset") {
    return NextResponse.json({ error: "not an MPL Core asset" }, { status: 404 });
  }

  // If the asset itself carries our attestation AppData, verify it directly.
  if (appData(das)) {
    const verdict = verifyAttestation(das, COVENANT_DATA_AUTHORITY);
    return NextResponse.json({ kind: "attestation", ...verdict });
  }

  // Otherwise treat it as an agent identity: accountable iff the Covenant
  // authority has minted a verified attestation whose subject is this asset.
  const verified: Verdict[] = [];
  try {
    for (let page = 1; page <= 5; page += 1) {
      const resp = await rpc("getAssetsByOwner", {
        ownerAddress: COVENANT_DATA_AUTHORITY,
        page,
        limit: 1000,
      });
      const items = (resp["items"] ?? []) as Array<Record<string, unknown>>;
      if (items.length === 0) break;
      for (const item of items) {
        const v = verifyAttestation(item, COVENANT_DATA_AUTHORITY);
        if (v.verified && v.subjectAsset === asset) verified.push(v);
      }
      if (items.length < 1000) break;
    }
  } catch {
    return NextResponse.json({ error: "DAS endpoint unavailable" }, { status: 502 });
  }

  const latest = verified.reduce<Verdict | null>(
    (acc, v) => (acc && (acc.recordedAt ?? 0) >= (v.recordedAt ?? 0) ? acc : v),
    null,
  );

  // Enforcement: is this agent's lifecycle gated on its live audit verdict?
  const gate = await readAuditGate(
    rpc,
    (das["external_plugins"] ?? []) as Array<Record<string, unknown>>,
    new PublicKey(asset),
  );

  return NextResponse.json({
    kind: "agent",
    agent: asset,
    accountable: verified.length > 0,
    attestationCount: verified.length,
    latest,
    gate,
  });
}
