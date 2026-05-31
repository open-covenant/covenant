import type { MetadataRoute } from "next";
import { headers } from "next/headers";

const APEX = "https://opencovenant.org";
const DOCS = "https://docs.opencovenant.org";

// Host-aware: served on both the apex and docs hosts (middleware skips
// dotted paths). The `host` directive reflects the requesting host, and
// both sitemaps are advertised so either host's robots.txt leads crawlers
// to the complete, self-consistent set.
export default async function robots(): Promise<MetadataRoute.Robots> {
  const host = (await headers()).get("host") ?? "opencovenant.org";
  const isDocs = /^docs\./i.test(host);
  return {
    rules: [{ userAgent: "*", allow: "/", disallow: ["/positions"] }],
    sitemap: [`${APEX}/sitemap.xml`, `${DOCS}/docs/sitemap.xml`],
    host: isDocs ? DOCS : APEX,
  };
}
