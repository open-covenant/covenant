// Anchor one receipt batch on the devnet Covenant settlement program, committing
// a run's audit root on-chain. Produces the settlement-anchor witness for /verify.
//
//   node anchor-settlement.mjs --root <audit-root-hex> --sha <commit> --keypair <authority.json>
//
// The authority must equal the program config's authority. The batch PDA is
// derived from a unique batch_id = sha256("<sha>:<root>"); re-running with the
// same inputs fails because the PDA is init-once.

import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const arg = (n) => {
  const i = process.argv.indexOf(`--${n}`);
  return i >= 0 ? process.argv[i + 1] : undefined;
};
const root = arg("root");
const sha = arg("sha");
const keypairPath = arg("keypair");
if (!root || !sha || !keypairPath) {
  console.error("usage: --root <audit-root-hex> --sha <commit> --keypair <authority.json>");
  process.exit(2);
}

const PROGRAM = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const CONFIG = new PublicKey("BGGx99dV5LU2GpKCmhXqT1mi1yNr8EuMuMd5BAG7Lcvi");
const DISCRIMINATOR = Buffer.from([90, 207, 91, 121, 57, 61, 176, 129]); // global:anchor_receipt_batch

const merkleRoot = Buffer.from(root, "hex");
if (merkleRoot.length !== 32) {
  console.error("root must be 32 bytes of hex");
  process.exit(2);
}
const batchId = createHash("sha256").update(`${sha}:${root}`).digest();
const count = Buffer.alloc(4);
count.writeUInt32LE(1, 0);
const data = Buffer.concat([DISCRIMINATOR, batchId, merkleRoot, count]);

const [batchPda] = PublicKey.findProgramAddressSync([Buffer.from("receipt_batch"), batchId], PROGRAM);
const authority = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(keypairPath, "utf8"))));

const ix = new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: CONFIG, isSigner: false, isWritable: false },
    { pubkey: batchPda, isSigner: false, isWritable: true },
    { pubkey: authority.publicKey, isSigner: true, isWritable: true },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data,
});

const conn = new Connection("https://api.devnet.solana.com", "confirmed");
const tx = await sendAndConfirmTransaction(conn, new Transaction().add(ix), [authority], {
  commitment: "confirmed",
});
const status = await conn.getSignatureStatus(tx);
console.log(
  JSON.stringify({
    tx,
    batch_pda: batchPda.toBase58(),
    merkle_root: root,
    slot: status.value?.slot ?? null,
    authority: authority.publicKey.toBase58(),
    cluster: "devnet",
  }),
);
