import { afterEach, describe, expect, it, vi } from "vitest";
import { PublicKey } from "@solana/web3.js";
import { GET } from "../route";
import {
  AGENT_IDENTITY_PROGRAM,
  ATTESTATION_HASH_ALG,
  ATTESTATION_SCHEMA_V2,
  ATTESTATION_TYPE,
  COVENANT_COLLECTION,
  COVENANT_DATA_AUTHORITY,
  FEATURED_AGENT_ASSET,
} from "@/app/agents/_registry";

// GET /api/agents/[asset] returns bounded agent-passport observations: the asset (DAS
// getAsset), the 014 Registry binding (the agent_identity PDA owner), and the
// DAS-reported Covenant record envelope plus its reported write authority. These tests pin the
// route-level decisions: the address gate (400), the fail-closed DAS-failure
// surface (502), the Core-only interface gate (404), and on the happy path the
// structural envelope check, casing-independent fields, and the registry-owner
// binding. Registration-document URIs are never fetched by the
// server because they are controlled by on-chain input.

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
        adapter_config: {
          data_authority: {
            address: overrides.appDataAuthority ?? COVENANT_DATA_AUTHORITY,
          },
        },
        // Helius snake_cases AppData; the route must normalize it to camelCase.
        data: {
          type: ATTESTATION_TYPE,
          schema: overrides.appDataSchema ?? ATTESTATION_SCHEMA_V2,
          hash_alg: ATTESTATION_HASH_ALG,
          response_hash: "a".repeat(64),
          validator: COVENANT_DATA_AUTHORITY,
          subject: { registry: "mpl-agent-014", asset: FEATURED_AGENT_ASSET },
          tag: "audit",
          covenant: {
            release_target: "covenant",
            release_subject: "witness-loop",
            release_scope: "audit",
          },
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
  accountAsset?: string;
  accountLookupFails?: boolean;
}) {
  const f = vi.fn(
    async (_url: string, init?: { method?: string; body?: unknown }) => {
      if (init?.method === "POST" && typeof init.body === "string") {
        const { method } = JSON.parse(init.body) as { method: string };
        if (method === "getAsset") {
          if (opts.getAssetHttpOk === false) return { ok: false, status: 502 };
          return { ok: true, json: async () => ({ result: opts.getAsset }) };
        }
        if (method === "getAccountInfo") {
          if (opts.accountLookupFails) return { ok: false, status: 503 };
          const state = Buffer.concat([
            Buffer.alloc(8),
            new PublicKey(opts.accountAsset ?? FEATURED_AGENT_ASSET).toBuffer(),
          ]).toString("base64");
          const value =
            opts.accountOwner === undefined
              ? null
              : { owner: opts.accountOwner, data: [state, "base64"] };
          return { ok: true, json: async () => ({ result: { value } }) };
        }
      }
      throw new Error("unexpected non-RPC fetch");
    },
  );
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
      error: "asset lookup failed: DAS endpoint unavailable",
    });
  });

  it("404s an asset whose interface is not MplCoreAsset", async () => {
    stubFetch({ getAsset: das({ interface: "V1_NFT" }) });
    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({
      error: "not an MPL Core asset: the 014 Registry binds Core assets only",
    });
  });

  it("404s when DAS returns no asset", async () => {
    stubFetch({ getAsset: null });
    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(404);
  });

  it("assembles a matching record observation without fetching the registration URI", async () => {
    stubFetch({
      getAsset: das(),
      accountOwner: AGENT_IDENTITY_PROGRAM,
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

    expect(body.attestation.matchesExpectedEnvelope).toBe(true);
    expect(body.attestation.evidenceSource).toBe("configured_das");
    expect(body.attestation.authority).toBe(COVENANT_DATA_AUTHORITY);
    expect(body.attestation.responseHash).toBe("a".repeat(64));
    expect(body.attestation.recordedAt).toBe(42);

    expect(body.doc).toBeNull();
  });

  it("rejects a mismatched reported authority and PDA owner", async () => {
    stubFetch({
      getAsset: das({ appDataAuthority: OTHER_ADDR }),
      accountOwner: OTHER_ADDR,
    });

    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(200);
    const body = await res.json();

    expect(body.attestation).not.toBeNull();
    expect(body.attestation.authority).toBe(OTHER_ADDR);
    expect(body.attestation.matchesExpectedEnvelope).toBe(false);
    expect(body.attestation.reasons).toContain(
      `data authority ${OTHER_ADDR} is not the Covenant authority ${COVENANT_DATA_AUTHORITY}`,
    );
    expect(body.registry.registered).toBe(false);
  });

  it("does not accept registry state that names a different asset", async () => {
    stubFetch({
      getAsset: das(),
      accountOwner: AGENT_IDENTITY_PROGRAM,
      accountAsset: OTHER_ADDR,
    });

    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(200);
    expect((await res.json()).registry.registered).toBe(false);
  });

  it("reports an unknown registry state when the configured RPC fails", async () => {
    stubFetch({
      getAsset: das(),
      accountLookupFails: true,
    });

    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(200);
    expect((await res.json()).registry.registered).toBeNull();
  });

  it("reports AppData whose schema does not match the expected envelope", async () => {
    stubFetch({
      getAsset: das({ appDataSchema: "some.other.schema.v1" }),
      accountOwner: AGENT_IDENTITY_PROGRAM,
    });

    const res = await call(FEATURED_AGENT_ASSET);
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.attestation.matchesExpectedEnvelope).toBe(false);
    expect(body.attestation.reasons).toContain(
      `schema is not ${ATTESTATION_SCHEMA_V2}`,
    );
  });
});
