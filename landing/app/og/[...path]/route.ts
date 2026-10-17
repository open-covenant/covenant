import { renderCard } from "../_card";
import { fetchPage, readCardData } from "../_page";
import { computePageUrl, fetchComputePage } from "../_compute";
import { CardPipeline, CardQueueFull } from "../_pipeline";

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
const pipeline = new CardPipeline();
class CardNotFound extends Error {}

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
  const maxSegments = segments[0] === "compute-api" ? 9 : 8;
  if (!segments.length || segments.length > maxSegments) return notFound();
  if (segments[0] !== "compute-api" && !segments.every((s) => SEGMENT.test(s))) return notFound();
  segments[segments.length - 1] = segments[segments.length - 1].replace(/\.(png|jpe?g)$/i, "");
  if (!segments.every((s) => SEGMENT.test(s))) return notFound();
  const requested = `/${segments.join("/")}`;
  const pagePath = LEGACY[requested] ?? requested;
  const computeSegments = segments[0] === "compute-api" ? segments.slice(1) : null;
  const computeUrl = computeSegments ? computePageUrl(computeSegments) : null;
  if (computeSegments && !computeUrl) return notFound();

  const host = (req.headers.get("x-forwarded-host") ?? req.headers.get("host") ?? "opencovenant.org").replace(
    /^(www|docs|stake|legacy-stake)\./i,
    "",
  );
  const port = Number(process.env.PORT) || Number(new URL(req.url).port) || 3000;
  // Pages that call the site's own API build the URL from this; a local server
  // speaks plain http.
  const proto = req.headers.get("x-forwarded-proto") === "http" ? "http" : "https";

  try {
    const body = await pipeline.getOrRender(pagePath, async () => {
      const page = computeSegments
        ? await fetchComputePage(computeSegments)
        : await fetchPage(port, host, pagePath, proto);
      if (page.status === 404 || page.status === 410) throw new CardNotFound();
      if (page.status !== 200) throw new Error(`Card source returned ${page.status}`);
      const data = readCardData(page.html);
      if (!data) throw new CardNotFound();

      const image = await renderCard({
        title: computeSegments ? data.title.replace(/\s*[|:·—–]\s*Covenant Compute\s*$/i, "") : data.title,
        section: data.section ?? (computeSegments ? "Compute" : undefined),
        // The public source cannot substitute another canonical address on a card.
        url: computeUrl ?? data.url ?? `https://${page.host}${page.path}`,
        key: pagePath,
      });
      return image.arrayBuffer();
    });
    return png(body);
  } catch (err) {
    if (err instanceof CardNotFound) return notFound();
    if (err instanceof CardQueueFull) return new Response("Card renderer is busy; retry shortly", {
      status: 503, headers: { "cache-control": "no-store", "retry-after": "2" },
    });
    console.error(`[og] could not read ${pagePath}:`, err);
    return new Response("Card unavailable", { status: 503, headers: { "cache-control": "no-store" } });
  }
}
