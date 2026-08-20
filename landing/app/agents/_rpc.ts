// Mainnet RPC shared by the agent passport routes. The default fallback endpoint
// cannot answer DAS getAsset (it returns -32601 method-not-found), so a missing
// asset and an endpoint that can't read DAS look the same in a catch unless you
// classify the error. See isAssetNotFound.

export function rpcUrl(): string {
  return (
    process.env.COVENANT_SOLANA_MAINNET_RPC_URL ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ||
    "https://api.mainnet-beta.solana.com"
  );
}

export async function rpc(method: string, params: unknown): Promise<unknown> {
  const res = await fetch(rpcUrl(), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
    signal: AbortSignal.timeout(8000),
  });
  if (!res.ok) throw new Error(`rpc ${method} http ${res.status}`);
  const body = (await res.json()) as { result?: unknown; error?: { message?: string } };
  if (body.error) throw new Error(`rpc ${method}: ${body.error.message ?? "error"}`);
  return body.result;
}

// True only for a genuine DAS asset-not-found: Helius answers getAsset with a
// JSON-RPC error naming the missing asset, or a null result. "Method not found"
// / -32601 is the opposite: the endpoint can't do DAS at all (a config error),
// which is a transport failure (502), never a 404.
export function isAssetNotFound(err: unknown): boolean {
  if (err == null) return true;
  const s = String(err);
  if (/-32601|method not found/i.test(s)) return false;
  return /asset\s*not\s*found|record\s*not\s*found/i.test(s);
}
