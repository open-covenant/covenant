// Bond our demo agent on mainnet and close the accountability loop: register the
// agent, stake a slashable CVNT position against it, then slash it with the
// reason read straight from the on-chain provenance_root that the reference run
// folded from the agent's real work (credits PDA [b"credits", operator] is
// DrawYGmd, the account we metered). Nothing caller-supplied to forge.
//
//   node bond-slash.mjs            # register + stake 5000 + slash 1000 CVNT
//   STAKE=5000 SLASH=1000 node bond-slash.mjs
import fs from "node:fs";
import os from "node:os";
import crypto from "node:crypto";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction, sendAndConfirmTransaction } from "@solana/web3.js";

const PROG = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const MINT = new PublicKey("2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump");
const TOKEN22 = new PublicKey("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ATA_PROG = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const TREASURY = new PublicKey("8zBseLENQbY8gDQS8X1YoY7ijfxVUfcjvnS5dgjNrqXQ");
const DEC = 6n, U = 10n ** DEC;
const STAKE = BigInt(process.env.STAKE || "5000") * U;
const SLASH = BigInt(process.env.SLASH || "1000") * U;

const owner = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/solana-id.json`, "utf8"))));
const l1 = new Connection(fs.readFileSync("/tmp/cov-deploy-rpc", "utf8").trim(), "confirmed");
const pda = (s) => PublicKey.findProgramAddressSync(s, PROG)[0];
const ata = (o) => PublicKey.findProgramAddressSync([o.toBuffer(), TOKEN22.toBuffer(), MINT.toBuffer()], ATA_PROG)[0];
const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const sha = (s) => crypto.createHash("sha256").update(s).digest();
const m = (pubkey, s, w) => ({ pubkey, isSigner: s, isWritable: w });
const send = (ix) => sendAndConfirmTransaction(l1, new Transaction().add(ix), [owner], { commitment: "confirmed" });

const SYSTEM_PROMPT =
  "You are the demo agent on Covenant's public sandbox. Reply briefly, 2 to 4 sentences. " +
  "You're running as a sandboxed agent inside a Covenant daemon that signs and logs every step.";
const agentKey = sha("covenant.demo-agent.v1");
const config = pda([Buffer.from("config")]);
const agent = pda([Buffer.from("agent"), agentKey]);
const position = pda([Buffer.from("stake"), agentKey, owner.publicKey.toBuffer()]);
const credits = pda([Buffer.from("credits"), owner.publicKey.toBuffer()]);
const ownerCovnt = ata(owner.publicKey);
const stakeVault = ata(position);

const agentAcct = (async () => {
  console.log(`operator   ${owner.publicKey.toBase58()}`);
  console.log(`agent_key  ${agentKey.toString("hex")}`);
  console.log(`agent PDA  ${agent.toBase58()}`);
  console.log(`position   ${position.toBase58()}`);
  console.log(`credits    ${credits.toBase58()} (metered account, holds the provenance)\n`);

  const prov = (await l1.getAccountInfo(credits)).data.subarray(49, 81);
  console.log(`provenance_root to be cited: ${Buffer.from(prov).toString("hex")}\n`);

  if (!(await l1.getAccountInfo(agent))) {
    const data = Buffer.concat([disc("register_agent"), agentKey, sha(SYSTEM_PROMPT), sha("llm.chat:haiku")]);
    const ix = new TransactionInstruction({ programId: PROG, keys: [m(config, false, false), m(agent, false, true), m(owner.publicKey, true, true), m(SystemProgram.programId, false, false)], data });
    console.log(`registered ${await send(ix)}`);
  } else console.log("agent already registered");

  if (!(await l1.getAccountInfo(stakeVault))) {
    const ix = new TransactionInstruction({ programId: ATA_PROG, keys: [m(owner.publicKey, true, true), m(stakeVault, false, true), m(position, false, false), m(MINT, false, false), m(SystemProgram.programId, false, false), m(TOKEN22, false, false)], data: Buffer.from([1]) });
    console.log(`vault      ${await send(ix)} (${stakeVault.toBase58()})`);
  } else console.log("stake vault exists");

  const posInfo = await l1.getAccountInfo(position);
  if (!posInfo) {
    const now = Math.floor(Date.now() / 1000);
    const lockUntil = BigInt(now + 604800 + 3600);
    const data = Buffer.alloc(24);
    disc("stake").copy(data, 0);
    data.writeBigUInt64LE(STAKE, 8);
    data.writeBigUInt64LE(lockUntil, 16);
    const ix = new TransactionInstruction({ programId: PROG, keys: [
      m(config, false, false), m(agent, false, true), m(position, false, true), m(owner.publicKey, true, true),
      m(ownerCovnt, false, true), m(stakeVault, false, true), m(MINT, false, false), m(TOKEN22, false, false), m(SystemProgram.programId, false, false),
    ], data });
    console.log(`staked     ${await send(ix)}  (${STAKE / U} CVNT bonded, lock +7d)`);
  } else console.log(`already staked (${posInfo.data.readBigUInt64LE(72) / U} CVNT)`);

  const agentStakeOffset = 136; // disc8 + agent_key32 + operator32 + metadata32 + capability32
  const agentBonded = (await l1.getAccountInfo(agent)).data.readBigUInt64LE(agentStakeOffset);
  console.log(`\nagent.stake (bonded): ${agentBonded / U} CVNT`);

  if (SLASH > 0n) {
    const data = Buffer.alloc(16);
    disc("slash_for_actions").copy(data, 0);
    data.writeBigUInt64LE(SLASH, 8);
    const ix = new TransactionInstruction({ programId: PROG, keys: [
      m(config, false, false), m(owner.publicKey, true, false), m(agent, false, true), m(position, false, true),
      m(credits, false, false), m(stakeVault, false, true), m(TREASURY, false, true), m(MINT, false, false), m(TOKEN22, false, false),
    ], data });
    console.log(`slashed    ${await send(ix)}  (${SLASH / U} CVNT slashed for its on-chain actions)`);
  }

  const after = (await l1.getAccountInfo(agent)).data.readBigUInt64LE(agentStakeOffset);
  console.log("\n================ BOND + SLASH ================");
  console.log(`agent bonded    ${agentBonded / U} CVNT`);
  console.log(`slashed         ${SLASH / U} CVNT (reason = on-chain provenance_root, not caller-supplied)`);
  console.log(`agent remaining ${after / U} CVNT still bonded`);
  console.log(`loop            bond -> act (provenance from real work) -> slash: LIVE on mainnet`);

  fs.writeFileSync(new URL("./bond-slash-result.json", import.meta.url), JSON.stringify({
    ts: new Date().toISOString(), program: PROG.toBase58(), agentKey: agentKey.toString("hex"),
    agentPda: agent.toBase58(), position: position.toBase58(), credits: credits.toBase58(),
    provenanceCited: Buffer.from(prov).toString("hex"), bonded: (agentBonded / U).toString(),
    slashed: (SLASH / U).toString(), remaining: (after / U).toString(),
  }, null, 2));
})();
await agentAcct;
