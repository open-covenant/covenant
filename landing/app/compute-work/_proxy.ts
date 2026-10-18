import { COMPUTE_ORIGIN, COMPUTE_TIMEOUT_MS, readComputeBody } from "../og/_compute";
import { validateDetail, validateManifest, validateSnapshot, WORK_ID, WORK_MAX_BYTES, type WorkDetail } from "./_work";

const headers = { "content-type": "application/json; charset=utf-8", "cache-control": "private, no-store, max-age=0", "x-content-type-options": "nosniff" };
const unavailable = () => new Response(JSON.stringify({ error: "Public work data temporarily unavailable" }), { status: 503, headers });

export async function readPublicWork(kind: "snapshot" | "detail" | "manifest", id?: string, fetcher: typeof fetch = fetch) {
  if (kind !== "snapshot" && (!id || !WORK_ID.test(id))) throw new Error("Invalid work identifier");
  const path = kind === "snapshot" ? "/community-work" : `/community-work/${id}${kind === "manifest" ? "/manifest" : ""}`;
  const upstream = await fetcher(`${COMPUTE_ORIGIN}${path}`, {
    redirect: "manual", credentials: "omit", cache: "no-store", headers: { accept: "application/json" }, signal: AbortSignal.timeout(COMPUTE_TIMEOUT_MS),
  });
  if (upstream.status !== 200 || upstream.redirected || upstream.headers.has("set-cookie") ||
      upstream.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") {
    await upstream.body?.cancel();
    throw new Error("Public work data unavailable");
  }
  const value: unknown = JSON.parse(await readComputeBody(upstream, WORK_MAX_BYTES));
  return kind === "snapshot" ? validateSnapshot(value) : kind === "detail" ? validateDetail(value, id) : validateManifest(value, id!);
}

export async function readWorkDetail(id: string, fetcher: typeof fetch = fetch): Promise<WorkDetail | null> {
  try { return await readPublicWork("detail", id, fetcher) as WorkDetail; } catch { return null; }
}

export async function proxyWork(query: URLSearchParams, kind: "snapshot" | "detail" | "manifest" = "snapshot", id?: string, fetcher: typeof fetch = fetch): Promise<Response> {
  if (query.size || (kind !== "snapshot" && (!id || !WORK_ID.test(id)))) {
    return new Response(JSON.stringify({ error: "Unsupported public work request" }), { status: 400, headers });
  }
  try {
    const report = await readPublicWork(kind, id, fetcher);
    return new Response(JSON.stringify(report), { headers: { ...headers, ...(kind === "manifest" ? { "content-disposition": `attachment; filename="${id}-manifest.json"` } : {}) } });
  } catch {
    // Upstream error bodies and exception details may contain private data.
    return unavailable();
  }
}
