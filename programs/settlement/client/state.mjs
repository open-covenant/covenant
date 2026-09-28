#!/usr/bin/env node
// Devnet exercise for the Covenant settlement program's pre-existing state.
//
// The settlement program is upgradeable and holds live balances, so the only
// honest way to clear an upgrade is to rehearse it: stand the previous binary
// up under a disposable program id, write real state through it, upgrade, and
// then read every account back out of its raw bytes and compare.
//
// Four subcommands, meant to be run in this order around the upgrade:
//
//   create                   open the protocol and write one of every
//                            account type the program owns, then snapshot
//   snapshot <label>         re-read the same addresses and write a snapshot
//   compare <a> <b>          byte-for-byte diff of two snapshots
//   exercise                 drive the pre-existing instructions again and
//                            check each one moved exactly what it should
//   pause                    pause the protocol, confirm a funding path is
//                            refused, unpause, confirm it works again
//
// A snapshot records each account's owner, lamports, length, raw bytes and a
// decode. `compare` fails on any difference in the bytes, which is the whole
// point: an upgrade that reorders a field, changes a seed or shifts a
// discriminator shows up here as a changed account rather than as a support
// ticket six weeks later.
//
// Usage:
//   PROGRAM=<program id> node state.mjs create
//   PROGRAM=<program id> node state.mjs snapshot after-upgrade
//   node state.mjs compare before-upgrade after-upgrade
//   PROGRAM=<program id> node state.mjs exercise

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import {
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  createMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
} from "@solana/spl-token";

// ---------------------------------------------------------------- constants

// The mainnet deployment. Named here only so this client can refuse to touch
// it: everything below writes state, and none of it belongs on mainnet.
const MAINNET_PROGRAM = "3dTtXH7rah8YyWTsAfSg6qC3iqrJXWGEhAx57uUHXZff";
const DEVNET_GENESIS = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

const RPC = process.env.RPC || "https://api.devnet.solana.com";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..", "..");
const KEYDIR = path.join(REPO, "scratchpad", "compute-mainnet");
const SNAPDIR = path.join(KEYDIR, "rehearsal");

// Fixed inputs, so a snapshot is reproducible and every figure below can be
// asserted against an exact expectation rather than against "whatever it was".
const CREDITS_PER_COVNT = 1_000n;
const MIN_STAKE_LOCK = 60n;
const COVNT_DECIMALS = 9;
const COVNT_SUPPLY = 1_000_000_000_000n; // 1000 COVNT
const BUY_COVNT = 5_000_000_000n; // 5 COVNT -> 5_000_000_000_000 credits
const CONSUME_CREDITS = 1_234_567_890n;
const STAKE_COVNT = 2_000_000_000n; // 2 COVNT
const STAKE_LOCK_SECS = 3_600n;
const BATCH_RECEIPTS = 7;

const tag = (s) => crypto.createHash("sha256").update(`covenant-upgrade-rehearsal:${s}`).digest();

// --------------------------------------------------------------- primitives

const disc = (name) => crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const accountDisc = (name) =>
  crypto.createHash("sha256").update(`account:${name}`).digest().subarray(0, 8);
const meta = (pubkey, isSigner, isWritable) => ({ pubkey, isSigner, isWritable });
const pda = (seeds, program) => PublicKey.findProgramAddressSync(seeds, program)[0];
const sol = (lamports) => `${(lamports / LAMPORTS_PER_SOL).toFixed(9)} SOL`;
const hex = (buf) => Buffer.from(buf).toString("hex");

function fail(message) {
  throw new Error(message);
}

function assertEq(label, actual, expected) {
  const a = typeof actual === "bigint" ? actual.toString() : String(actual);
  const e = typeof expected === "bigint" ? expected.toString() : String(expected);
  if (a !== e) fail(`${label}: got ${a}, expected ${e}`);
  console.log(`  ok  ${label} = ${a}`);
}

const TRANSIENT = /blockhash not found|node is behind|block height exceeded|timed out|502|503|429/i;

async function send(conn, ix, signers, opts = {}) {
  for (let attempt = 0; ; attempt++) {
    try {
      return await sendAndConfirmTransaction(conn, new Transaction().add(ix), signers, {
        commitment: "confirmed",
        ...opts,
      });
    } catch (e) {
      const text = [e?.message ?? "", ...(e?.logs ?? [])].join("\n");
      if (attempt >= 4 || /custom program error/i.test(text) || !TRANSIENT.test(text)) throw e;
      await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
    }
  }
}

function loadOrCreateKey(name) {
  fs.mkdirSync(KEYDIR, { recursive: true, mode: 0o700 });
  const file = path.join(KEYDIR, `${name}.json`);
  if (fs.existsSync(file)) {
    return Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(file, "utf8"))));
  }
  const kp = Keypair.generate();
  fs.writeFileSync(file, JSON.stringify(Array.from(kp.secretKey)), { mode: 0o600 });
  console.log(`  created ${name}.json (throwaway devnet key, gitignored)`);
  return kp;
}

