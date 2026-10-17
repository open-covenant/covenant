import { request } from "node:http";

// A card shows what the page itself declares, so the card route reads the page.
// It asks this server directly on its own port, not through the public
// hostname: that keeps Cloudflare out of the loop and works the same in
// development. Host-based routing (docs.*, stake.*) and the site's redirects
// still apply because every request carries the Host and scheme the browser
// would send.

export type PageCardData = {
  title: string;
  description?: string;
  section?: string;
  url?: string;
};

export type PageResponse = { status: number; html: string; host: string; path: string };

const MAX_REDIRECTS = 5;
const TIMEOUT_MS = 8000;
// Next renders metadata into <head> up front for crawlers; identify as one.
const USER_AGENT = "Twitterbot/1.0 (Covenant card renderer)";

export type Scheme = "http" | "https";

function get(port: number, host: string, path: string, proto: Scheme): Promise<{ status: number; location?: string; body: string }> {
  return new Promise((resolve, reject) => {
    const req = request(
      {
        host: "127.0.0.1",
        port,
        path,
        method: "GET",
        headers: { host, "user-agent": USER_AGENT, accept: "text/html", "x-forwarded-proto": proto },
        timeout: TIMEOUT_MS,
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c: Buffer) => chunks.push(c));
        res.on("end", () =>
          resolve({
            status: res.statusCode ?? 0,
            location: typeof res.headers.location === "string" ? res.headers.location : undefined,
            body: Buffer.concat(chunks).toString("utf8"),
          }),
        );
        res.on("error", reject);
      },
    );
    req.on("timeout", () => req.destroy(new Error(`timed out reading ${host}${path}`)));
    req.on("error", reject);
    req.end();
  });
}

// Follows redirects only within the site: the apex host and its subdomains.
export async function fetchPage(port: number, apexHost: string, path: string, proto: Scheme = "https"): Promise<PageResponse> {
  const apex = apexHost.replace(/^www\./, "");
  let host = apexHost;
  for (let hop = 0; hop <= MAX_REDIRECTS; hop++) {
    const res = await get(port, host, path, proto);
    if (res.status >= 300 && res.status < 400 && res.location) {
      const next = new URL(res.location, `${proto}://${host}`);
      if (next.host !== apex && !next.host.endsWith(`.${apex}`)) {
        return { status: 404, html: "", host, path };
      }
      host = next.host;
      path = `${next.pathname}${next.search}`;
      continue;
    }
    return { status: res.status, html: res.body, host, path };
  }
  return { status: 508, html: "", host, path };
}

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };

export function decodeEntities(text: string): string {
  return text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (whole, code: string) => {
    if (code[0] === "#") {
      const n = code[1] === "x" || code[1] === "X" ? parseInt(code.slice(2), 16) : parseInt(code.slice(1), 10);
      return Number.isFinite(n) ? String.fromCodePoint(n) : whole;
    }
    return ENTITIES[code.toLowerCase()] ?? whole;
  });
}

function metaTags(html: string): Map<string, string> {
  const tags = new Map<string, string>();
  for (const [, attrs] of html.matchAll(/<meta\s+([^>]*?)\/?>/gi)) {
    const a: Record<string, string> = {};
    for (const [, k, v] of attrs.matchAll(/([a-zA-Z:-]+)="([^"]*)"/g)) a[k.toLowerCase()] = v;
    const key = a.property ?? a.name;
    if (key && a.content !== undefined && !tags.has(key)) tags.set(key, decodeEntities(a.content));
  }
  return tags;
}

// The page's own card fields (set by pageMetadata), falling back to its social
// title and then its document title.
export function readCardData(html: string): PageCardData | null {
  const meta = metaTags(html);
  const docTitle = html.match(/<title>([^<]*)<\/title>/i)?.[1];
  const title = meta.get("covenant:card-title") ?? meta.get("og:title") ?? (docTitle ? decodeEntities(docTitle) : undefined);
  if (!title?.trim()) return null;
  return {
    title: title.trim(),
    description: (meta.get("og:description") ?? meta.get("description"))?.trim() || undefined,
    section: meta.get("covenant:card-section")?.trim() || undefined,
    url: meta.get("og:url")?.trim() || undefined,
  };
}
