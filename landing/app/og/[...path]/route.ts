import { renderCard } from "../_card";
import { fetchPage, readCardData } from "../_page";

// /og/<page path>.png: the social card for any page on the site, drawn from the
// title and section the page declares (see app/_metadata.ts). New pages get a
// card without anyone making one.

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const SEGMENT = /^[\w.-]{1,80}$/;
// Card URLs that were shared before cards were generated. The site's own
// redirects cover the rest (/og/covenant-compute.jpg lands on /blog/compute).
const LEGACY: Record<string, string> = {
  "/covenant-is-now-multichain": "/blog/covenant-is-now-multichain",
};
const CACHE_TTL_MS = 6 * 60 * 60 * 1000;
const CACHE_MAX = 300;
const cache = new Map<string, { png: ArrayBuffer; at: number }>();

const notFound = () => new Response("Not found", { status: 404, headers: { "cache-control": "public, max-age=300" } });

function png(body: ArrayBuffer) {
  return new Response(body, {
    headers: {
      "content-type": "image/png",
      "cache-control": "public, max-age=3600, s-maxage=86400, stale-while-revalidate=604800",
    },
  });
}

export async function GET(req: Request, { params }: { params: Promise<{ path: string[] }> }) {
  const segments = [...(await params).path];
  if (segments.length > 8 || !segments.every((s) => SEGMENT.test(s))) return notFound();
  segments[segments.length - 1] = segments[segments.length - 1].replace(/\.(png|jpe?g)$/i, "");
  if (!segments[segments.length - 1]) return notFound();
  const requested = `/${segments.join("/")}`;
  const pagePath = LEGACY[requested] ?? requested;

  const hit = cache.get(pagePath);
  if (hit && Date.now() - hit.at < CACHE_TTL_MS) return png(hit.png);

  const host = (req.headers.get("x-forwarded-host") ?? req.headers.get("host") ?? "opencovenant.org").replace(
    /^(www|docs|stake|legacy-stake)\./i,
    "",
  );
  const port = Number(process.env.PORT) || Number(new URL(req.url).port) || 3000;
  // Pages that call the site's own API build the URL from this; a local server
  // speaks plain http.
  const proto = req.headers.get("x-forwarded-proto") === "http" ? "http" : "https";

  let page;
  try {
    page = await fetchPage(port, host, pagePath, proto);
  } catch (err) {
    console.error(`[og] could not read ${pagePath}:`, err);
    return new Response("Card unavailable", { status: 503, headers: { "cache-control": "no-store" } });
  }
  if (page.status === 404 || page.status === 410) return notFound();
  if (page.status !== 200) {
    return new Response("Card unavailable", { status: 503, headers: { "cache-control": "no-store" } });
  }
  const data = readCardData(page.html);
  if (!data) return notFound();

  const image = await renderCard({
    title: data.title,
    section: data.section,
    url: data.url ?? `https://${page.host}${page.path}`,
    key: pagePath,
  });
  const body = await image.arrayBuffer();

  if (cache.size >= CACHE_MAX) cache.delete(cache.keys().next().value!);
  cache.set(pagePath, { png: body, at: Date.now() });
  return png(body);
}
