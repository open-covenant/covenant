import type { MetadataRoute } from "next";

export default function robots(): MetadataRoute.Robots {
  return {
    rules: [{ userAgent: "*", allow: "/" }],
    sitemap: "https://opencovenant.org/sitemap.xml",
    host: "https://opencovenant.org",
  };
}
