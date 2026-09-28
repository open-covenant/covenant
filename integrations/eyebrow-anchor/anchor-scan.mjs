#!/usr/bin/env node
// Anchor an eyebrow supply-chain scan onto Solana.
//
// eyebrow inventories the AI agent tooling installed on a machine (skills,
// MCP servers, plugins, hooks), hashes every file behind each one and writes
// eyebrowlock.json. That lockfile is only as trustworthy as the disk holding
// it. This publishes a commitment to it through the Covenant settlement
// program's `anchor_receipt_batch`, so the scan gets a timestamp and a root
// that a third party can check and nobody can quietly revise.
//
//   node anchor-scan.mjs --dry-run              derive everything, send nothing
//   node anchor-scan.mjs --cluster devnet       derive and send
//
// Run `node verify-anchor.mjs --help` for the other half.

import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LAMPORTS_PER_SOL, Transaction, sendAndConfirmTransaction } from "@solana/web3.js";

import { deriveFromLockfile, hex } from "./derive.mjs";
import {
  CLUSTERS,
  Connection,
  PROGRAM_ID,
  RECEIPT_BATCH_LEN,
  anchorReceiptBatchIx,
  batchPda,
  configPda,
  explorerAddress,
  explorerTx,
  loadSigner,
  programStatus,
  readConfigAuthority,
  readReceiptBatch,
} from "./chain.mjs";

const USAGE = `anchor-scan: anchor an eyebrow scan through the Covenant settlement program

  --cluster <mainnet|devnet>   which Solana cluster to write to (default mainnet)
  --rpc <url>                  override the cluster's RPC endpoint
  --lockfile <path>            anchor an existing lockfile instead of scanning
  --path <dir>                 project root to scan (default: the current directory)
  --global                     include the user-home scope in the scan
  --keypair <path>             signer; must be the settlement Config authority
  --dry-run                    derive and report, send nothing, spend nothing
  --json                       machine-readable output
  -h, --help                   this text
`;

function parseArgs(argv) {
  const opts = {
    cluster: "mainnet",
    rpc: null,
    lockfile: null,
    path: process.cwd(),
    global: false,
    keypair: null,
    dryRun: false,
    json: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      const value = argv[i + 1];
      if (value === undefined) throw new Error(`${arg} needs a value`);
      i += 1;
      return value;
    };
    switch (arg) {
      case "--cluster": opts.cluster = next(); break;
      case "--rpc": opts.rpc = next(); break;
      case "--lockfile": opts.lockfile = next(); break;
      case "--path": opts.path = next(); break;
      case "--global": opts.global = true; break;
      case "--keypair": opts.keypair = next(); break;
      case "--dry-run": opts.dryRun = true; break;
      case "--json": opts.json = true; break;
      case "-h":
      case "--help": opts.help = true; break;
      default: throw new Error(`unknown flag ${arg}`);
    }
  }
  if (!CLUSTERS[opts.cluster]) {
    throw new Error(`unknown cluster ${opts.cluster}; expected mainnet or devnet`);
  }
  return opts;
}

/** Run eyebrow into a throwaway lockfile and return its path. */
function runScan({ path, global: includeGlobal }, log) {
  const dir = mkdtempSync(join(tmpdir(), "eyebrow-anchor-"));
  const lockfile = join(dir, "eyebrowlock.json");
  const args = ["scan", "--json", "--path", path, "--lockfile", lockfile];
  if (includeGlobal) args.push("--global");
  log(`scanning ${includeGlobal ? "project and global scope" : "project scope"}…`);
  try {
    execFileSync("eyebrow", args, { stdio: ["ignore", "ignore", "inherit"] });
  } catch (error) {
    if (error.code === "ENOENT") {
      throw new Error(
        "eyebrow is not on PATH. Install it with `brew install alexverify/tap/eyebrow`, " +
          "or pass an existing lockfile with --lockfile.",
      );
    }
    throw error;
  }
  if (!existsSync(lockfile)) throw new Error("eyebrow scan wrote no lockfile");
  return lockfile;
}

function loadLockfile(path) {
  let text;
  try {
    text = readFileSync(path, "utf8");
  } catch {
    throw new Error(`cannot read lockfile ${path}`);
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`${path} is not valid JSON: ${error.message}`);
  }
}

