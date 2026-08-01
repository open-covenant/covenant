// Coverage-limited observations of public on-chain USDC transfers in
// transactions paid for by PayAI. These observations do not establish completed
// jobs or a wallet's lifetime payment history. Read-only; never touches payment.

const USDC_MAINNET = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const TOKEN_PROGRAM_ID = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const MAX_SIGNATURES = 1000;
const TRANSACTION_CONCURRENCY = 8;
const SNAPSHOT_TTL_MS = 15_000;
const MAX_SNAPSHOTS = 8;
export const PAYAI_FEE_PAYER = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";

export interface SponsoredTransfer {
  signature: string;
  slot: number;
  blockTime: number | null;
  mint: string;
  payer: string;
  payTo: string;
  amountMicro: bigint;
}

export interface PaymentHistoryCoverage {
  signatures_requested: number;
  signatures_returned: number;
  signatures_candidates: number;
  signatures_scanned: number;
  signatures_unavailable: number;
  oldest_slot: number | null;
  newest_slot: number | null;
}

export interface PaymentObservation {
  transaction_signature: string;
  slot: number;
  block_time: number | null;
  sender: string;
  amount_micro_usdc: string;
  mint: string;
}

export interface PaymentHistory {
  wallet: string;
  observed_at: string;
  observed_inbound_transfers: number;
  distinct_senders: number;
  volume_micro_usdc: string;
  observations: PaymentObservation[];
  source_fee_payer: string;
  classification: "payai-sponsored-usdc-transfer";
  settlement_receipt_linked: false;
  coverage: PaymentHistoryCoverage;
}

async function rpc(
  url: string,
  timeoutMs: number,
  method: string,
  params: unknown,
  signal?: AbortSignal,
): Promise<any> {
  const requestSignal = signal
    ? AbortSignal.any([signal, AbortSignal.timeout(timeoutMs)])
    : AbortSignal.timeout(timeoutMs);
  const res = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
    signal: requestSignal,
  });
  if (!res.ok) throw new Error(`${method} status ${res.status}`);
  const body = (await res.json()) as { error?: unknown; result?: unknown };
  if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
  return body.result ?? null;
}

function accountKeys(tx: any): string[] {
  const arr = tx?.transaction?.message?.accountKeys;
  if (!Array.isArray(arr)) return [];
  return arr
    .map((k: any) => (typeof k === "string" ? k : k?.pubkey))
    .filter((s: any): s is string => typeof s === "string");
}

function isPayaiFeePayer(tx: any): boolean {
  return accountKeys(tx)[0] === PAYAI_FEE_PAYER;
}

// ata -> field, from post+pre token balances (post wins).
function balanceMap(tx: any, keys: string[], field: string): Map<string, string> {
  const m = new Map<string, string>();
  const post = tx?.meta?.postTokenBalances;
  const pre = tx?.meta?.preTokenBalances;
  const all = [...(Array.isArray(post) ? post : []), ...(Array.isArray(pre) ? pre : [])];
  for (const b of all) {
    const idx = b?.accountIndex;
    if (typeof idx !== "number") continue;
    const ata = keys[idx];
    if (!ata) continue;
    const val = b?.[field];
    if (typeof val === "string" && !m.has(ata)) m.set(ata, val);
  }
  return m;
}

function splTokenInstructions(tx: any): any[] {
  const out: any[] = [];
  const isSpl = (ins: any): boolean =>
    ins?.program === "spl-token" || ins?.programId === TOKEN_PROGRAM_ID;
  const top = tx?.transaction?.message?.instructions;
  if (Array.isArray(top)) for (const ins of top) if (isSpl(ins)) out.push(ins);
  const inner = tx?.meta?.innerInstructions;
  if (Array.isArray(inner)) {
    for (const g of inner) {
      if (Array.isArray(g?.instructions)) for (const ins of g.instructions) if (isSpl(ins)) out.push(ins);
    }
  }
  return out;
}