// ------------------------------------------------------------------ decoders
//
// Hand-written rather than generated, on purpose. A decoder derived from the
// same IDL as the program would move when the program moves, and agreeing with
// itself is not evidence. These offsets are written down from the deployed
// layout and stay put.

const decodeConfig = (d) => ({
  discriminator: hex(d.subarray(0, 8)),
  authority: new PublicKey(d.subarray(8, 40)).toBase58(),
  slashAuthority: new PublicKey(d.subarray(40, 72)).toBase58(),
  covntMint: new PublicKey(d.subarray(72, 104)).toBase58(),
  treasury: new PublicKey(d.subarray(104, 136)).toBase58(),
  creditsPerCovnt: d.readBigUInt64LE(136).toString(),
  paused: d[144] === 1,
  bump: d[145],
  minStakeLock: d.readBigUInt64LE(146).toString(),
});

const decodeAgent = (d) => ({
  discriminator: hex(d.subarray(0, 8)),
  agentKey: hex(d.subarray(8, 40)),
  operator: new PublicKey(d.subarray(40, 72)).toBase58(),
  metadataHash: hex(d.subarray(72, 104)),
  capabilityHash: hex(d.subarray(104, 136)),
  stake: d.readBigUInt64LE(136).toString(),
  reputation: d.readBigUInt64LE(144).toString(),
  active: d[152] === 1,
  bump: d[153],
});

const decodeCreditAccount = (d) => ({
  discriminator: hex(d.subarray(0, 8)),
  owner: new PublicKey(d.subarray(8, 40)).toBase58(),
  balance: d.readBigUInt64LE(40).toString(),
  bump: d[48],
  provenanceRoot: hex(d.subarray(49, 81)),
});

const decodeStakePosition = (d) => ({
  discriminator: hex(d.subarray(0, 8)),
  agentKey: hex(d.subarray(8, 40)),
  owner: new PublicKey(d.subarray(40, 72)).toBase58(),
  amount: d.readBigUInt64LE(72).toString(),
  lockUntil: d.readBigUInt64LE(80).toString(),
  vault: new PublicKey(d.subarray(88, 120)).toBase58(),
  active: d[120] === 1,
  bump: d[121],
});

const decodeReceiptBatch = (d) => ({
  discriminator: hex(d.subarray(0, 8)),
  batchId: hex(d.subarray(8, 40)),
  authority: new PublicKey(d.subarray(40, 72)).toBase58(),
  merkleRoot: hex(d.subarray(72, 104)),
  receiptCount: d.readUInt32LE(104),
  createdAt: d.readBigInt64LE(108).toString(),
  bump: d[116],
});

// An SPL / Token-2022 account's amount sits at the same offset in both, and
// the base layout is what the escrow and treasury balances are read from.
const decodeTokenAccount = (d) => ({
  mint: new PublicKey(d.subarray(0, 32)).toBase58(),
  owner: new PublicKey(d.subarray(32, 64)).toBase58(),
  amount: d.readBigUInt64LE(64).toString(),
});

// The layout the deployed program was built against. `create` checks the
// on-chain length of every account it opens against this table, so a rehearsal
// that silently wrote a different shape fails at the point it happened.
const LAYOUT = {
  Config: { space: 8 + 146, decode: decodeConfig },
  Agent: { space: 8 + 146, decode: decodeAgent },
  CreditAccount: { space: 8 + 73, decode: decodeCreditAccount },
  StakePosition: { space: 8 + 114, decode: decodeStakePosition },
  ReceiptBatch: { space: 8 + 109, decode: decodeReceiptBatch },
  TokenAccount: { space: null, decode: decodeTokenAccount },
};

// ------------------------------------------------------------------- context

function programId() {
  const raw = process.env.PROGRAM;
  if (!raw) fail("set PROGRAM to the program id this run should drive");
  if (raw === MAINNET_PROGRAM) {
    fail(`refusing to run against the mainnet deployment ${MAINNET_PROGRAM}`);
  }
  return new PublicKey(raw);
}

async function requireDevnet(conn) {
  const genesis = await conn.getGenesisHash();
  if (genesis !== DEVNET_GENESIS) {
    fail(`${RPC} is not devnet (genesis ${genesis})`);
  }
}

const ledgerPath = () => path.join(SNAPDIR, "state-ledger.json");

