// Bounded USDC transfer-activity observations from recent transactions that
// mention the configured PayAI fee-payer account. Fee-payer association does
// not prove that a transfer was an x402 settlement, a completed job, or a
// positive outcome. Read-only; never touches the payment flow.

const USDC_MAINNET = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
const USDC_DEVNET = '4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU';
const TOKEN_PROGRAM_ID = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
export const PAYAI_FEE_PAYER = '2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4';

const isUsdc = (m: string): boolean => m === USDC_MAINNET || m === USDC_DEVNET;

export interface TransferObservation {
  signature: string;
  slot: number;
  blockTime: number | null;
  mint: string;
  payer: string;
  payTo: string;
  amountMicro: bigint;
}

export interface TransferActivity {
  schema: 'covenant.payai-transfer-activity.v1';
  wallet: string;
  observed_inbound_transfers: number;
  distinct_observed_senders: number;
  observed_volume_micro_usdc: string;
  source_account_scanned: string;
  coverage: {
    requested_signature_limit: number;
    signatures_returned: number;
    transactions_loaded: number;
    commitment: 'confirmed';
  };
  limitations: string[];
}

async function rpc(url: string, timeoutMs: number, method: string, params: unknown): Promise<any> {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const res = await fetch(url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      signal: ctrl.signal,
    });
    if (!res.ok) throw new Error(`${method} status ${res.status}`);
    const j = (await res.json()) as { error?: unknown; result?: unknown };
    if (j.error) throw new Error(`${method}: ${JSON.stringify(j.error)}`);
    return j.result ?? null;
  } finally {
    clearTimeout(timer);
  }
}

function accountKeys(tx: any): string[] {
  const arr = tx?.transaction?.message?.accountKeys;
  if (!Array.isArray(arr)) return [];
  return arr
    .map((k: any) => (typeof k === 'string' ? k : k?.pubkey))
    .filter((s: any): s is string => typeof s === 'string');
}

// ata -> field, from post+pre token balances (post wins).
function balanceMap(tx: any, keys: string[], field: string): Map<string, string> {
  const m = new Map<string, string>();
  const post = tx?.meta?.postTokenBalances;
  const pre = tx?.meta?.preTokenBalances;
  const all = [...(Array.isArray(post) ? post : []), ...(Array.isArray(pre) ? pre : [])];
  for (const b of all) {
    const idx = b?.accountIndex;
    if (typeof idx !== 'number') continue;
    const ata = keys[idx];
    if (!ata) continue;
    const val = b?.[field];
    if (typeof val === 'string' && !m.has(ata)) m.set(ata, val);
  }
  return m;
}

function splTokenInstructions(tx: any): any[] {
  const out: any[] = [];
  const isSpl = (ins: any): boolean =>
    ins?.program === 'spl-token' || ins?.programId === TOKEN_PROGRAM_ID;
  const top = tx?.transaction?.message?.instructions;
  if (Array.isArray(top)) for (const ins of top) if (isSpl(ins)) out.push(ins);
  const inner = tx?.meta?.innerInstructions;
  if (Array.isArray(inner)) {
    for (const g of inner) {
      if (Array.isArray(g?.instructions))
        for (const ins of g.instructions) if (isSpl(ins)) out.push(ins);
    }
  }
  return out;
}

