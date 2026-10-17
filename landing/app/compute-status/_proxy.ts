import { COMPUTE_ORIGIN, COMPUTE_TIMEOUT_MS, readComputeBody } from "../og/_compute";
import { STATUS_MAX_BYTES, validateStatus } from "./_status";

const headers = { "content-type": "application/json; charset=utf-8", "cache-control": "private, no-store, max-age=0", "x-content-type-options": "nosniff" };
const unavailable = () => new Response(JSON.stringify({ error: "Status data temporarily unavailable" }), { status: 503, headers });

export async function proxyStatus(query: URLSearchParams, fetcher: typeof fetch = fetch): Promise<Response> {
  if (query.size) return new Response(JSON.stringify({ error: "Status queries are not supported" }), { status: 400, headers });
  try {
    const upstream = await fetcher(`${COMPUTE_ORIGIN}/status`, {
      redirect: "manual", credentials: "omit", cache: "no-store", headers: { accept: "application/json" }, signal: AbortSignal.timeout(COMPUTE_TIMEOUT_MS),
    });
    if (upstream.status !== 200 || upstream.redirected || upstream.headers.has("set-cookie") ||
        upstream.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") {
      await upstream.body?.cancel();
      return unavailable();
    }
    const report = validateStatus(JSON.parse(await readComputeBody(upstream, STATUS_MAX_BYTES)));
    return new Response(JSON.stringify(report), { headers });
  } catch {
    // Do not reflect upstream bodies, exception text or infrastructure details.
    return unavailable();
  }
}