function readLedger() {
  const file = ledgerPath();
  if (!fs.existsSync(file)) fail(`no ledger at ${file}; run \`create\` first`);
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

// --------------------------------------------------------------------- create

async function create() {
  const PROGRAM_ID = programId();
  const conn = new Connection(RPC, "confirmed");
  await requireDevnet(conn);

  console.log("Covenant settlement: writing pre-existing state on devnet\n");
  console.log(`  program   ${PROGRAM_ID.toBase58()}`);
  console.log(`  rpc       ${RPC}`);

  const authority = loadOrCreateKey("devnet-deployer");
  const owner = loadOrCreateKey("devnet-renter");
  const slasher = loadOrCreateKey("devnet-coordinator");
  console.log(`  authority ${authority.publicKey.toBase58()}`);
  console.log(`  owner     ${owner.publicKey.toBase58()}`);

  const ownerBalance = await conn.getBalance(owner.publicKey, "confirmed");
  if (ownerBalance < 0.2 * LAMPORTS_PER_SOL) {
    const need = Math.ceil(0.2 * LAMPORTS_PER_SOL) - ownerBalance;
    const sig = await send(
      conn,
      SystemProgram.transfer({
        fromPubkey: authority.publicKey,
        toPubkey: owner.publicKey,
        lamports: need,
      }),
      [authority],
    );
    console.log(`  topped the owner up by ${sol(need)}  ${sig}`);
  }

  // 1. COVNT stand-in. Token-2022, as the live mint is, so the program's
  //    TokenInterface paths are exercised the way mainnet exercises them.
  console.log("\n1. COVNT stand-in mint (Token-2022)");
  const covntMint = await createMint(
    conn,
    authority,
    authority.publicKey,
    null,
    COVNT_DECIMALS,
    undefined,
    { commitment: "confirmed" },
    TOKEN_2022_PROGRAM_ID,
  );
  const treasuryAta = await getOrCreateAssociatedTokenAccount(
    conn,
    authority,
    covntMint,
    authority.publicKey,
    false,
    "confirmed",
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  const ownerAta = await getOrCreateAssociatedTokenAccount(
    conn,
    authority,
    covntMint,
    owner.publicKey,
    false,
    "confirmed",
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  await mintTo(
    conn,
    authority,
    covntMint,
    ownerAta.address,
    authority,
    COVNT_SUPPLY,
    [],
    { commitment: "confirmed" },
    TOKEN_2022_PROGRAM_ID,
  );
  console.log(`  mint      ${covntMint.toBase58()} (${COVNT_DECIMALS} decimals)`);
  console.log(`  treasury  ${treasuryAta.address.toBase58()}`);
  console.log(`  owner ATA ${ownerAta.address.toBase58()}  balance ${COVNT_SUPPLY}`);

  // 2. initialize
  console.log("\n2. initialize");
  const config = pda([Buffer.from("config")], PROGRAM_ID);
  const initData = Buffer.alloc(8 + 32 + 8 + 8);
  disc("initialize").copy(initData, 0);
  slasher.publicKey.toBuffer().copy(initData, 8);
  initData.writeBigUInt64LE(CREDITS_PER_COVNT, 40);
  initData.writeBigUInt64LE(MIN_STAKE_LOCK, 48);
  const initSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(config, false, true),
        meta(authority.publicKey, true, true),
        meta(covntMint, false, false),
        meta(treasuryAta.address, false, false),
        meta(SystemProgram.programId, false, false),
      ],
      data: initData,
    }),
    [authority],
  );
  console.log(`  config    ${config.toBase58()}  ${initSig}`);

  // 3. register_agent
  console.log("\n3. register_agent");
  const agentKey = tag("agent");
  const agent = pda([Buffer.from("agent"), agentKey], PROGRAM_ID);
  const agentData = Buffer.concat([disc("register_agent"), agentKey, tag("metadata"), tag("capability")]);
  const agentSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(config, false, false),
        meta(agent, false, true),
        meta(authority.publicKey, true, true),
        meta(SystemProgram.programId, false, false),
      ],
      data: agentData,
    }),
    [authority],
  );
  console.log(`  agent     ${agent.toBase58()}  ${agentSig}`);

  // 4. open_credit_account
  console.log("\n4. open_credit_account");
  const credits = pda([Buffer.from("credits"), owner.publicKey.toBuffer()], PROGRAM_ID);
  const openSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(credits, false, true),
        meta(owner.publicKey, true, true),
        meta(SystemProgram.programId, false, false),
      ],
      data: disc("open_credit_account"),
    }),
    [owner],
  );
  console.log(`  credits   ${credits.toBase58()}  ${openSig}`);

  // 5. buy_credits
  console.log("\n5. buy_credits");
  const buyData = Buffer.alloc(16);
  disc("buy_credits").copy(buyData, 0);
  buyData.writeBigUInt64LE(BUY_COVNT, 8);
  const buySig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(config, false, false),
        meta(credits, false, true),
        meta(owner.publicKey, true, true),
        meta(ownerAta.address, false, true),
        meta(treasuryAta.address, false, true),
        meta(covntMint, false, false),
        meta(TOKEN_2022_PROGRAM_ID, false, false),
      ],
      data: buyData,
    }),
    [owner],
  );
  console.log(`  bought    ${BUY_COVNT} COVNT base units  ${buySig}`);

  // 6. consume_credits
  console.log("\n6. consume_credits");
  const receiptHash = tag("receipt-1");
  const consumeData = Buffer.concat([
    disc("consume_credits"),
    (() => {
      const b = Buffer.alloc(8);
      b.writeBigUInt64LE(CONSUME_CREDITS);
      return b;
    })(),
    receiptHash,
  ]);
  const consumeSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [meta(config, false, false), meta(credits, false, true), meta(owner.publicKey, true, false)],
      data: consumeData,
    }),
    [owner],
  );
  console.log(`  consumed  ${CONSUME_CREDITS}  ${consumeSig}`);

  // 7. stake
  console.log("\n7. stake");
  const position = pda(
    [Buffer.from("stake"), agentKey, owner.publicKey.toBuffer()],
    PROGRAM_ID,
  );
  const stakeVault = await getOrCreateAssociatedTokenAccount(
    conn,
    owner,
    covntMint,
    position,
    true,
    "confirmed",
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  const lockUntil = BigInt(Math.floor(Date.now() / 1000)) + STAKE_LOCK_SECS;
  const stakeData = Buffer.alloc(24);
  disc("stake").copy(stakeData, 0);
  stakeData.writeBigUInt64LE(STAKE_COVNT, 8);
  stakeData.writeBigUInt64LE(lockUntil, 16);
  const stakeSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(config, false, false),
        meta(agent, false, true),
        meta(position, false, true),
        meta(owner.publicKey, true, true),
        meta(ownerAta.address, false, true),
        meta(stakeVault.address, false, true),
        meta(covntMint, false, false),
        meta(TOKEN_2022_PROGRAM_ID, false, false),
        meta(SystemProgram.programId, false, false),
      ],
      data: stakeData,
    }),
    [owner],
  );
  console.log(`  position  ${position.toBase58()}  ${stakeSig}`);
  console.log(`  vault     ${stakeVault.address.toBase58()}  ${STAKE_COVNT} base units`);

  // 8. anchor_receipt_batch
  console.log("\n8. anchor_receipt_batch");
  const batchId = tag("batch-1");
  const batch = pda([Buffer.from("receipt_batch"), batchId], PROGRAM_ID);
  const batchData = Buffer.concat([
    disc("anchor_receipt_batch"),
    batchId,
    tag("merkle-root-1"),
    (() => {
      const b = Buffer.alloc(4);
      b.writeUInt32LE(BATCH_RECEIPTS);
      return b;
    })(),
  ]);
  const batchSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(config, false, false),
        meta(batch, false, true),
        meta(authority.publicKey, true, true),
        meta(SystemProgram.programId, false, false),
      ],
      data: batchData,
    }),
    [authority],
  );
  console.log(`  batch     ${batch.toBase58()}  ${batchSig}`);

  const ledger = {
    program: PROGRAM_ID.toBase58(),
    rpc: RPC,
    createdAt: new Date().toISOString(),
    parties: {
      authority: authority.publicKey.toBase58(),
      owner: owner.publicKey.toBase58(),
      slashAuthority: slasher.publicKey.toBase58(),
    },
    covntMint: covntMint.toBase58(),
    covntTokenProgram: TOKEN_2022_PROGRAM_ID.toBase58(),
    inputs: {
      creditsPerCovnt: CREDITS_PER_COVNT.toString(),
      minStakeLock: MIN_STAKE_LOCK.toString(),
      covntSupply: COVNT_SUPPLY.toString(),
      buyCovnt: BUY_COVNT.toString(),
      consumeCredits: CONSUME_CREDITS.toString(),
      stakeCovnt: STAKE_COVNT.toString(),
      lockUntil: lockUntil.toString(),
      batchReceipts: BATCH_RECEIPTS,
      agentKey: hex(agentKey),
      batchId: hex(batchId),
      receiptHash: hex(receiptHash),
    },
    signatures: {
      initialize: initSig,
      registerAgent: agentSig,
      openCreditAccount: openSig,
      buyCredits: buySig,
      consumeCredits: consumeSig,
      stake: stakeSig,
      anchorReceiptBatch: batchSig,
    },
    accounts: [
      { name: "config", type: "Config", address: config.toBase58() },
      { name: "agent", type: "Agent", address: agent.toBase58() },
      { name: "credits", type: "CreditAccount", address: credits.toBase58() },
      { name: "position", type: "StakePosition", address: position.toBase58() },
      { name: "batch", type: "ReceiptBatch", address: batch.toBase58() },
      { name: "treasury", type: "TokenAccount", address: treasuryAta.address.toBase58() },
      { name: "stakeVault", type: "TokenAccount", address: stakeVault.address.toBase58() },
      { name: "ownerCovnt", type: "TokenAccount", address: ownerAta.address.toBase58() },
    ],
  };
  writeJson(ledgerPath(), ledger);
  console.log(`\n  ledger written to scratchpad/compute-mainnet/rehearsal/state-ledger.json`);

  // The figures the program should be holding, checked before anything is
  // snapshotted. A rehearsal that starts from state nobody verified proves
  // nothing after the upgrade.
  console.log("\n9. what the program wrote");
  const snap = await takeSnapshot(conn, ledger, "before-upgrade");
  const view = Object.fromEntries(snap.accounts.map((a) => [a.name, a.decoded]));
  assertEq("config.credits_per_covnt", view.config.creditsPerCovnt, CREDITS_PER_COVNT);
  assertEq("config.min_stake_lock", view.config.minStakeLock, MIN_STAKE_LOCK);
  assertEq("config.covnt_mint", view.config.covntMint, covntMint.toBase58());
  assertEq("agent.stake", view.agent.stake, STAKE_COVNT);
  assertEq("agent.active", view.agent.active, true);
  assertEq(
    "credits.balance",
    view.credits.balance,
    BUY_COVNT * CREDITS_PER_COVNT - CONSUME_CREDITS,
  );
  assertEq(
    "credits.provenance_root",
    view.credits.provenanceRoot,
    hex(crypto.createHash("sha256").update(Buffer.concat([Buffer.alloc(32), receiptHash])).digest()),
  );
  assertEq("position.amount", view.position.amount, STAKE_COVNT);
  assertEq("position.active", view.position.active, true);
  assertEq("position.vault", view.position.vault, stakeVault.address.toBase58());
  assertEq("batch.receipt_count", view.batch.receiptCount, BATCH_RECEIPTS);
  assertEq("treasury holds the COVNT that bought credits", view.treasury.amount, BUY_COVNT);
  assertEq("stake vault holds the staked COVNT", view.stakeVault.amount, STAKE_COVNT);
  assertEq(
    "owner COVNT left",
    view.ownerCovnt.amount,
    COVNT_SUPPLY - BUY_COVNT - STAKE_COVNT,
  );
  for (const a of snap.accounts) {
    const expect = LAYOUT[a.type].space;
    if (expect !== null) assertEq(`${a.name} on-chain length`, a.dataLen, expect);
  }
  console.log(`\n  snapshot written to ${path.relative(REPO, snapshotPath("before-upgrade"))}`);
}

