import { afterEach, describe, expect, it, vi } from "vitest";
import { GET } from "../route";
import {
  AGENT_IDENTITY_PROGRAM,
  ATTESTATION_SCHEMA,
  COVENANT_COLLECTION,
  COVENANT_DATA_AUTHORITY,
  FEATURED_AGENT_ASSET,
} from "@/app/agents/_registry";

// GET /api/agents/[asset] is the server-side agent-passport verifier: it proves
// three independent on-chain facts about a Core asset — the asset itself (DAS
// getAsset), the 014 Registry binding (the agent_identity PDA owner), and the
// Covenant attestation AppData plus its write authority. These tests pin the
// route-level decisions: the address gate (400), the fail-closed DAS-failure
// surface (502), the Core-only interface gate (404), and on the happy path the
// authority trust check (covenantAuthored iff AppData authority is exactly the
// Covenant minting key), the casing-independent payload normalization, the
// registry-owner binding, and the document cross-listing — all driven against a
// stubbed RPC/HTTP boundary so no fact is asserted on the server's say-so.

const OTHER_ADDR = "11111111111111111111111111111111";
const REG_URI = "https://reg.example.test/agent.json";

type DasOverrides = Partial<{
  interface: unknown;
  appDataAuthority: string;
  appDataSchema: string;
}>;

function das(overrides: DasOverrides = {}): Record<string, unknown> {
  return {
    interface: "interface" in overrides ? overrides.interface : "MplCoreAsset",
    burnt: false,
    content: {
      metadata: { name: "Agent X" },
      json_uri: "https://json.example.test/asset.json",
    },
    ownership: { owner: "OWNER_ADDR" },
    authorities: [{ address: "AUTH_ADDR" }],
    grouping: [{ group_key: "collection", group_value: COVENANT_COLLECTION }],
    external_plugins: [
      { type: "AgentIdentity", adapter_config: { uri: REG_URI } },
      {
        type: "AppData",
        authority: { address: overrides.appDataAuthority ?? COVENANT_DATA_AUTHORITY },
        // Helius snake_cases AppData; the route must normalize it to camelCase.
        data: {
          schema: overrides.appDataSchema ?? ATTESTATION_SCHEMA,
          root_hash_hex: "ab12cd",
          release_target: "v0.3.0",
          release_subject: "covenant",
          release_scope: "audit-root",
          recorded_at: 42,
        },
      },
    ],
  };
}

function stubFetch(opts: {
  getAsset?: unknown;
  getAssetHttpOk?: boolean;
  accountOwner?: string | null;
  doc?: { ok: boolean; body?: unknown };
}) {
  const f = vi.fn(async (_url: string, init?: { method?: string; body?: unknown }) => {
    if (init?.method === "POST" && typeof init.body === "string") {
      const { method } = JSON.parse(init.body) as { method: string };
      if (method === "getAsset") {
        if (opts.getAssetHttpOk === false) return { ok: false, status: 502 };
        return { ok: true, json: async () => ({ result: opts.getAsset }) };
      }
      if (method === "getAccountInfo") {
        const value = opts.accountOwner === undefined ? null : { owner: opts.accountOwner };
        return { ok: true, json: async () => ({ result: { value } }) };
      }
    }
    return { ok: opts.doc?.ok ?? false, json: async () => opts.doc?.body ?? {} };
  });
  vi.stubGlobal("fetch", f);
  return f;
}

function call(asset: string) {
  return GET(new Request(`http://x/api/agents/${asset}`), {
    params: Promise.resolve({ asset }),
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("agent passport route", () => {
  it("400s an asset that is not a valid Solana address before any RPC", async () => {
    const f = stubFetch({});
    const res = await call("!!!not-base58!!!");
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "not a valid Solana address" });
    expect(f).not.toHaveBeenCalled();
  });

  it("502s fail-closed when the DAS lookup fails", async () => {
    stubFetch({ getAssetHttpOk: false });
    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({
      error: "asset lookup failed — DAS endpoint unavailable or asset not found",
    });
  });

  it("404s an asset whose interface is not MplCoreAsset", async () => {
    stubFetch({ getAsset: das({ interface: "V1_NFT" }) });
    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({
      error: "not an MPL Core asset — the 014 Registry binds Core assets only",
    });
  });

  it("404s when DAS returns no asset", async () => {
    stubFetch({ getAsset: null });
    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(404);
  });

  it("assembles a Covenant-authored passport with a normalized attestation", async () => {
    stubFetch({
      getAsset: das(),
      accountOwner: AGENT_IDENTITY_PROGRAM,
      doc: {
        ok: true,
        body: {
          name: "Agent X",
          image: "https://img.example.test/x.png",
          description: "a registered agent",
          registrations: [{ agentId: FEATURED_AGENT_ASSET }],
        },
      },
    });

    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(200);
    const body = await res.json();

    expect(body.asset.inCovenantCollection).toBe(true);
    expect(body.asset.collection).toBe(COVENANT_COLLECTION);
    expect(body.asset.burnt).toBe(false);

    expect(body.registry.registered).toBe(true);
    expect(body.registry.identityPlugin).toBe(true);
    expect(body.registry.registrationUri).toBe(REG_URI);

    expect(body.attestation.covenantAuthored).toBe(true);
    expect(body.attestation.authority).toBe(COVENANT_DATA_AUTHORITY);
    // snake_cased indexer keys must surface as the camelCase contract, coerced.
    expect(body.attestation.payload).toMatchObject({
      schema: ATTESTATION_SCHEMA,
      rootHashHex: "ab12cd",
      releaseTarget: "v0.3.0",
      releaseScope: "audit-root",
      recordedAt: 42,
    });

    expect(body.doc.listsThisAsset).toBe(true);
    expect(body.doc.image).toBe("https://img.example.test/x.png");
  });

  it("marks a non-Covenant AppData authority as not Covenant-authored and a mismatched PDA owner as unregistered", async () => {
    stubFetch({
      getAsset: das({ appDataAuthority: OTHER_ADDR }),
      accountOwner: OTHER_ADDR,
      doc: { ok: false },
    });

    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(200);
    const body = await res.json();

    // The attestation is still present (schema matched) but its authority is
    // not the minting key, so it must not be trusted as Covenant-authored.
    expect(body.attestation).not.toBeNull();
    expect(body.attestation.authority).toBe(OTHER_ADDR);
    expect(body.attestation.covenantAuthored).toBe(false);
    expect(body.registry.registered).toBe(false);
  });

  it("drops AppData whose schema is not the attestation schema", async () => {
    stubFetch({
      getAsset: das({ appDataSchema: "some.other.schema.v1" }),
      accountOwner: AGENT_IDENTITY_PROGRAM,
      doc: { ok: false },
    });

    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.attestation).toBeNull();
  });
});
