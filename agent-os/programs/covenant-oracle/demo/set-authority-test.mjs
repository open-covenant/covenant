// Devnet proof that set_authority moves the verdict authority: init an oracle,
// hand it to a fresh key, then confirm the old key can no longer flip the
// verdict and the new key can. Run against the upgraded devnet program.

import { readFileSync } from 'node:fs';
import {
  Connection, Keypair, PublicKey, SystemProgram,
  Transaction, TransactionInstruction, sendAndConfirmTransaction,
} from '@solana/web3.js';

const RPC = process.env.RPC_URL || 'https://api.devnet.solana.com';
const ORACLE_PROGRAM = new PublicKey('2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD');
const idl = JSON.parse(readFileSync(new URL('./idl/covenant_oracle_program.json', import.meta.url)));
const disc = (n) => Buffer.from(idl.instructions.find((i) => i.name === n).discriminator);

const secret = Uint8Array.from(JSON.parse(readFileSync(`${process.env.HOME}/.config/solana/id.json`)));
const payer = Keypair.fromSecretKey(secret);
const conn = new Connection(RPC, 'confirmed');

const subject = Keypair.generate().publicKey;
const pda = PublicKey.findProgramAddressSync([Buffer.from('oracle'), subject.toBuffer()], ORACLE_PROGRAM)[0];
const newAuth = Keypair.generate();

const send = (keys, data, signers) =>
  sendAndConfirmTransaction(conn,
    new Transaction().add(new TransactionInstruction({ programId: ORACLE_PROGRAM, keys, data })),
    signers, { commitment: 'confirmed' });

const authorityOf = async () => {
  const acct = await conn.getAccountInfo(pda, 'confirmed');
  return new PublicKey(acct.data.subarray(13, 45)).toBase58();
};

let failed = false;
const check = (label, ok) => { console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${label}`); if (!ok) failed = true; };

console.log(`\nset_authority devnet test (${RPC})`);
console.log(`subject ${subject.toBase58()}\noracle  ${pda.toBase58()}`);
console.log(`old authority ${payer.publicKey.toBase58()}\nnew authority ${newAuth.publicKey.toBase58()}\n`);

await send([
  { pubkey: pda, isSigner: false, isWritable: true },
  { pubkey: payer.publicKey, isSigner: true, isWritable: true },
  { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
], Buffer.concat([disc('init_oracle'), subject.toBuffer()]), [payer]);
check('init_oracle, authority = payer', (await authorityOf()) === payer.publicKey.toBase58());

await send([
  { pubkey: pda, isSigner: false, isWritable: true },
  { pubkey: payer.publicKey, isSigner: true, isWritable: false },
], Buffer.concat([disc('set_authority'), newAuth.publicKey.toBuffer()]), [payer]);
check('set_authority moved authority to the new key', (await authorityOf()) === newAuth.publicKey.toBase58());

let oldRejected = false;
try {
  await send([
    { pubkey: pda, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: false },
  ], Buffer.concat([disc('set_validation'), Buffer.from([0])]), [payer]);
} catch { oldRejected = true; }
check('old authority can no longer set_validation', oldRejected);

// Fund the new authority so it can pay fees, then prove it can flip the verdict.
await sendAndConfirmTransaction(conn, new Transaction().add(SystemProgram.transfer({
  fromPubkey: payer.publicKey, toPubkey: newAuth.publicKey, lamports: 20_000_000,
})), [payer], { commitment: 'confirmed' });
let newWorks = true;
try {
  await send([
    { pubkey: pda, isSigner: false, isWritable: true },
    { pubkey: newAuth.publicKey, isSigner: true, isWritable: false },
  ], Buffer.concat([disc('set_validation'), Buffer.from([0])]), [newAuth]);
} catch (e) { newWorks = false; console.log('   new-auth set_validation failed:', String(e.message || e).split('\n')[0]); }
check('new authority can set_validation', newWorks);

console.log(`\n${failed ? 'TEST FAILED' : 'TEST PASSED'} - set_authority transfers control as intended.\n`);
process.exit(failed ? 1 : 0);