// Parse every USDC transfer from a jsonParsed transaction. Empty for a failed or
// non-USDC transaction. Attributes transfers to owner wallets, not token accounts.
export function parseTransfers(tx: any, signature: string): SponsoredTransfer[] {
  if (tx?.meta?.err != null) return [];
  const slot = typeof tx?.slot === "number" ? tx.slot : 0;
  const blockTime = typeof tx?.blockTime === "number" ? tx.blockTime : null;
  const keys = accountKeys(tx);
  const ownerByAta = balanceMap(tx, keys, "owner");
  const mintByAta = balanceMap(tx, keys, "mint");

  const out: SponsoredTransfer[] = [];
  for (const ins of splTokenInstructions(tx)) {
    const parsed = ins?.parsed;
    if (!parsed) continue;
    const typ = parsed?.type;
    if (typ !== "transferChecked" && typ !== "transfer") continue;
    const info = parsed?.info;
    if (!info) continue;
    const source = typeof info.source === "string" ? info.source : "";
    const dest = typeof info.destination === "string" ? info.destination : "";
    if (!source || !dest) continue;

    const sourceMint = mintByAta.get(source);
    const destinationMint = mintByAta.get(dest);
    const instructionMint = typeof info.mint === "string" ? info.mint : undefined;
    if (sourceMint !== USDC_MAINNET || destinationMint !== USDC_MAINNET) continue;
    if (instructionMint !== undefined && instructionMint !== USDC_MAINNET) continue;

    const amountStr = typ === "transferChecked" ? info?.tokenAmount?.amount : info?.amount;
    let amountMicro: bigint;
    try {
      amountMicro = BigInt(amountStr ?? "0");
    } catch {
      amountMicro = 0n;
    }
    if (amountMicro <= 0n) continue;

    const payer = ownerByAta.get(source);
    const payTo = ownerByAta.get(dest);
    if (!payer || !payTo) continue;

    out.push({ signature, slot, blockTime, mint: USDC_MAINNET, payer, payTo, amountMicro });
  }
  return out;
}

function inferredCoverage(transfers: SponsoredTransfer[]): PaymentHistoryCoverage {
  const slotsBySignature = new Map<string, number>();
  for (const transfer of transfers) {
    if (!slotsBySignature.has(transfer.signature)) {
      slotsBySignature.set(transfer.signature, transfer.slot);
    }
  }
  const slots = [...slotsBySignature.values()].filter(Number.isSafeInteger);
  const signatures = slotsBySignature.size;
  return {
    signatures_requested: signatures,
    signatures_returned: signatures,
    signatures_candidates: signatures,
    signatures_scanned: signatures,
    signatures_unavailable: 0,
    oldest_slot: slots.length > 0 ? Math.min(...slots) : null,
    newest_slot: slots.length > 0 ? Math.max(...slots) : null,
  };
}

// Inbound transfers from other wallets only. This is a payment-history input,
// not a settlement or reputation verdict.
export function computePaymentHistory(
  transfers: SponsoredTransfer[],
  wallet: string,
  feePayer: string,
  coverage: PaymentHistoryCoverage = inferredCoverage(transfers),
  observedAt = new Date().toISOString(),
): PaymentHistory {
  let volume = 0n;
  const counterparties = new Set<string>();
  const observations: PaymentObservation[] = [];
  for (const s of transfers) {
    if (s.payTo === wallet && s.payer !== wallet) {
      volume += s.amountMicro;
      counterparties.add(s.payer);
      observations.push({
        transaction_signature: s.signature,
        slot: s.slot,
        block_time: s.blockTime,
        sender: s.payer,
        amount_micro_usdc: s.amountMicro.toString(),
        mint: s.mint,
      });
    }
  }
  return {
    wallet,
    observed_at: observedAt,
    observed_inbound_transfers: observations.length,
    distinct_senders: counterparties.size,
    volume_micro_usdc: volume.toString(),
    observations,
    source_fee_payer: feePayer,
    classification: "payai-sponsored-usdc-transfer",
    settlement_receipt_linked: false,
    coverage,
  };
}

interface SignatureCandidate {
  signature: string;
  slot: number | null;
}

interface ScannedTransaction extends SignatureCandidate {
  tx: any;
}

interface ScanResult {
  transactions: ScannedTransaction[];
  unavailable: number;
}

interface TransferSnapshot {
  transfers: SponsoredTransfer[];
  coverage: PaymentHistoryCoverage;
}

interface CachedSnapshot {
  expiresAt: number;
  value: Promise<TransferSnapshot>;
}

const snapshots = new Map<string, CachedSnapshot>();

function signatureCandidates(entries: any[]): SignatureCandidate[] {
  const seen = new Set<string>();
  const out: SignatureCandidate[] = [];
  for (const entry of entries) {
    if (entry?.err != null) continue;
    const signature = entry?.signature;
    if (typeof signature !== "string" || signature.length === 0 || seen.has(signature)) continue;
    seen.add(signature);
    out.push({
      signature,
      slot: Number.isSafeInteger(entry?.slot) ? entry.slot : null,
    });
  }
  return out;
}

