/**
 * An agent finds Mizuki in Coinbase's x402 Bazaar and pays for a repository
 * assessment. Nothing here is configured ahead of time: the endpoint, its price,
 * and its input schema all come from the public catalog.
 */
import { readFileSync } from 'node:fs';
import { createKeyPairSignerFromBytes } from '@solana/kit';
import { x402Client, x402HTTPClient } from '@x402/core/client';
import { registerExactSvmScheme } from '@x402/svm/exact/client';

const BAZAAR = 'https://api.cdp.coinbase.com/platform/v2/x402/discovery/resources';

// 1. Discover. Search the public catalog for a service that maintains repositories.
async function findMizuki() {
  const { pagination } = await fetch(`${BAZAAR}?limit=1`).then((r) => r.json());
  const offsets = [];
  for (let o = 0; o < pagination.total; o += 100) offsets.push(o);
  console.log('catalog    :', pagination.total, 'resources');

  // Fetch pages in parallel, as an agent scanning a catalog this size would.
  const pages = [];
  for (let i = 0; i < offsets.length; i += 12) {
    pages.push(
      ...(await Promise.all(
        offsets.slice(i, i + 12).map((o) =>
          fetch(`${BAZAAR}?limit=100&offset=${o}`)
            .then((r) => r.json())
            .then((p) => p.items ?? [])
            .catch(() => []),
        ),
      )),
    );
    const hit = pages.flat().find((x) => String(x.resource).includes('mizuki/assess'));
    if (hit) return hit;
  }
  return undefined;
}

const listing = await findMizuki();
if (!listing) throw new Error('Mizuki is not in the Bazaar');

const accepts = listing.accepts[0];
console.log('found      :', listing.resource);
console.log('service    :', listing.serviceName);
console.log('price      :', accepts.amount, 'atomic USDC on', accepts.network);
console.log('input      :', JSON.stringify(listing.extensions.bazaar.info.input.pathParams));

// 2. Fill in the declared path parameters from the catalog's own schema.
const target = { owner: 'open-covenant', repo: 'covenant' };
const url = listing.resource.replace(':owner', target.owner).replace(':repo', target.repo);

// 3. Pay. The wallet needs USDC; the facilitator sponsors the Solana fee.
// A funded Solana keypair, as a 64-byte JSON array. The wallet needs USDC for
// the price above; the facilitator sponsors the network fee, so it needs no SOL.
const keypair = process.env.SOLANA_KEYPAIR
  ? JSON.parse(process.env.SOLANA_KEYPAIR)
  : JSON.parse(readFileSync(process.env.SOLANA_KEYPAIR_PATH ?? '', 'utf8'));
const signer = await createKeyPairSignerFromBytes(new Uint8Array(keypair));
const http = new x402HTTPClient(registerExactSvmScheme(new x402Client(), { signer }));

const challenge = await fetch(url);
const required = await http.getPaymentRequiredResponse(
  (name) => challenge.headers.get(name),
  await challenge
    .clone()
    .json()
    .catch(() => ({})),
);
const paid = await fetch(url, {
  headers: http.encodePaymentSignatureHeader(await http.createPaymentPayload(required)),
});

const settlement = JSON.parse(
  Buffer.from(paid.headers.get('payment-response'), 'base64').toString('utf8'),
);
console.log('\npayer      :', signer.address);
console.log('settled    :', settlement.transaction);
console.log('assessment :', JSON.stringify(await paid.json(), null, 2));
