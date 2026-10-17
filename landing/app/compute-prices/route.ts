import { applyPageMetadata } from "../_html-metadata";
import { fetchComputePage } from "../og/_compute";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET() {
  try {
    let source = await fetchComputePage(["prices", "page"]);
    // Deploy the public website first; old coordinators expose only /prices.
    // Do not mask any other failure or follow the later /prices redirect.
    if (source.status === 404) source = await fetchComputePage(["prices"]);
    if (source.status !== 200) throw new Error("Price history page unavailable");
    const html = applyPageMetadata(source.html, "/compute-prices", { title: "GPU price history", section: "Compute" }, {
      title: "GPU price history · Covenant Compute",
      description: "Compare hourly GPU provider quotes and settled rental rates, with timestamps, sample counts, explicit currencies and evidence.",
    });
    const headers = new Headers({ "content-type": "text/html; charset=utf-8", "cache-control": "no-cache" });
    for (const key of ["content-security-policy", "referrer-policy", "x-content-type-options"]) {
      const value = source.headers.get(key);
      if (value) headers.set(key, value);
    }
    return new Response(html, { headers });
  } catch (error) {
    console.error("[compute-prices] public page unavailable", error);
    return new Response("GPU price history is temporarily unavailable. Please try again shortly.", {
      status: 503, headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store" },
    });
  }
}
