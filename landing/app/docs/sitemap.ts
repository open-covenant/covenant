import type { MetadataRoute } from "next";
import { DOCS_NAV } from "./_nav";
import { SITE_BUILD_DATE } from "../_brand";

const DOCS = "https://docs.opencovenant.org";

// Served at /docs/sitemap.xml. Resolves on both hosts (middleware skips
// dotted paths), and the docs host's robots.txt points crawlers here.
export default function sitemap(): MetadataRoute.Sitemap {
  return DOCS_NAV.flatMap((section) =>
    section.items.map((item) => ({
      url: `${DOCS}${item.href === "/" ? "" : item.href}`,
      lastModified: SITE_BUILD_DATE,
      changeFrequency: "weekly" as const,
      priority: item.href === "/" ? 0.8 : 0.6,
    })),
  );
}