// ------------------------------------------------------------------ snapshot

const snapshotPath = (label) => path.join(SNAPDIR, `snapshot-${label}.json`);

async function takeSnapshot(conn, ledger, label) {
  const addresses = ledger.accounts.map((a) => new PublicKey(a.address));
  const infos = await conn.getMultipleAccountsInfo(addresses, "confirmed");
  const slot = await conn.getSlot("confirmed");
  const accounts = ledger.accounts.map((entry, i) => {
    const info = infos[i];
    if (!info) fail(`${entry.name} (${entry.address}) is missing on chain`);
    const data = Buffer.from(info.data);
    return {
      name: entry.name,
      type: entry.type,
      address: entry.address,
      owner: info.owner.toBase58(),
      lamports: info.lamports,
      dataLen: data.length,
      sha256: crypto.createHash("sha256").update(data).digest("hex"),
      base64: data.toString("base64"),
      decoded: LAYOUT[entry.type].decode(data),
    };
  });
  const snapshot = {
    label,
    takenAt: new Date().toISOString(),
    program: ledger.program,
    rpc: RPC,
    slot,
    accounts,
  };
  writeJson(snapshotPath(label), snapshot);
  return snapshot;
}

async function snapshot(label) {
  if (!label) fail("usage: state.mjs snapshot <label>");
  const conn = new Connection(RPC, "confirmed");
  await requireDevnet(conn);
  const ledger = readLedger();
  const snap = await takeSnapshot(conn, ledger, label);
  console.log(`snapshot "${label}" at slot ${snap.slot}\n`);
  for (const a of snap.accounts) {
    console.log(`  ${a.name.padEnd(12)} ${a.address}  ${String(a.dataLen).padStart(4)}B  ${a.sha256}`);
  }
  console.log(`\nwritten to ${path.relative(REPO, snapshotPath(label))}`);
}

