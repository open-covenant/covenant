import type { Metadata } from "next";

const DOCS_HOST = "https://docs.opencovenant.org";

// Per-page docs metadata helper. Centralises the SEO surface so every
// page gets a real canonical, openGraph, and twitter card instead of
// inheriting the layout's `alternates: { canonical: "/" }`. Pass the
// slug without a leading slash (`getting-started`, `concepts`, etc.).
// The root docs page uses `""` for the index.
export function buildDocsMetadata(
  slug: string,
  title: string,
  description: string,
): Metadata {
  const path = slug ? `/${slug}` : "/";
  const url = slug ? `${DOCS_HOST}/${slug}` : DOCS_HOST;
  const fullTitle = slug ? `${title} — Covenant docs` : "Documentation — Covenant";
  return {
    title,
    description,
    alternates: { canonical: path },
    openGraph: {
      type: "article",
      url,
      siteName: "Covenant docs",
      title: fullTitle,
      description,
    },
    twitter: {
      card: "summary_large_image",
      site: "@OpenCovenant",
      title: fullTitle,
      description,
    },
  };
}
