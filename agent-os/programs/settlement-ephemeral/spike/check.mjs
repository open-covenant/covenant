import { Connection, PublicKey } from "@solana/web3.js";
const L1 = new Connection("https://api.devnet.solana.com", "confirmed");
const PROG = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const OWNER = new PublicKey(process.argv[2]); // payer/owner pubkey
const pda = (s) => PublicKey.findProgramAddressSync(s, PROG)[0];
const config = pda([Buffer.from("config")]);
const credits = pda([Buffer.from("credits"), OWNER.toBuffer()]);
console.log("program ", PROG.toBase58());
console.log("config  ", config.toBase58());
console.log("credits ", credits.toBase58());
const cfg = await L1.getAccountInfo(config);
if (!cfg) console.log("config: DOES NOT EXIST (need initialize)");
else {
  const d = cfg.data;
  console.log("config: exists, owner", cfg.owner.toBase58(), "len", d.length);
  console.log("  authority     ", new PublicKey(d.subarray(8, 40)).toBase58());
  console.log("  slash_authority", new PublicKey(d.subarray(40, 72)).toBase58());
  console.log("  covnt_mint    ", new PublicKey(d.subarray(72, 104)).toBase58());
  console.log("  treasury      ", new PublicKey(d.subarray(104, 136)).toBase58());
  console.log("  credits_per_covnt", Number(d.readBigUInt64LE(136)));
}
const cr = await L1.getAccountInfo(credits);
if (!cr) console.log("credits: DOES NOT EXIST (need open_credit_account + buy_credits)");
else {
  console.log("credits: exists, owner-program", cr.owner.toBase58(), "len", cr.data.length);
  console.log("  balance", Number(cr.data.readBigUInt64LE(40)));
  console.log("  (owner-program == delegation program means currently delegated)");
}