// ------------------------------------------------------------------- compare

function compare(aLabel, bLabel) {
  if (!aLabel || !bLabel) fail("usage: state.mjs compare <label-a> <label-b>");
  const a = JSON.parse(fs.readFileSync(snapshotPath(aLabel), "utf8"));
  const b = JSON.parse(fs.readFileSync(snapshotPath(bLabel), "utf8"));
  console.log(`comparing "${a.label}" (slot ${a.slot}) against "${b.label}" (slot ${b.slot})\n`);

  const byName = new Map(b.accounts.map((x) => [x.name, x]));
  const problems = [];
  for (const before of a.accounts) {
    const after = byName.get(before.name);
    if (!after) {
      problems.push(`${before.name} is missing from "${b.label}"`);
      continue;
    }
    const diffs = [];
    if (before.owner !== after.owner) diffs.push(`owner ${before.owner} -> ${after.owner}`);
    if (before.dataLen !== after.dataLen) diffs.push(`length ${before.dataLen} -> ${after.dataLen}`);
    if (before.base64 !== after.base64) {
      const x = Buffer.from(before.base64, "base64");
      const y = Buffer.from(after.base64, "base64");
      const at = [];
      for (let i = 0; i < Math.min(x.length, y.length) && at.length < 8; i++) {
        if (x[i] !== y[i]) at.push(`byte ${i}: 0x${x[i].toString(16)} -> 0x${y[i].toString(16)}`);
      }
      diffs.push(`data changed (${at.join(", ")})`);
    }
    if (before.lamports !== after.lamports) {
      diffs.push(`lamports ${before.lamports} -> ${after.lamports}`);
    }
    if (diffs.length) {
      problems.push(`${before.name} (${before.address}): ${diffs.join("; ")}`);
      console.log(`  CHANGED  ${before.name.padEnd(12)} ${diffs.join("; ")}`);
    } else {
      console.log(`  same     ${before.name.padEnd(12)} ${before.address}  ${before.sha256}`);
    }
  }

  if (problems.length) {
    console.log(`\n${problems.length} account(s) changed:`);
    for (const p of problems) console.log(`  - ${p}`);
    process.exitCode = 1;
    fail("state did not survive");
  }
  console.log(`\nall ${a.accounts.length} accounts are byte-identical, same owner, same lamports`);
}

