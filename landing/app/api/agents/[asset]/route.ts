// /api/agents/[asset] — server-side verification for the agent passport.
//
// Three independent facts, each checked against the chain this request:
//   1. the Core asset itself (DAS getAsset),
//   2. the 014 Registry binding (the ["agent_identity", asset] PDA under
//      the registry program — derived here, fetched over plain RPC),
//   3. the Covenant attestation AppData and its on-chain write authority.
// The witness-chain recomputation deliberately does NOT happen here: the
// browser fetches the published event chain and recomputes the root
// itself, so the proof never depends on this server saying so.

import { NextResponse } from "next/server";
import { PublicKey } from "@solana/web3.js";
import {
  AGENT_IDENTITY_PROGRAM,
  ATTESTATION_SCHEMA,
  COVENANT_COLLECTION,
  COVENANT_DATA_AUTHORITY,
} from "@/app/agents/_registry";
import { readAuditGate, type AuditGate } from "@/app/agents/_gate";
import { findAccountability, type Accountability } from "@/app/agents/_attest";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

type AttestationPayload = {
  schema: string;
  rootHashHex: string;
  releaseTarget: string;
  releaseSubject: string;
  releaseScope: string;
  recordedAt: number;
};

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
  attestation: {
    payload: AttestationPayload;
    /** AppData write authority recorded on-chain. */
    authority: string | null;
    /** authority === the Covenant minting key. */
    covenantAuthored: boolean;
  } | null;
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

function rpcUrl(): string {
  return (
    process.env.COVENANT_SOLANA_MAINNET_RPC_URL ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ||
    "https://api.mainnet-beta.solana.com"
  );
}

async function rpc(method: string, params: unknown): Promise<unknown> {
  const res = await fetch(rpcUrl(), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
    signal: AbortSignal.timeout(8000),
  });
  if (!res.ok) throw new Error(`rpc ${method} http ${res.status}`);
  const body = (await res.json()) as { result?: unknown; error?: { message?: string } };
  if (body.error) throw new Error(`rpc ${method}: ${body.error.message ?? "error"}`);
  return body.result;
}

// Helius indexes the AppData JSON with snake_cased keys; the on-chain bytes
// are camelCase. Accept either so the passport never depends on an
// indexer's casing choice.
function normalizePayload(raw: Record<string, unknown>): AttestationPayload | null {
  const pick = (camel: string, snake: string): unknown => raw[camel] ?? raw[snake];
  const schema = pick("schema", "schema");
  const root = pick("rootHashHex", "root_hash_hex");
  if (typeof schema !== "string" || typeof root !== "string") return null;
  return {
    schema,
    rootHashHex: root,
    releaseTarget: String(pick("releaseTarget", "release_target") ?? ""),
    releaseSubject: String(pick("releaseSubject", "release_subject") ?? ""),
    releaseScope: String(pick("releaseScope", "release_scope") ?? ""),
    recordedAt: Number(pick("recordedAt", "recorded_at") ?? 0),
  };
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
    if (/not found/i.test(String(e))) {
      return NextResponse.json({ error: "no asset at this address" }, { status: 404 });
    }
    return NextResponse.json({ error: "asset lookup failed — DAS endpoint unavailable" }, { status: 502 });
  }
  if (!das || das["interface"] !== "MplCoreAsset") {
    return NextResponse.json(
      { error: "not an MPL Core asset — the 014 Registry binds Core assets only" },
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

  // 2. The 014 Registry binding.
  const [pda] = PublicKey.findProgramAddressSync(
    [Buffer.from("agent_identity"), assetPk.toBytes()],
    new PublicKey(AGENT_IDENTITY_PROGRAM),
  );
  let registered = false;
  try {
    const info = (await rpc("getAccountInfo", [
      pda.toBase58(),
      { encoding: "base64" },
    ])) as { value: { owner: string } | null } | null;
    registered = info?.value?.owner === AGENT_IDENTITY_PROGRAM;
  } catch {
    // leave registered=false; the page renders the gray "unknown" state
  }

  const identityPlugin = externalPlugins.find((p) => p["type"] === "AgentIdentity");
  const registrationUri =
    ((identityPlugin?.["adapter_config"] as Record<string, unknown> | undefined)?.[
      "uri"
    ] as string | undefined) ?? null;

  // 3. The Covenant attestation, when the asset carries AppData.
  const appData = externalPlugins.find((p) => p["type"] === "AppData");
  let attestation: AgentPassport["attestation"] = null;
  if (appData) {
    const payload = normalizePayload((appData["data"] ?? {}) as Record<string, unknown>);
    if (payload && payload.schema === ATTESTATION_SCHEMA) {
      const authority =
        ((appData["authority"] as Record<string, unknown> | undefined)?.["address"] as
          | string
          | undefined) ?? null;
      attestation = {
        payload,
        authority,
        covenantAuthored: authority === COVENANT_DATA_AUTHORITY,
      };
    }
  }

  // 4. The registration document, when the asset points at one over https.
  const jsonUri = (content["json_uri"] as string | undefined) ?? "";
  const docUri = registrationUri ?? jsonUri;
  let doc: AgentPassport["doc"] = null;
  if (docUri.startsWith("https://")) {
    try {
      const res = await fetch(docUri, { signal: AbortSignal.timeout(4000) });
      if (res.ok) {
        const d = (await res.json()) as Record<string, unknown>;
        const registrations = (d["registrations"] ?? []) as Array<Record<string, unknown>>;
        doc = {
          name: String(d["name"] ?? ""),
          image: typeof d["image"] === "string" ? d["image"] : null,
          description: typeof d["description"] === "string" ? d["description"] : null,
          listsThisAsset: registrations.some((r) => r["agentId"] === assetPk.toBase58()),
        };
      }
    } catch {
      // doc unreachable — rendered as a yellow state, not an error
    }
  }

  // 5. The Covenant audit gate (Core Oracle plugin → covenant-oracle program).
  const gate = await readAuditGate(rpc, externalPlugins, assetPk);

  // 6. Accountability: validation records that name this asset as subject (the
  // agent's record lives on a separate asset, not as AppData on the agent).
  let accountability: Accountability | null = null;
  try {
    accountability = await findAccountability(rpc, assetPk.toBase58(), COVENANT_DATA_AUTHORITY);
  } catch {
    // leave null — the page renders accountability as an "unknown" state
  }

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

  return NextResponse.json(passport, {
    headers: { "Cache-Control": "public, s-maxage=30, stale-while-revalidate=120" },
  });
}
