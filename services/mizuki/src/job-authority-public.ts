import { createPrivateKey, createPublicKey } from 'node:crypto';
import { PublicKey } from '@solana/web3.js';

const encoded = process.env.MIZUKI_JOB_AUTHORITY_SEED;
if (!encoded) throw new Error('MIZUKI_JOB_AUTHORITY_SEED is required');
const seed = Buffer.from(encoded, 'base64');
if (seed.length !== 32 || seed.toString('base64') !== encoded) {
  throw new Error('MIZUKI_JOB_AUTHORITY_SEED must be canonical base64 for a 32-byte seed');
}

const privateKey = createPrivateKey({
  key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), seed]),
  format: 'der',
  type: 'pkcs8',
});
const publicDer = createPublicKey(privateKey).export({ format: 'der', type: 'spki' });
const publicKey = new PublicKey(publicDer.subarray(publicDer.length - 32));
process.stdout.write(`${publicKey.toBase58()}\n`);