// ------------------------------------------------------------------ exercise

// Re-run the pre-existing instructions against whatever binary is deployed now
// and check each one moved exactly what it should. Deliberately non-destructive:
// every account that existed before is still there afterwards, so a snapshot
// taken before this runs stays comparable.
async function exercise() {
  const PROGRAM_ID = programId();
  const conn = new Connection(RPC, "confirmed");
  await requireDevnet(conn);
  const ledger = readLedger();
  if (ledger.program !== PROGRAM_ID.toBase58()) {
    fail(`ledger was written against ${ledger.program}, not ${PROGRAM_ID.toBase58()}`);
  }

  const authority = loadOrCreateKey("devnet-deployer");
  const owner = loadOrCreateKey("devnet-renter");
  const addr = Object.fromEntries(ledger.accounts.map((a) => [a.name, new PublicKey(a.address)]));
  const covntMint = new PublicKey(ledger.covntMint);
  const agentKey = Buffer.from(ledger.inputs.agentKey, "hex");

  const read = async (name) => {
    const info = await conn.getAccountInfo(addr[name], "confirmed");
    if (!info) fail(`${name} is missing`);
    const entry = ledger.accounts.find((a) => a.name === name);
    return LAYOUT[entry.type].decode(Buffer.from(info.data));
  };

  console.log("pre-existing instructions, against the binary deployed now\n");
  console.log(`  program   ${PROGRAM_ID.toBase58()}`);

  // consume_credits: reads the balance and the provenance root at the offsets
  // the new binary believes they live at, and writes both back.
  console.log("\n1. consume_credits");
  const before = await read("credits");
  const amount = 10_000_000n;
  const receiptHash = tag(`exercise-${Date.now()}`);
  const data = Buffer.concat([
    disc("consume_credits"),
    (() => {
      const b = Buffer.alloc(8);
      b.writeBigUInt64LE(amount);
      return b;
    })(),
    receiptHash,
  ]);
  const consumeSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(addr.config, false, false),
        meta(addr.credits, false, true),
        meta(owner.publicKey, true, false),
      ],
      data,
    }),
    [owner],
  );
  console.log(`  tx        ${consumeSig}`);
  const afterConsume = await read("credits");
  assertEq("balance fell by exactly the amount consumed", afterConsume.balance, BigInt(before.balance) - amount);
  assertEq(
    "provenance root extended by exactly one receipt",
    afterConsume.provenanceRoot,
    hex(
      crypto
        .createHash("sha256")
        .update(Buffer.concat([Buffer.from(before.provenanceRoot, "hex"), receiptHash]))
        .digest(),
    ),
  );
  assertEq("owner unchanged", afterConsume.owner, before.owner);
  assertEq("bump unchanged", afterConsume.bump, before.bump);

  // buy_credits: a Token-2022 transfer_checked plus Config.credits_per_covnt.
  console.log("\n2. buy_credits");
  const treasuryBefore = await read("treasury");
  const buy = 1_000_000_000n;
  const buyData = Buffer.alloc(16);
  disc("buy_credits").copy(buyData, 0);
  buyData.writeBigUInt64LE(buy, 8);
  const buySig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(addr.config, false, false),
        meta(addr.credits, false, true),
        meta(owner.publicKey, true, true),
        meta(addr.ownerCovnt, false, true),
        meta(addr.treasury, false, true),
        meta(covntMint, false, false),
        meta(TOKEN_2022_PROGRAM_ID, false, false),
      ],
      data: buyData,
    }),
    [owner],
  );
  console.log(`  tx        ${buySig}`);
  const afterBuy = await read("credits");
  assertEq(
    "credits rose by amount x credits_per_covnt",
    afterBuy.balance,
    BigInt(afterConsume.balance) + buy * CREDITS_PER_COVNT,
  );
  assertEq(
    "treasury received the COVNT",
    (await read("treasury")).amount,
    BigInt(treasuryBefore.amount) + buy,
  );

  // set_agent_active: writes a single byte deep inside Agent, then puts it
  // back. If the layout had shifted, this would land on the wrong field.
  console.log("\n3. set_agent_active, off and on again");
  const setActive = async (active) =>
    send(
      conn,
      new TransactionInstruction({
        programId: PROGRAM_ID,
        keys: [
          meta(addr.config, false, false),
          meta(authority.publicKey, true, false),
          meta(addr.agent, false, true),
        ],
        data: Buffer.concat([disc("set_agent_active"), Buffer.from([active ? 1 : 0])]),
      }),
      [authority],
    );
  const agentBefore = await read("agent");
  const offSig = await setActive(false);
  const agentOff = await read("agent");
  assertEq("agent deactivated", agentOff.active, false);
  assertEq("agent stake untouched", agentOff.stake, agentBefore.stake);
  assertEq("agent key untouched", agentOff.agentKey, agentBefore.agentKey);
  const onSig = await setActive(true);
  assertEq("agent reactivated", (await read("agent")).active, true);
  console.log(`  tx        ${offSig} / ${onSig}`);

  // anchor_receipt_batch: opens a second account of a pre-existing type under
  // a pre-existing seed prefix, which is the check that the seeds still derive
  // where they did.
  console.log("\n4. anchor_receipt_batch (a second batch)");
  const batchId = tag(`batch-after-upgrade-${Date.now()}`);
  const batch = pda([Buffer.from("receipt_batch"), batchId], PROGRAM_ID);
  const batchSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(addr.config, false, false),
        meta(batch, false, true),
        meta(authority.publicKey, true, true),
        meta(SystemProgram.programId, false, false),
      ],
      data: Buffer.concat([
        disc("anchor_receipt_batch"),
        batchId,
        tag("merkle-root-2"),
        (() => {
          const b = Buffer.alloc(4);
          b.writeUInt32LE(11);
          return b;
        })(),
      ]),
    }),
    [authority],
  );
  const newBatch = await conn.getAccountInfo(batch, "confirmed");
  console.log(`  batch     ${batch.toBase58()}  ${batchSig}`);
  assertEq("new batch length matches the deployed layout", newBatch.data.length, LAYOUT.ReceiptBatch.space);
  assertEq(
    "new batch discriminator matches the deployed one",
    hex(newBatch.data.subarray(0, 8)),
    hex(accountDisc("ReceiptBatch")),
  );
  assertEq("new batch receipt_count", decodeReceiptBatch(Buffer.from(newBatch.data)).receiptCount, 11);

  // stake against the same agent from a second owner: proves the `stake` seed
  // prefix and the StakePosition layout still work for a fresh account.
  console.log("\n5. stake, from a second owner");
  const second = loadOrCreateKey("devnet-operator");
  const secondBalance = await conn.getBalance(second.publicKey, "confirmed");
  if (secondBalance < 0.05 * LAMPORTS_PER_SOL) {
    await send(
      conn,
      SystemProgram.transfer({
        fromPubkey: authority.publicKey,
        toPubkey: second.publicKey,
        lamports: Math.ceil(0.05 * LAMPORTS_PER_SOL) - secondBalance,
      }),
      [authority],
    );
  }
  const secondAta = await getOrCreateAssociatedTokenAccount(
    conn,
    authority,
    covntMint,
    second.publicKey,
    false,
    "confirmed",
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  const secondStake = 750_000_000n;
  await mintTo(
    conn,
    authority,
    covntMint,
    secondAta.address,
    authority,
    secondStake,
    [],
    { commitment: "confirmed" },
    TOKEN_2022_PROGRAM_ID,
  );
  const secondPosition = pda(
    [Buffer.from("stake"), agentKey, second.publicKey.toBuffer()],
    PROGRAM_ID,
  );
  const secondVault = await getOrCreateAssociatedTokenAccount(
    conn,
    authority,
    covntMint,
    secondPosition,
    true,
    "confirmed",
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  const lockUntil = BigInt(Math.floor(Date.now() / 1000)) + STAKE_LOCK_SECS;
  const stakeData = Buffer.alloc(24);
  disc("stake").copy(stakeData, 0);
  stakeData.writeBigUInt64LE(secondStake, 8);
  stakeData.writeBigUInt64LE(lockUntil, 16);
  const agentStakeBefore = BigInt((await read("agent")).stake);
  const stakeSig = await send(
    conn,
    new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(addr.config, false, false),
        meta(addr.agent, false, true),
        meta(secondPosition, false, true),
        meta(second.publicKey, true, true),
        meta(secondAta.address, false, true),
        meta(secondVault.address, false, true),
        meta(covntMint, false, false),
        meta(TOKEN_2022_PROGRAM_ID, false, false),
        meta(SystemProgram.programId, false, false),
      ],
      data: stakeData,
    }),
    [second],
  );
  console.log(`  position  ${secondPosition.toBase58()}  ${stakeSig}`);
  const positionInfo = await conn.getAccountInfo(secondPosition, "confirmed");
  assertEq("second position length", positionInfo.data.length, LAYOUT.StakePosition.space);
  assertEq(
    "second position discriminator",
    hex(positionInfo.data.subarray(0, 8)),
    hex(accountDisc("StakePosition")),
  );
  assertEq("second position amount", decodeStakePosition(Buffer.from(positionInfo.data)).amount, secondStake);
  assertEq("agent stake accumulated", (await read("agent")).stake, agentStakeBefore + secondStake);

  // The original position is untouched by all of the above.
  const position = await read("position");
  assertEq("the first position still holds its stake", position.amount, ledger.inputs.stakeCovnt);
  assertEq("the first position is still active", position.active, true);

  writeJson(path.join(SNAPDIR, "exercise-result.json"), {
    ranAt: new Date().toISOString(),
    program: PROGRAM_ID.toBase58(),
    signatures: {
      consumeCredits: consumeSig,
      buyCredits: buySig,
      setAgentInactive: offSig,
      setAgentActive: onSig,
      anchorReceiptBatch: batchSig,
      stake: stakeSig,
    },
    secondPosition: secondPosition.toBase58(),
    secondBatch: batch.toBase58(),
    creditsAfter: (await read("credits")).balance,
  });
  console.log("\n  all pre-existing instructions behaved as they did before");
}

