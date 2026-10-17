import type { PageResponse } from "./_page";

// Compute's public HTML pages share the site's renderer, but can only be read
// from this fixed origin. Never turn a card path or Host header into a URL.
export const COMPUTE_ORIGIN = "https://compute-api.opencovenant.org";
export const COMPUTE_MAX_BYTES = 1024 * 1024;
export const COMPUTE_TIMEOUT_MS = 8000;
const SEGMENT = /^[\w.-]{1,80}$/;

export function computePageUrl(segments: readonly string[]): string | null {
  if (!segments.length || segments.length > 8 ||
      !segments.every((segment) => SEGMENT.test(segment) && segment !== "." && segment !== "..")) return null;
  const path = segments.length === 1 && segments[0] === "index" ? "/" : `/${segments.join("/")}`;
  return `${COMPUTE_ORIGIN}${path}`;
}

export async function readComputeBody(response: Response, maxBytes = COMPUTE_MAX_BYTES): Promise<string> {
  if (Number(response.headers.get("content-length")) > maxBytes) {
    await response.body?.cancel();
    throw new Error("Compute source is too large");
  }
  const reader = response.body?.getReader();
  if (!reader) throw new Error("Compute source has no body");
  const chunks: Uint8Array[] = [];
  let bytes = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > maxBytes) throw new Error("Compute source is too large");
      chunks.push(value);
    }
  } finally {
    await reader.cancel();
    reader.releaseLock();
  }
  return Buffer.concat(chunks).toString("utf8");
}

export function publicComputeResponse(response: Response, contentType: string): boolean {
  return response.status === 200 && !response.redirected &&
    response.headers.get("content-type")?.split(";")[0].trim().toLowerCase() === contentType &&
    !response.headers.has("set-cookie") &&
    !/(?:^|,)\s*(?:private|no-store)\b/i.test(response.headers.get("cache-control") ?? "");
}

export async function fetchComputePage(segments: readonly string[], fetcher: typeof fetch = fetch): Promise<PageResponse & { headers: Headers }> {
  const url = computePageUrl(segments);
  if (!url) throw new Error("Invalid Compute card path");
  const response = await fetcher(url, {
    redirect: "manual",
    credentials: "omit",
    headers: { accept: "text/html", "user-agent": "Twitterbot/1.0 (Covenant card renderer)" },
    signal: AbortSignal.timeout(COMPUTE_TIMEOUT_MS),
  });
  const result = { status: response.status, html: "", host: new URL(COMPUTE_ORIGIN).host, path: new URL(url).pathname, headers: new Headers(response.headers) };
  if (response.status === 404 || response.status === 410) {
    await response.body?.cancel();
    return result;
  }
  // A redirect, login page response, JSON endpoint or private response cannot
  // become a publicly cached card. No cookies or authorization are sent.
  if (!publicComputeResponse(response, "text/html")) {
    await response.body?.cancel();
    throw new Error("Compute card source is not public HTML");
  }
  return { ...result, html: await readComputeBody(response) };
}
