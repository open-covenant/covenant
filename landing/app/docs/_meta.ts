import type { Metadata } from "next";
import { SITE_BUILD_DATE, DOCS_PUBLISHED_DATE } from "../_brand";
import { pageMetadata } from "../_metadata";

const DOCS_HOST = "https://docs.opencovenant.org";
const SITE_HOST = "https://opencovenant.org";

// Per-page docs metadata helper. Centralises the SEO surface so every
// page gets a real canonical, openGraph, twitter card and its own generated
// social card instead of inheriting the layout's. Pass the slug without a
// leading slash (`getting-started`, `concepts`, etc.). The root docs page uses
// `""` for the index.
export function buildDocsMetadata(
  slug: string,
  title: string,
  description: string,
): Metadata {
  const url = slug ? `${DOCS_HOST}/${slug}` : DOCS_HOST;
  const fullTitle = slug ? `${title}: Covenant docs` : "Documentation: Covenant";
  return pageMetadata(
    slug ? `/docs/${slug}` : "/docs",
    slug ? { title, section: "Documentation" } : { title: "Documentation" },
    {
      title,
      description,
      alternates: { canonical: url },
      openGraph: { type: "article", url, siteName: "Covenant docs", title: fullTitle, description },
      twitter: { title: fullTitle, description },
    },
    { url },
  );
}

// Per-page docs JSON-LD: a TechArticle for the page plus a BreadcrumbList
// (Home → Documentation → Page). Organization, WebSite, and SoftwareApplication
// are emitted once in the root layout graph, so they are not repeated here.
export function buildDocsJsonLd(
  slug: string,
  title: string,
  description: string,
) {
  const url = slug ? `${DOCS_HOST}/${slug}` : DOCS_HOST;
  const crumbs: Array<Record<string, unknown>> = [
    { "@type": "ListItem", position: 1, name: "Home", item: `${SITE_HOST}/` },
    { "@type": "ListItem", position: 2, name: "Documentation", item: DOCS_HOST },
  ];
  if (slug) {
    crumbs.push({ "@type": "ListItem", position: 3, name: title, item: url });
  }
  return {
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "WebSite",
        "@id": `${DOCS_HOST}/#website`,
        name: "Covenant docs",
        url: DOCS_HOST,
        publisher: { "@id": `${SITE_HOST}/#org` },
      },
      {
        "@type": "TechArticle",
        "@id": `${url}#article`,
        headline: slug ? `${title}: Covenant docs` : "Documentation: Covenant",
        description,
        url,
        inLanguage: "en",
        datePublished: DOCS_PUBLISHED_DATE,
        dateModified: SITE_BUILD_DATE,
        isPartOf: { "@id": `${DOCS_HOST}/#website` },
        author: { "@id": `${SITE_HOST}/#org` },
        publisher: { "@id": `${SITE_HOST}/#org` },
        about: { "@id": `${SITE_HOST}/#software` },
      },
      { "@type": "BreadcrumbList", itemListElement: crumbs },
    ],
  };
}
