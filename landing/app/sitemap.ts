import type { MetadataRoute } from "next";
import { SITE_BUILD_DATE } from "./_brand";

const SITE = "https://opencovenant.org";

// Apex sitemap. Docs URLs live in their own host-scoped sitemap at
// /docs/sitemap.xml (app/docs/sitemap.ts) so each host advertises a
// self-consistent set rather than the same merged list on both hosts.
export default function sitemap(): MetadataRoute.Sitemap {
  const lastModified = SITE_BUILD_DATE;
  return [
    { url: `${SITE}/`, lastModified, changeFrequency: "weekly", priority: 1 },
    { url: `${SITE}/stake`, lastModified, changeFrequency: "weekly", priority: 0.9 },
    { url: `${SITE}/treasury`, lastModified, changeFrequency: "daily", priority: 0.7 },
    { url: `${SITE}/roadmap`, lastModified, changeFrequency: "weekly", priority: 0.8 },
    { url: `${SITE}/contact`, lastModified, changeFrequency: "monthly", priority: 0.5 },
    { url: `${SITE}/privacy`, lastModified, changeFrequency: "monthly", priority: 0.3 },
    { url: `${SITE}/terms`, lastModified, changeFrequency: "monthly", priority: 0.3 },
  ];
}