// --------------------------------------------------------------------- pause

const ERR_PROTOCOL_PAUSED = 6002;

// The pause lives at a single byte inside Config, two bytes ahead of the field
// that was appended last. Toggling it and watching a funding path close and
// reopen is the cheapest on-chain evidence that the byte the new binary writes
// is the byte the old one read, and that the appended tail came through intact.
async function pause() {
  const PROGRAM_ID = programId();
  const conn = new Connection(RPC, "confirmed");
  await requireDevnet(conn);
  const ledger = readLedger();
  const authority = loadOrCreateKey("devnet-deployer");
  const owner = loadOrCreateKey("devnet-renter");
  const addr = Object.fromEntries(ledger.accounts.map((a) => [a.name, new PublicKey(a.address)]));
  const covntMint = new PublicKey(ledger.covntMint);

  const readConfig = async () =>
    decodeConfig(Buffer.from((await conn.getAccountInfo(addr.config, "confirmed")).data));

  const setPause = (paused) =>
    send(
      conn,
      new TransactionInstruction({
        programId: PROGRAM_ID,
        keys: [meta(addr.config, false, true), meta(authority.publicKey, true, false)],
        data: Buffer.concat([disc("set_pause"), Buffer.from([paused ? 1 : 0])]),
      }),
      [authority],
    );

  const buy = (amount) => {
    const data = Buffer.alloc(16);
    disc("buy_credits").copy(data, 0);
    data.writeBigUInt64LE(amount, 8);
    return send(
      conn,
      new TransactionInstruction({
        programId: PROGRAM_ID,
        keys: [
          meta(addr.config, false, false),
          meta(addr.credits, false, true),
          meta(owner.publicKey, true, true),
          meta(addr.ownerCovnt, false, true),
          meta(addr.treasury, false, true),
          meta(covntMint, false, false),
          meta(TOKEN_2022_PROGRAM_ID, false, false),
        ],
        data,
      }),
      [owner],
    );
  };

  console.log("the protocol pause, against the binary deployed now\n");
  const before = await readConfig();
  assertEq("starts unpaused", before.paused, false);

  const pauseSig = await setPause(true);
  const paused = await readConfig();
  assertEq("paused", paused.paused, true);
  assertEq("authority untouched", paused.authority, before.authority);
  assertEq("credits_per_covnt untouched", paused.creditsPerCovnt, before.creditsPerCovnt);
  assertEq("the appended min_stake_lock untouched", paused.minStakeLock, before.minStakeLock);
  console.log(`  tx        ${pauseSig}`);

  let refused = null;
  try {
    await buy(1_000_000n);
  } catch (e) {
    const hay = [e?.message ?? "", ...(e?.logs ?? [])].join("\n");
    const m = hay.match(/custom program error: 0x([0-9a-fA-F]+)/);
    refused = m ? Number.parseInt(m[1], 16) : null;
  }
  assertEq("buy_credits refused while paused", refused, ERR_PROTOCOL_PAUSED);

  const unpauseSig = await setPause(false);
  const unpaused = await readConfig();
  assertEq("unpaused", unpaused.paused, false);
  assertEq("the appended min_stake_lock still untouched", unpaused.minStakeLock, before.minStakeLock);
  const buySig = await buy(1_000_000n);
  console.log(`  tx        ${unpauseSig} / ${buySig}`);
  console.log("\n  the pause closes and reopens the funding path, and moves nothing else in Config");
}

// ---------------------------------------------------------------------- main

const [, , command, ...rest] = process.argv;
switch (command) {
  case "create":
    await create();
    break;
  case "snapshot":
    await snapshot(rest[0]);
    break;
  case "compare":
    compare(rest[0], rest[1]);
    break;
  case "exercise":
    await exercise();
    break;
  case "pause":
    await pause();
    break;
  default:
    console.error("usage: state.mjs <create|snapshot <label>|compare <a> <b>|exercise|pause>");
    process.exit(2);
}
