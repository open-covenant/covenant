export type AttestationPayload = {
  schema: string;
  rootHashHex: string;
  releaseTarget: string;
  releaseSubject: string;
  releaseScope: string;
  recordedAt: number;
};

export function rpcUrl(): string {
  return (
    process.env.COVENANT_SOLANA_MAINNET_RPC_URL ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ||
    "https://api.mainnet-beta.solana.com"
  );
}

// Helius indexes the AppData JSON with snake_cased keys; the on-chain bytes
// are camelCase. Accept either so the passport never depends on an
// indexer's casing choice.
export function normalizePayload(raw: Record<string, unknown>): AttestationPayload | null {
  const pick = (camel: string, snake: string): unknown => raw[camel] ?? raw[snake];
  const schema = pick("schema", "schema");
  const root = pick("rootHashHex", "root_hash_hex");
  if (typeof schema !== "string" || typeof root !== "string") return null;
  return {
    schema,
    rootHashHex: root,
    releaseTarget: String(pick("releaseTarget", "release_target") ?? ""),
    releaseSubject: String(pick("releaseSubject", "release_subject") ?? ""),
    releaseScope: String(pick("releaseScope", "release_scope") ?? ""),
    recordedAt: Number(pick("recordedAt", "recorded_at") ?? 0),
  };
}
