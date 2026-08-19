import fs from "node:fs";
import { Connection, PublicKey } from "@solana/web3.js";
import { loadKeypair, PROGRAM, configPda, creditsPda, readConfig, readCredits, ataFor } from "../settlement.mjs";

const MAINNET_GENESIS = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const EXPECT_SIGNER = "13vGvvgTz3EZZ6rgz4BsCV3BSfBz4V8LvBDjhV6GN5xQ";
const COVNT_MINT = new PublicKey("2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump");
const TOKEN_CLASSIC = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

const RPC = process.env.SOLANA_RPC;
const conn = new Connection(RPC, "confirmed");

const signer = loadKeypair(process.env.SIGNER);
const pk = signer.publicKey.toBase58();
console.log("signer pubkey        ", pk, pk === EXPECT_SIGNER ? "OK ✓" : "MISMATCH ✗");
if (pk !== EXPECT_SIGNER) process.exit(1);

const genesis = await conn.getGenesisHash();
console.log("genesis              ", genesis, genesis === MAINNET_GENESIS ? "MAINNET ✓" : "NOT MAINNET ✗");
if (genesis !== MAINNET_GENESIS) process.exit(1);

const sol = await conn.getBalance(signer.publicKey);
console.log("task-key SOL         ", (sol / 1e9).toFixed(6), "SOL", `(${sol} lamports)`);

const mintInfo = await conn.getAccountInfo(COVNT_MINT);
const mintOwner = mintInfo.owner.toBase58();
const tokenProgram = mintOwner === TOKEN_2022 ? "TOKEN-2022" : mintOwner === TOKEN_CLASSIC ? "CLASSIC-SPL" : "UNKNOWN";
console.log("COVNT mint owner prog", mintOwner, `= ${tokenProgram}`);

const ata = ataFor(signer.publicKey, COVNT_MINT);
console.log("task-key COVNT ATA   ", ata.toBase58());
const covnt = await conn.getTokenAccountBalance(ata).then((r) => r.value).catch((e) => ({ err: e.message }));
console.log("task-key COVNT bal   ", JSON.stringify(covnt));

console.log("program              ", PROGRAM.toBase58());
const cfgAi = await conn.getAccountInfo(configPda());
console.log("config PDA           ", configPda().toBase58(), cfgAi ? `EXISTS (${cfgAi.data.length}b, owner ${cfgAi.owner.toBase58()})` : "MISSING");
if (cfgAi) {
  const cfg = await readConfig(conn);
  console.log("  config.authority   ", cfg.authority.toBase58());
  console.log("  config.covntMint   ", cfg.covntMint.toBase58(), cfg.covntMint.equals(COVNT_MINT) ? "matches ✓" : "DIFFERS ✗");
  console.log("  config.treasury    ", cfg.treasury.toBase58());
  console.log("  creditsPerCovnt    ", cfg.creditsPerCovnt);
  console.log("  paused             ", cfg.paused);
}

const credPda = creditsPda(signer.publicKey);
console.log("credits PDA          ", credPda.toBase58());
const cred = await readCredits(conn, signer.publicKey);
console.log("credits account      ", cred ? JSON.stringify({ delegated: cred.delegated, balance: cred.balance?.toString(), root: cred.provenanceRoot?.toString("hex") }) : "MISSING (fresh)");

console.log("\n=== ER ROUTER ===");
try {
  const res = await fetch("https://router.magicblock.app", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getRoutes", params: [] }),
  });
  const j = await res.json();
  console.log("getRoutes raw:", JSON.stringify(j).slice(0, 2000));
} catch (e) {
  console.log("router error:", e.message);
}
