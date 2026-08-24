// PayAI settlement-grounded reputation, computed from public on-chain USDC
// settlements on Solana. Faithful port of the covenant-payai Rust crate
// (index.rs parse_settlements + reputation.rs compute_reputation); reputation.test.ts
// pins the two to identical output on shared fixtures so the numbers can't drift.
// Read-only. Never touches the payment flow.

const USDC_MAINNET = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const USDC_DEVNET = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
const TOKEN_PROGRAM_ID = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
export const PAYAI_FEE_PAYER = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";

const isUsdc = (m: string): boolean => m === USDC_MAINNET || m === USDC_DEVNET;

export interface Settlement {
  signature: string;
  slot: number;
  blockTime: number | null;
  mint: string;
  payer: string;
  payTo: string;
  amountMicro: bigint;
}

export type Tier = "bronze" | "silver" | "gold" | "platinum";

export interface Reputation {
  wallet: string;
  settled_jobs: number;
  distinct_counterparties: number;
  volume_micro_usdc: number;
  tier: Tier;
  score: number;
  source_fee_payer: string;
}

function tierFromMicro(v: bigint): Tier {
  const usdc = 1_000_000n;
  if (v >= 100_000n * usdc) return "platinum";
  if (v >= 10_000n * usdc) return "gold";
  if (v >= 1_000n * usdc) return "silver";
  return "bronze";
}

// jobs (<=400, 4/job capped at 100), distinct counterparties (<=200, 4 each
// capped at 50), volume (<=400, 1 point per $100 capped at $40k). Cap 1000.
function score(jobs: number, distinct: number, volMicro: bigint): number {
  const jobsScore = Math.min(jobs, 100) * 4;
  const cpScore = Math.min(distinct, 50) * 4;
  const volUsdc = Number(volMicro / 1_000_000n);
  const volScore = Math.min(Math.floor(volUsdc / 100), 400);
  return Math.min(jobsScore + cpScore + volScore, 1000);
}

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