async function scanTransactions(
  rpcUrl: string,
  timeoutMs: number,
  candidates: SignatureCandidate[],
  signal: AbortSignal,
): Promise<ScanResult> {
  const results = new Array<ScannedTransaction | null>(candidates.length).fill(null);
  let next = 0;
  let unavailable = 0;

  async function worker(): Promise<void> {
    while (next < candidates.length) {
      signal.throwIfAborted();
      const index = next++;
      const candidate = candidates[index];
      try {
        const tx = await rpc(rpcUrl, timeoutMs, "getTransaction", [
          candidate.signature,
          { encoding: "jsonParsed", maxSupportedTransactionVersion: 0, commitment: "confirmed" },
        ], signal);
        if (tx == null) {
          unavailable += 1;
          continue;
        }
        results[index] = {
          ...candidate,
          slot: Number.isSafeInteger(tx?.slot) ? tx.slot : candidate.slot,
          tx,
        };
      } catch {
        if (signal.aborted) signal.throwIfAborted();
        unavailable += 1;
      }
    }
  }

  const workers = Math.min(TRANSACTION_CONCURRENCY, candidates.length);
  await Promise.all(Array.from({ length: workers }, () => worker()));
  return {
    transactions: results.filter((result): result is ScannedTransaction => result !== null),
    unavailable,
  };
}

function coverage(
  requested: number,
  returned: number,
  candidates: number,
  transactions: ScannedTransaction[],
  unavailable: number,
): PaymentHistoryCoverage {
  const slots = transactions
    .map(({ slot }) => slot)
    .filter((slot): slot is number => slot !== null);
  return {
    signatures_requested: requested,
    signatures_returned: returned,
    signatures_candidates: candidates,
    signatures_scanned: transactions.length,
    signatures_unavailable: unavailable,
    oldest_slot: slots.length > 0 ? Math.min(...slots) : null,
    newest_slot: slots.length > 0 ? Math.max(...slots) : null,
  };
}

async function loadSnapshot(
  rpcUrl: string,
  timeoutMs: number,
  lim: number,
): Promise<TransferSnapshot> {
  const sigs = await rpc(rpcUrl, timeoutMs, "getSignaturesForAddress", [PAYAI_FEE_PAYER, { limit: lim }]);
  if (!Array.isArray(sigs)) throw new Error("getSignaturesForAddress: result not an array");
  const scanController = new AbortController();
  const scanTimer = setTimeout(
    () => scanController.abort(new Error('payment-history transaction scan timed out')),
    Math.min(timeoutMs * 2, 60_000),
  );
  const candidates = signatureCandidates(sigs);
  let scan: ScanResult;
  try {
    scan = await scanTransactions(
      rpcUrl,
      timeoutMs,
      candidates,
      scanController.signal,
    );
  } finally {
    clearTimeout(scanTimer);
  }
  const transfers: SponsoredTransfer[] = [];
  for (const { signature, tx } of scan.transactions) {
    if (!isPayaiFeePayer(tx)) continue;
    transfers.push(...parseTransfers(tx, signature));
  }
  return {
    transfers,
    coverage: coverage(
      lim,
      sigs.length,
      candidates.length,
      scan.transactions,
      scan.unavailable,
    ),
  };
}

function transferSnapshot(rpcUrl: string, timeoutMs: number, limit: number): Promise<TransferSnapshot> {
  const key = `${rpcUrl}\n${limit}`;
  const now = Date.now();
  const cached = snapshots.get(key);
  if (cached && cached.expiresAt > now) return cached.value;
  if (cached) snapshots.delete(key);

  if (snapshots.size >= MAX_SNAPSHOTS) {
    const oldest = snapshots.keys().next().value;
    if (oldest !== undefined) snapshots.delete(oldest);
  }

  const value = loadSnapshot(rpcUrl, timeoutMs, limit).catch((error) => {
    snapshots.delete(key);
    throw error;
  });
  snapshots.set(key, {expiresAt: now + SNAPSHOT_TTL_MS, value});
  return value;
}

// Fetch a bounded, short-lived shared snapshot of recent PayAI fee-payer
// transactions. Only transfers in transactions whose first account key is the
// PayAI fee payer contribute to the observed metrics.
export async function getPaymentHistory(
  rpcUrl: string,
  timeoutMs: number,
  wallet: string,
  limit: number,
): Promise<PaymentHistory> {
  const lim = Number.isFinite(limit)
    ? Math.max(1, Math.min(Math.floor(limit), MAX_SIGNATURES))
    : 1;
  const snapshot = await transferSnapshot(rpcUrl, timeoutMs, lim);
  return computePaymentHistory(
    snapshot.transfers,
    wallet,
    PAYAI_FEE_PAYER,
    snapshot.coverage,
  );
}
