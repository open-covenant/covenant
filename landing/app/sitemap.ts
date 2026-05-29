import type { MetadataRoute } from "next";
import { DOCS_NAV } from "./docs/_nav";

const SITE = "https://opencovenant.org";
const DOCS = "https://docs.opencovenant.org";

export default function sitemap(): MetadataRoute.Sitemap {
  const now = new Date();
  const root: MetadataRoute.Sitemap = [
    { url: `${SITE}/`, lastModified: now, changeFrequency: "weekly", priority: 1 },
    { url: `${SITE}/roadmap`, lastModified: now, changeFrequency: "weekly", priority: 0.8 },
    { url: `${SITE}/contact`, lastModified: now, changeFrequency: "monthly", priority: 0.5 },
  ];
  const docs = DOCS_NAV.flatMap((section) =>
    section.items.map((item) => ({
      url: `${DOCS}${item.href === "/" ? "" : item.href}`,
      lastModified: now,
      changeFrequency: "weekly" as const,
      priority: 0.6,
    })),
  );
  return [...root, ...docs];
}