async function rpcOnce(url: string, timeoutMs: number, method: string, params: unknown): Promise<any> {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
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

// Shared RPC endpoints rate-limit bursts of getTransaction; back off and retry
// instead of surfacing a transient 429/5xx to the caller.
async function rpc(url: string, timeoutMs: number, method: string, params: unknown): Promise<any> {
  let lastError: unknown;
  for (let attempt = 0; attempt < 3; attempt++) {
    if (attempt > 0) await sleep(300 * 4 ** (attempt - 1));
    try {
      return await rpcOnce(url, timeoutMs, method, params);
    } catch (error) {
      lastError = error;
      const message = error instanceof Error ? error.message : "";
      if (!/status (429|5\d\d)$/.test(message)) throw error;
    }
  }
  throw lastError;
}

function accountKeys(tx: any): string[] {
  const arr = tx?.transaction?.message?.accountKeys;
  if (!Array.isArray(arr)) return [];
  return arr
    .map((k: any) => (typeof k === "string" ? k : k?.pubkey))
    .filter((s: any): s is string => typeof s === "string");
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

// Parse every USDC settlement from a jsonParsed transaction. Empty for a failed
// or non-USDC transaction. Attributes transfers to OWNER wallets, not ATAs.
export function parseSettlements(tx: any, signature: string): Settlement[] {
  if (tx?.meta?.err != null) return [];
  const slot = typeof tx?.slot === "number" ? tx.slot : 0;
  const blockTime = typeof tx?.blockTime === "number" ? tx.blockTime : null;
  const keys = accountKeys(tx);
  const ownerByAta = balanceMap(tx, keys, "owner");
  const mintByAta = balanceMap(tx, keys, "mint");

  const out: Settlement[] = [];
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

    const mint =
      (typeof info.mint === "string" ? info.mint : undefined) ??
      mintByAta.get(dest) ??
      mintByAta.get(source);
    if (!mint || !isUsdc(mint)) continue;

    const amountStr = typ === "transferChecked" ? info?.tokenAmount?.amount : info?.amount;
    let amountMicro: bigint;
    try {
      amountMicro = BigInt(amountStr ?? "0");
    } catch {
      amountMicro = 0n;
    }
    if (amountMicro === 0n) continue;

    const payer =
      (typeof info.authority === "string" && info.authority) || ownerByAta.get(source) || source;
    const payTo = ownerByAta.get(dest) ?? dest;

    out.push({ signature, slot, blockTime, mint, payer, payTo, amountMicro });
  }
  return out;
}

// Inbound settlements from OTHER wallets only (payTo == wallet, payer != wallet).
// Self-payments are excluded so a wallet can't inflate its own numbers.
export function computeReputation(
  settlements: Settlement[],
  wallet: string,
  feePayer: string,
): Reputation {
  let jobs = 0;
  let volume = 0n;
  const counterparties = new Set<string>();
  for (const s of settlements) {
    if (s.payTo === wallet && s.payer !== wallet) {
      jobs += 1;
      volume += s.amountMicro;
      counterparties.add(s.payer);
    }
  }
  return {
    wallet,
    settled_jobs: jobs,
    distinct_counterparties: counterparties.size,
    volume_micro_usdc: Number(volume),
    tier: tierFromMicro(volume),
    score: score(jobs, counterparties.size, volume),
    source_fee_payer: feePayer,
  };
}

const SNAPSHOT_TTL_MS = 120_000;
const SCAN_PACE_MS = 40;

interface SettlementSnapshot {
  settlements: Settlement[];
  fetchedAt: number;
}

let snapshot: { key: string; value: SettlementSnapshot } | null = null;
let refreshing: Promise<SettlementSnapshot> | null = null;

// One getTransaction per signature, paced to stay under shared rate limits. A
// transaction that stays unavailable after retries narrows coverage but must
// not sink the scan. Solana caps getSignaturesForAddress at 1000, so a single
// scan can't fan out unbounded.
async function scanSettlements(rpcUrl: string, timeoutMs: number, lim: number): Promise<SettlementSnapshot> {
  const sigs = await rpc(rpcUrl, timeoutMs, "getSignaturesForAddress", [PAYAI_FEE_PAYER, { limit: lim }]);
  if (!Array.isArray(sigs)) throw new Error("getSignaturesForAddress: result not an array");
  const settlements: Settlement[] = [];
  for (const e of sigs) {
    if (e?.err != null) continue;
    const sig = e?.signature;
    if (typeof sig !== "string") continue;
    try {
      const tx = await rpc(rpcUrl, timeoutMs, "getTransaction", [
        sig,
        { encoding: "jsonParsed", maxSupportedTransactionVersion: 0, commitment: "confirmed" },
      ]);
      if (tx != null) settlements.push(...parseSettlements(tx, sig));
    } catch {
      // skip; settlements are append-only on chain, the next refresh sees it
    }
    await sleep(SCAN_PACE_MS);
  }
  return { settlements, fetchedAt: Date.now() };
}

// Serve the last snapshot immediately and refresh in the background once it is
// stale. The scan is per-fee-payer, not per-wallet, so every reputation call
// shares it; a full rescan is seconds long and must never sit on the request
// path of an MCP client with a short timeout.
function snapshotSettlements(rpcUrl: string, timeoutMs: number, lim: number): Promise<SettlementSnapshot> {
  const key = `${rpcUrl}\n${lim}`;
  const current = snapshot?.key === key ? snapshot.value : null;
  const fresh = current !== null && Date.now() - current.fetchedAt < SNAPSHOT_TTL_MS;
  if (!fresh && refreshing === null) {
    refreshing = scanSettlements(rpcUrl, timeoutMs, lim)
      .then((value) => {
        snapshot = { key, value };
        return value;
      })
      .finally(() => {
        refreshing = null;
      });
    refreshing.catch(() => {});
  }
  return current !== null ? Promise.resolve(current) : refreshing!;
}

export function warmReputation(rpcUrl: string, timeoutMs: number, limit: number): void {
  const lim = Math.max(1, Math.min(Math.floor(limit), 1000));
  void snapshotSettlements(rpcUrl, timeoutMs, lim).catch(() => {});
}

// Score `wallet` against the shared settlement snapshot for the fee payer.
export async function getReputation(
  rpcUrl: string,
  timeoutMs: number,
  wallet: string,
  limit: number,
): Promise<Reputation> {
  const lim = Math.max(1, Math.min(Math.floor(limit), 1000));
  const snap = await snapshotSettlements(rpcUrl, timeoutMs, lim);
  return computeReputation(snap.settlements, wallet, PAYAI_FEE_PAYER);
}
