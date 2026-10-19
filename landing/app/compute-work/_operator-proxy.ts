import { COMPUTE_ORIGIN, COMPUTE_TIMEOUT_MS, readComputeBody } from "../og/_compute";
import { OPERATOR_MAX_BYTES, validateOperatorSnapshot } from "./_operator";

const headers = { "content-type": "application/json; charset=utf-8", "cache-control": "private, no-store, max-age=0", "x-content-type-options": "nosniff" };
export async function proxyOperator(query: URLSearchParams, fetcher: typeof fetch = fetch): Promise<Response> {
  if (query.size) return new Response(JSON.stringify({ error: "Unsupported pilot status request" }), { status: 400, headers });
  try {
    const upstream = await fetcher(`${COMPUTE_ORIGIN}/pool-operator`, { redirect: "manual", credentials: "omit", cache: "no-store",
      headers: { accept: "application/json", "user-agent": "Covenant-Pool-Operator/1" }, signal: AbortSignal.timeout(COMPUTE_TIMEOUT_MS) });
    if (upstream.status !== 200 || upstream.redirected || upstream.headers.has("set-cookie") ||
        upstream.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") {
      await upstream.body?.cancel();
      throw new Error("Public pilot status unavailable");
    }
    const data = validateOperatorSnapshot(JSON.parse(await readComputeBody(upstream, OPERATOR_MAX_BYTES)));
    return new Response(JSON.stringify(data), { headers });
  } catch {
    return new Response(JSON.stringify({ error: "Public pilot status temporarily unavailable" }), { status: 503, headers });
  }
}