function defaultKeypairPath() {
  return join(process.env.HOME ?? "", ".config", "solana", "id.json");
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    process.stdout.write(USAGE);
    return 0;
  }

  const out = [];
  const log = (line = "") => {
    if (opts.json) out.push(line);
    else process.stdout.write(`${line}\n`);
  };

  const cluster = CLUSTERS[opts.cluster];
  const lockfilePath = opts.lockfile ?? runScan(opts, log);
  const lockfile = loadLockfile(lockfilePath);
  const derived = deriveFromLockfile(lockfile);

  const hashed = derived.leaves.filter((leaf) => leaf.hashed).length;
  log("eyebrow scan");
  log(`  generator      ${derived.generator}`);
  log(`  generated at   ${derived.generatedAt}`);
  log(`  artifacts      ${derived.receiptCount} (${hashed} file-hashed, ${derived.receiptCount - hashed} remote)`);
  log("");
  log("derived commitment");
  log(`  content hash   ${hex(derived.contentHash)}`);
  log(`  batch id       ${hex(derived.batchId)}`);
  log(`  merkle root    ${hex(derived.merkleRoot)}`);
  log(`  receipt count  ${derived.receiptCount}`);
  log("");

  const pda = batchPda(derived.batchId);
  const connection = new Connection(opts.rpc ?? cluster.rpcUrl, "confirmed");

  log(`settlement program ${PROGRAM_ID.toBase58()} on ${cluster.key}`);
  log(`  config         ${configPda().toBase58()}`);
  log(`  batch account  ${pda.toBase58()}`);

  const status = await programStatus(connection);
  if (!status.live) {
    log(`  program        NOT CALLABLE: ${status.reason}`);
  }

  const existing = await readReceiptBatch(connection, derived.batchId);
  if (existing.account) {
    const matches = existing.account.merkleRoot.equals(derived.merkleRoot);
    log("");
    log("this scan is already anchored");
    log(`  anchored at    ${new Date(existing.account.createdAt * 1000).toISOString()}`);
    log(`  on-chain root  ${hex(existing.account.merkleRoot)}`);
    log(`  root matches   ${matches ? "yes" : "NO"}`);
    log(`  account        ${explorerAddress(pda.toBase58(), cluster)}`);
    if (opts.json) process.stdout.write(`${JSON.stringify(reportJson(derived, pda, cluster, null, out), null, 2)}\n`);
    return matches ? 0 : 1;
  }

  const rent = await connection.getMinimumBalanceForRentExemption(RECEIPT_BATCH_LEN);
  log(`  account rent   ${(rent / LAMPORTS_PER_SOL).toFixed(9)} SOL for ${RECEIPT_BATCH_LEN} bytes`);

  if (opts.dryRun) {
    log("");
    log("dry run: nothing sent, nothing spent.");
    if (opts.json) process.stdout.write(`${JSON.stringify(reportJson(derived, pda, cluster, null, out), null, 2)}\n`);
    return 0;
  }

  if (!status.live) {
    throw new Error(
      `the settlement program is not callable on ${cluster.key}: ${status.reason}. ` +
        "Re-run with --dry-run, or point --cluster at a cluster where it is deployed.",
    );
  }

  const keypairPath = opts.keypair ?? defaultKeypairPath();
  const signer = loadSigner(keypairPath);
  const config = await readConfigAuthority(connection);
  log("");
  log("signer");
  log(`  address        ${signer.publicKey.toBase58()}`);
  if (config.authority !== signer.publicKey.toBase58()) {
    throw new Error(
      `anchor_receipt_batch only accepts the Config authority (${config.authority ?? "config missing"}); ` +
        `the loaded signer is ${signer.publicKey.toBase58()}`,
    );
  }
  if (config.paused) throw new Error("the settlement protocol is paused; anchoring is refused");

  const balance = await connection.getBalance(signer.publicKey);
  const needed = rent + 10_000;
  log(`  balance        ${(balance / LAMPORTS_PER_SOL).toFixed(9)} SOL`);
  if (balance < needed) {
    throw new Error(
      `balance is ${(balance / LAMPORTS_PER_SOL).toFixed(9)} SOL, short of the ` +
        `${(needed / LAMPORTS_PER_SOL).toFixed(9)} SOL this anchor costs. Re-run with --dry-run.`,
    );
  }

  const transaction = new Transaction().add(
    anchorReceiptBatchIx({
      authority: signer.publicKey,
      batchId: derived.batchId,
      merkleRoot: derived.merkleRoot,
      receiptCount: derived.receiptCount,
    }),
  );
  const signature = await sendAndConfirmTransaction(connection, transaction, [signer], {
    commitment: "confirmed",
  });

  log("");
  log("anchored");
  log(`  signature      ${signature}`);
  log(`  transaction    ${explorerTx(signature, cluster)}`);
  log(`  batch account  ${explorerAddress(pda.toBase58(), cluster)}`);
  log("");
  log("verify it with:");
  log(
    `  node verify-anchor.mjs --cluster ${cluster.key} --lockfile ${lockfilePath} --batch-id ${hex(derived.batchId)}`,
  );

  if (opts.json) {
    process.stdout.write(`${JSON.stringify(reportJson(derived, pda, cluster, signature, out), null, 2)}\n`);
  }
  return 0;
}

function reportJson(derived, pda, cluster, signature, lines) {
  return {
    cluster: cluster.key,
    programId: PROGRAM_ID.toBase58(),
    generator: derived.generator,
    generatedAt: derived.generatedAt,
    contentHash: hex(derived.contentHash),
    batchId: hex(derived.batchId),
    merkleRoot: hex(derived.merkleRoot),
    receiptCount: derived.receiptCount,
    batchAccount: pda.toBase58(),
    signature,
    explorer: signature ? explorerTx(signature, cluster) : null,
    log: lines,
  };
}

main()
  .then((code) => process.exit(code))
  .catch((error) => {
    process.stderr.write(`anchor-scan: ${error.message}\n`);
    process.exit(1);
  });
