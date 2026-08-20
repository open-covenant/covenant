import { Connection } from "@solana/web3.js";
import { afterAll, beforeEach, describe, expect, it } from "vitest";
import {
  explorerAddressUrl,
  explorerTxUrl,
  getClusterConfig,
  getReadConnection,
} from "../env";

const KEY = "NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER";
const MAINNET_MINT = "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";
const DEVNET_MINT = "12zLnQiqHLosp4GpAG4b1ZyrcyHJK8863FiDcQZ5Drmd";
const TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const SPL_TOKEN = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

const original = process.env[KEY];
beforeEach(() => {
  delete process.env[KEY];
});
afterAll(() => {
  if (original === undefined) delete process.env[KEY];
  else process.env[KEY] = original;
});

describe("getClusterConfig", () => {
  it("defaults to mainnet when unset", () => {
    const cfg = getClusterConfig();
    expect(cfg.cluster).toBe("mainnet-beta");
    expect(cfg.explorerCluster).toBe("mainnet-beta");
    expect(cfg.cvntMint.toBase58()).toBe(MAINNET_MINT);
    expect(cfg.tokenProgramId.toBase58()).toBe(TOKEN_2022);
  });

  it("treats the mainnet-beta alias as mainnet", () => {
    process.env[KEY] = "mainnet-beta";
    expect(getClusterConfig().cluster).toBe("mainnet-beta");
    expect(getClusterConfig().cvntMint.toBase58()).toBe(MAINNET_MINT);
  });

  it("selects devnet config for devnet", () => {
    process.env[KEY] = "devnet";
    const cfg = getClusterConfig();
    expect(cfg.cluster).toBe("devnet");
    expect(cfg.explorerCluster).toBe("devnet");
    expect(cfg.cvntMint.toBase58()).toBe(DEVNET_MINT);
    expect(cfg.tokenProgramId.toBase58()).toBe(SPL_TOKEN);
  });

  it("lowercases the cluster selector", () => {
    process.env[KEY] = "DEVNET";
    expect(getClusterConfig().cluster).toBe("devnet");
  });

  it("falls back to devnet for an unknown cluster", () => {
    process.env[KEY] = "testnet";
    expect(getClusterConfig().cluster).toBe("devnet");
  });
});

describe("explorer URLs", () => {
  it("builds tx and address links stamped with the default cluster", () => {
    expect(explorerTxUrl("SIG123")).toBe("https://explorer.solana.com/tx/SIG123?cluster=mainnet-beta");
    expect(explorerAddressUrl("ADDR456")).toBe("https://explorer.solana.com/address/ADDR456?cluster=mainnet-beta");
  });

  it("stamps the devnet cluster when selected", () => {
    process.env[KEY] = "devnet";
    expect(explorerTxUrl("SIG123")).toBe("https://explorer.solana.com/tx/SIG123?cluster=devnet");
    expect(explorerAddressUrl("ADDR456")).toBe("https://explorer.solana.com/address/ADDR456?cluster=devnet");
  });
});

describe("getReadConnection", () => {
  it("returns a cached Connection singleton", () => {
    const a = getReadConnection();
    expect(a).toBeInstanceOf(Connection);
    expect(getReadConnection()).toBe(a);
  });
});
