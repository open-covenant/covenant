import { describe, expect, it } from "vitest";
import { DOCS_PUBLISHED_DATE, SITE_BUILD_DATE } from "../../_brand";
import { buildDocsJsonLd, buildDocsMetadata } from "../_meta";

const DOCS_HOST = "https://docs.opencovenant.org";
const SITE_HOST = "https://opencovenant.org";


type GraphNode = Record<string, unknown>;
const graphOf = (ld: ReturnType<typeof buildDocsJsonLd>) =>
  (ld as { "@graph": GraphNode[] })["@graph"];
const nodeOfType = (ld: ReturnType<typeof buildDocsJsonLd>, type: string) =>
  graphOf(ld).find((n) => n["@type"] === type) as GraphNode;

describe("buildDocsMetadata", () => {
  it("builds a slug-scoped canonical, raw page title, and suffixed social cards", () => {
    const m = buildDocsMetadata("getting-started", "Getting started", "Install and run.");

    expect(m.title).toBe("Getting started");
    expect(m.description).toBe("Install and run.");
    expect(m.alternates?.canonical).toBe(`${DOCS_HOST}/getting-started`);
    expect(m.openGraph?.url).toBe(`${DOCS_HOST}/getting-started`);
    expect((m.openGraph as { title?: string })?.title).toBe("Getting started: Covenant docs");
    expect((m.twitter as { title?: string })?.title).toBe("Getting started: Covenant docs");

    const ogImages = m.openGraph?.images as Array<Record<string, unknown>>;
    expect(ogImages[0]).toEqual({
      url: `${SITE_HOST}/og/docs/getting-started.png`,
      width: 1200,
      height: 630,
      alt: "Covenant documentation: Getting started",
    });
    expect((m.twitter as { images?: Array<{ url: string }> })?.images?.[0]?.url).toBe(
      `${SITE_HOST}/og/docs/getting-started.png`,
    );
    expect(m.other).toEqual({ "covenant:card-title": "Getting started", "covenant:card-section": "Documentation" });
  });

  it("uses the docs root url and index title when the slug is empty", () => {
    const m = buildDocsMetadata("", "Documentation", "All the docs.");

    expect(m.alternates?.canonical).toBe(DOCS_HOST);
    expect(m.openGraph?.url).toBe(DOCS_HOST);
    expect((m.openGraph as { title?: string })?.title).toBe("Documentation: Covenant");
    expect((m.twitter as { title?: string })?.title).toBe("Documentation: Covenant");
    expect((m.openGraph?.images as Array<{ url: string }>)[0].url).toBe(`${SITE_HOST}/og/docs.png`);
    expect(m.other).toEqual({ "covenant:card-title": "Documentation" });
  });
});

describe("buildDocsJsonLd", () => {
  it("emits a three-crumb breadcrumb and a dated TechArticle for a slug page", () => {
    const ld = buildDocsJsonLd("concepts", "Concepts", "Core ideas.");

    const article = nodeOfType(ld, "TechArticle");
    expect(article.headline).toBe("Concepts: Covenant docs");
    expect(article.url).toBe(`${DOCS_HOST}/concepts`);
    expect(article.datePublished).toBe(DOCS_PUBLISHED_DATE);
    expect(article.dateModified).toBe(SITE_BUILD_DATE);

    const crumbs = nodeOfType(ld, "BreadcrumbList").itemListElement as Array<Record<string, unknown>>;
    expect(crumbs.map((c) => c.name)).toEqual(["Home", "Documentation", "Concepts"]);
    expect(crumbs[2].item).toBe(`${DOCS_HOST}/concepts`);
  });

  it("omits the page crumb and uses the index headline at the docs root", () => {
    const ld = buildDocsJsonLd("", "Documentation", "All the docs.");

    const article = nodeOfType(ld, "TechArticle");
    expect(article.headline).toBe("Documentation: Covenant");
    expect(article.url).toBe(DOCS_HOST);

    const crumbs = nodeOfType(ld, "BreadcrumbList").itemListElement as Array<Record<string, unknown>>;
    expect(crumbs.map((c) => c.name)).toEqual(["Home", "Documentation"]);
  });
});
