import { afterEach, describe, expect, it, vi } from "vitest";
import { normalizePayload, rpcUrl } from "../_passport";

const SERVER = "https://server.example/rpc";
const PUBLIC_MAINNET = "https://public-mainnet.example/rpc";
const PUBLIC_GENERIC = "https://public-generic.example/rpc";
const DEFAULT = "https://api.mainnet-beta.solana.com";

describe("rpcUrl", () => {
  afterEach(() => vi.unstubAllEnvs());

  function env(server: string, publicMainnet: string, publicGeneric: string) {
    vi.stubEnv("COVENANT_SOLANA_MAINNET_RPC_URL", server);
    vi.stubEnv("NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL", publicMainnet);
    vi.stubEnv("NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL", publicGeneric);
  }

  it("falls back to the public mainnet-beta default when no RPC env is set", () => {
    env("", "", "");
    expect(rpcUrl()).toBe(DEFAULT);
  });

  it("prefers the server mainnet RPC over every public override", () => {
    env(SERVER, PUBLIC_MAINNET, PUBLIC_GENERIC);
    expect(rpcUrl()).toBe(SERVER);
  });

  it("prefers the public mainnet RPC over the generic public RPC", () => {
    env("", PUBLIC_MAINNET, PUBLIC_GENERIC);
    expect(rpcUrl()).toBe(PUBLIC_MAINNET);
  });

  it("uses the generic public RPC when it is the only override", () => {
    env("", "", PUBLIC_GENERIC);
    expect(rpcUrl()).toBe(PUBLIC_GENERIC);
  });
});

const NORMALIZED = {
  schema: "covenant.attestation.v1",
  rootHashHex: "ab12cd",
  releaseTarget: "v0.3.0",
  releaseSubject: "covenant",
  releaseScope: "audit-root",
  recordedAt: 1719600000,
};

const CAMEL = { ...NORMALIZED };
const SNAKE = {
  schema: "covenant.attestation.v1",
  root_hash_hex: "ab12cd",
  release_target: "v0.3.0",
  release_subject: "covenant",
  release_scope: "audit-root",
  recorded_at: 1719600000,
};

describe("normalizePayload", () => {
  it("accepts the on-chain camelCase keys", () => {
    expect(normalizePayload(CAMEL)).toEqual(NORMALIZED);
  });

  it("accepts the indexer's snake_case keys", () => {
    expect(normalizePayload(SNAKE)).toEqual(NORMALIZED);
  });

  it("prefers the camelCase value when both casings are present", () => {
    const mixed = { ...SNAKE, releaseTarget: "camel-wins", release_target: "snake-loses" };
    expect(normalizePayload(mixed)?.releaseTarget).toBe("camel-wins");
  });

  it("fails closed to null when schema is missing or non-string", () => {
    const { schema: _schema, ...noSchema } = CAMEL;
    expect(normalizePayload(noSchema)).toBeNull();
    expect(normalizePayload({ ...CAMEL, schema: 123 })).toBeNull();
  });

  it("fails closed to null when the root hash is missing or non-string", () => {
    const { rootHashHex: _root, ...noRoot } = CAMEL;
    expect(normalizePayload(noRoot)).toBeNull();
    expect(normalizePayload({ ...CAMEL, rootHashHex: 5 })).toBeNull();
  });

  it("defaults optional fields to empty and coerces recordedAt to a number", () => {
    expect(normalizePayload({ schema: "s", rootHashHex: "r" })).toEqual({
      schema: "s",
      rootHashHex: "r",
      releaseTarget: "",
      releaseSubject: "",
      releaseScope: "",
      recordedAt: 0,
    });
    expect(
      normalizePayload({ schema: "s", rootHashHex: "r", recordedAt: "42" })?.recordedAt,
    ).toBe(42);
  });
});
