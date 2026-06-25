import { readFileSync } from 'node:fs';
import { createUmi } from '@metaplex-foundation/umi-bundle-defaults';
import { publicKey } from '@metaplex-foundation/umi';
import { fetchAsset, mplCore } from '@metaplex-foundation/mpl-core';

const envFile = `${process.env.HOME}/Projects/covenant/landing/.env.local`;
const RPC = process.env.RPC_URL ||
  readFileSync(envFile, 'utf8').match(/NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL=(\S+)/)?.[1];
if (!RPC) throw new Error('no mainnet RPC');

const ASSET = process.argv[2];
const DKXXR = 'DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK';
const umi = createUmi(RPC).use(mplCore());
const a = await fetchAsset(umi, publicKey(ASSET));

console.log('asset           ', ASSET);
console.log('name            ', a.name);
console.log('owner           ', a.owner);
console.log('updateAuthority ', JSON.stringify(a.updateAuthority));
console.log('owner == DKxXr  ', a.owner === DKXXR);
console.log('updAuth == DKxXr', a.updateAuthority?.address === DKXXR);
const j = (v) => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? x.toString() : x), 2);
console.log('oracles         ', j(a.oracles ?? []));
