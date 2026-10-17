import { COMPUTE_ORIGIN, COMPUTE_TIMEOUT_MS, publicComputeResponse, readComputeBody } from "../og/_compute";

const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const PROOF = new RegExp(`^/prices/proofs/${UUID}$`);
const JSON_MAX_BYTES = 4 * 1024 * 1024;

export async function proxyPriceData(path: string, query: URLSearchParams, fetcher: typeof fetch = fetch): Promise<Response> {
  if (path !== "/prices/history" && !PROOF.test(path)) return new Response("Not found", { status: 404 });
  const safeQuery = new URLSearchParams();
  for (const [key, value] of query) {
    if (path !== "/prices/history" || !["since_ms", "until_ms"].includes(key) || safeQuery.has(key) ||
        !/^\d{1,20}$/.test(value) || BigInt(value) > 18446744073709551615n) {
      return new Response("Invalid price-history query", { status: 400 });
    }
    safeQuery.set(key, value);
  }
  const suffix = safeQuery.size ? `?${safeQuery}` : "";
  try {
    const upstream = await fetcher(`${COMPUTE_ORIGIN}${path}${suffix}`, {
      redirect: "manual", credentials: "omit", cache: "no-store",
      headers: { accept: "application/json" }, signal: AbortSignal.timeout(COMPUTE_TIMEOUT_MS),
    });
    if ([400, 404, 410, 429].includes(upstream.status)) {
      await upstream.body?.cancel();
      return new Response("Price-history request unavailable", { status: upstream.status, headers: { "cache-control": "no-store" } });
    }
    if (!publicComputeResponse(upstream, "application/json")) {
      await upstream.body?.cancel();
      throw new Error("Price data is not public JSON");
    }
    const body = await readComputeBody(upstream, JSON_MAX_BYTES);
    JSON.parse(body);
    return new Response(body, { headers: { "content-type": "application/json; charset=utf-8", "cache-control": "no-cache", "x-content-type-options": "nosniff" } });
  } catch (error) {
    console.error("[compute-prices] public data unavailable", error);
    return new Response("Price data temporarily unavailable", { status: 503, headers: { "cache-control": "no-store" } });
  }
}