// Parse every USDC settlement from a jsonParsed transaction. Empty for a failed
// or non-USDC transaction. Attributes transfers to OWNER wallets, not ATAs.
export function parseUsdcTransfers(tx: any, signature: string): TransferObservation[] {
  if (tx?.meta?.err != null) return [];
  const slot = typeof tx?.slot === 'number' ? tx.slot : 0;
  const blockTime = typeof tx?.blockTime === 'number' ? tx.blockTime : null;
  const keys = accountKeys(tx);
  const ownerByAta = balanceMap(tx, keys, 'owner');
  const mintByAta = balanceMap(tx, keys, 'mint');

  const out: TransferObservation[] = [];
  for (const ins of splTokenInstructions(tx)) {
    const parsed = ins?.parsed;
    if (!parsed) continue;
    const typ = parsed?.type;
    if (typ !== 'transferChecked' && typ !== 'transfer') continue;
    const info = parsed?.info;
    if (!info) continue;
    const source = typeof info.source === 'string' ? info.source : '';
    const dest = typeof info.destination === 'string' ? info.destination : '';
    if (!source || !dest) continue;

    const mint =
      (typeof info.mint === 'string' ? info.mint : undefined) ??
      mintByAta.get(dest) ??
      mintByAta.get(source);
    if (!mint || !isUsdc(mint)) continue;

    const amountStr = typ === 'transferChecked' ? info?.tokenAmount?.amount : info?.amount;
    let amountMicro: bigint;
    try {
      amountMicro = BigInt(amountStr ?? '0');
    } catch {
      amountMicro = 0n;
    }
    if (amountMicro === 0n) continue;

    const payer =
      (typeof info.authority === 'string' && info.authority) || ownerByAta.get(source) || source;
    const payTo = ownerByAta.get(dest) ?? dest;

    out.push({ signature, slot, blockTime, mint, payer, payTo, amountMicro });
  }
  return out;
}

// Inbound transfers from other wallets only. This is an observation summary,
// not an outcome, trust, or reputation score.
export function summarizeTransferActivity(
  transfers: TransferObservation[],
  wallet: string,
  sourceAccount: string,
  coverage = { requested: 0, returned: 0, loaded: 0 },
): TransferActivity {
  let observed = 0;
  let volume = 0n;
  const senders = new Set<string>();
  for (const transfer of transfers) {
    if (transfer.payTo === wallet && transfer.payer !== wallet) {
      observed += 1;
      volume += transfer.amountMicro;
      senders.add(transfer.payer);
    }
  }
  return {
    schema: 'covenant.payai-transfer-activity.v1',
    wallet,
    observed_inbound_transfers: observed,
    distinct_observed_senders: senders.size,
    observed_volume_micro_usdc: volume.toString(),
    source_account_scanned: sourceAccount,
    coverage: {
      requested_signature_limit: coverage.requested,
      signatures_returned: coverage.returned,
      transactions_loaded: coverage.loaded,
      commitment: 'confirmed',
    },
    limitations: [
      'Only the latest fee-payer-associated signatures within the configured limit are scanned.',
      'Observed transfers are not proof of x402 settlement, job delivery, quality, or reputation.',
      'Confirmed RPC data can still be reorganized and is not independently proven by this response.',
    ],
  };
}

// Fetch the latest `limit` fee-payer signatures and summarize parsed inbound
// USDC transfers. One getTransaction per signature (sequential). Solana caps
// getSignaturesForAddress at 1000, so a single call can't fan out unbounded.
export async function getTransferActivity(
  rpcUrl: string,
  timeoutMs: number,
  wallet: string,
  limit: number,
): Promise<TransferActivity> {
  const lim = Math.max(1, Math.min(Math.floor(limit), 1000));
  const sigs = await rpc(rpcUrl, timeoutMs, 'getSignaturesForAddress', [
    PAYAI_FEE_PAYER,
    { limit: lim },
  ]);
  if (!Array.isArray(sigs)) throw new Error('getSignaturesForAddress: result not an array');
  const transfers: TransferObservation[] = [];
  let loaded = 0;
  for (const e of sigs) {
    if (e?.err != null) continue;
    const sig = e?.signature;
    if (typeof sig !== 'string') continue;
    const tx = await rpc(rpcUrl, timeoutMs, 'getTransaction', [
      sig,
      { encoding: 'jsonParsed', maxSupportedTransactionVersion: 0, commitment: 'confirmed' },
    ]);
    if (tx == null) continue;
    loaded += 1;
    transfers.push(...parseUsdcTransfers(tx, sig));
  }
  return summarizeTransferActivity(transfers, wallet, PAYAI_FEE_PAYER, {
    requested: lim,
    returned: sigs.length,
    loaded,
  });
}
