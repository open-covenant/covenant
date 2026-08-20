// /api/agents/[asset]: server-side verification for the agent passport.
//
// Three independent facts, each checked against the chain this request:
//   1. the Core asset itself (DAS getAsset),
//   2. the 014 Registry binding (the ["agent_identity", asset] PDA under
//      the registry program, derived here, fetched over plain RPC),
//   3. the Covenant attestation AppData and its on-chain write authority.
// The witness-chain recomputation deliberately does NOT happen here: the
// browser fetches the published event chain and recomputes the root
// itself, so the proof never depends on this server saying so.

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
  findAccountability,
  verifyAttestation,
  type Accountability,
  type Verdict,
} from "@/app/agents/_attest";
import { rpc, isAssetNotFound } from "@/app/agents/_rpc";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export type AgentPassport = {
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
    registered: boolean;
    /** AgentIdentity external plugin found on the asset itself. */
    identityPlugin: boolean;
    registrationUri: string | null;
  };
  /** If this asset is itself a validation record, its verification verdict. */
  attestation: Verdict | null;
  doc: {
    name: string;
    image: string | null;
    description: string | null;
    listsThisAsset: boolean;
  } | null;
  /** Covenant audit gate (Core Oracle plugin), when the asset carries one. */
  gate: AuditGate | null;
  /** Validation records that name this asset as subject (agent accountability). */
  accountability: Accountability | null;
};

const MAX_DOC_BYTES = 256 * 1024;

// docUri is an on-chain-controlled URL, so the fetch is hostile by default:
// https-only, a 4s timeout, and a hard byte ceiling so a doc can't make us
// buffer unbounded. No egress allowlist: doc URIs are arbitrary by design.
async function fetchDoc(docUri: string, assetId: string): Promise<AgentPassport["doc"]> {
  if (!docUri.startsWith("https://")) return null;
  try {
    const res = await fetch(docUri, { signal: AbortSignal.timeout(4000) });
    if (!res.ok || !res.body) return null;
    if (Number(res.headers.get("content-length")) > MAX_DOC_BYTES) return null;
    const reader = res.body.getReader();
    const chunks: Uint8Array[] = [];
    let total = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.length;
      if (total > MAX_DOC_BYTES) {
        await reader.cancel();
        return null;
      }
      chunks.push(value);
    }
    const d = JSON.parse(Buffer.concat(chunks).toString("utf8")) as Record<string, unknown>;
    const registrations = (d["registrations"] ?? []) as Array<Record<string, unknown>>;
    return {
      name: String(d["name"] ?? ""),
      image: typeof d["image"] === "string" ? d["image"] : null,
      description: typeof d["description"] === "string" ? d["description"] : null,
      listsThisAsset: registrations.some((r) => r["agentId"] === assetId),
    };
  } catch {
    return null;
  }
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
  const authorities = (das["authorities"] ?? []) as Array<Record<string, unknown>>;
  const grouping = (das["grouping"] ?? []) as Array<Record<string, unknown>>;
  const externalPlugins = (das["external_plugins"] ?? []) as Array<Record<string, unknown>>;

  const collection =
    (grouping.find((g) => g["group_key"] === "collection")?.["group_value"] as
      | string
      | undefined) ?? null;

  // The 014 Registry PDA and the registration-doc target derive synchronously
  // from the asset; their on-chain/https reads run together below.
  const [pda] = PublicKey.findProgramAddressSync(
    [Buffer.from("agent_identity"), assetPk.toBytes()],
    new PublicKey(AGENT_IDENTITY_PROGRAM),
  );

  const identityPlugin = externalPlugins.find((p) => p["type"] === "AgentIdentity");
  const registrationUri =
    ((identityPlugin?.["adapter_config"] as Record<string, unknown> | undefined)?.[
      "uri"
    ] as string | undefined) ?? null;

  // If the asset itself is a Covenant validation record (carries AppData), verify
  // it. Agents have no AppData on themselves; an agent's record lives on a
  // separate asset, surfaced via accountability below. Pure, no RPC.
  const attestation = appData(das) ? verifyAttestation(das, COVENANT_DATA_AUTHORITY) : null;

  const jsonUri = (content["json_uri"] as string | undefined) ?? "";
  const docUri = registrationUri ?? jsonUri;

  // Registry binding, registration doc, audit gate, and accountability are all
  // independent once the asset resolves. Read them concurrently. Each keeps its
  // own fallback so one slow or failed read never sinks the whole passport.
  const [registered, doc, gate, accountability] = await Promise.all([
    rpc("getAccountInfo", [pda.toBase58(), { encoding: "base64" }])
      .then(
        (info) =>
          (info as { value: { owner: string } | null } | null)?.value?.owner ===
          AGENT_IDENTITY_PROGRAM,
      )
      .catch(() => false),
    fetchDoc(docUri, assetPk.toBase58()),
    readAuditGate(rpc, externalPlugins, assetPk),
    findAccountability(rpc, assetPk.toBase58(), COVENANT_DATA_AUTHORITY).catch(
      () => null as Accountability | null,
    ),
  ]);

  const passport: AgentPassport = {
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
    doc,
    gate,
    accountability,
  };

  // Embeds the live gate verdict + accountability; never serve a stale verdict.
  return NextResponse.json(passport, {
    headers: { "Cache-Control": "no-store" },
  });
}
