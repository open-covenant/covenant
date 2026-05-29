const CVNT_MINT = "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";
const SOL_MINT = "So11111111111111111111111111111111111111112";
const PROBE_CVNT_RAW = 1_000_000;

export interface CvntPrice {
  solPerCvnt: number;
  fetchedAt: number;
}

export async function fetchCvntSolPrice(): Promise<CvntPrice | null> {
  try {
    const url =
      `https://lite-api.jup.ag/swap/v1/quote` +
      `?inputMint=${CVNT_MINT}` +
      `&outputMint=${SOL_MINT}` +
      `&amount=${PROBE_CVNT_RAW}` +
      `&slippageBps=300`;
    const r = await fetch(url, { cache: "no-store" });
    if (!r.ok) return null;
    const j = (await r.json()) as { outAmount?: string; inAmount?: string };
    const outLamports = Number(j.outAmount ?? "0");
    const inRaw = Number(j.inAmount ?? "0");
    if (!Number.isFinite(outLamports) || !Number.isFinite(inRaw)) return null;
    if (outLamports <= 0 || inRaw <= 0) return null;
    const solPerCvnt = outLamports / 1e9 / (inRaw / 1e6);
    if (!Number.isFinite(solPerCvnt) || solPerCvnt <= 0) return null;
    return { solPerCvnt, fetchedAt: Math.floor(Date.now() / 1000) };
  } catch {
    return null;
  }
}
