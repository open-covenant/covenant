import type { Metadata } from "next";

// Page metadata with a generated social card. Every page exports its metadata
// through pageMetadata (docs pages through buildDocsMetadata, which uses it):
//
//   export const metadata: Metadata = pageMetadata("/brand/color", { section: "Brand guidelines" }, {
//     title: "Color: Covenant brand guidelines",
//     description: "…",
//   });
//
// The page gets its canonical URL, Open Graph and Twitter tags, and an
// og:image pointing at /og/<path>.png, which app/og draws from the card title
// (the page title unless given) and section declared here. `pnpm check:cards`
// fails the build when a page ships without one.

export const SITE_URL = "https://opencovenant.org";
const SITE_NAME = "Covenant";
const X_HANDLE = "@OpenCovenant";

export const CARD_SIZE = { width: 1200, height: 630 } as const;

export type PageCard = {
  /** The headline on the card. Defaults to the page title; usually the page's h1. */
  title?: string;
  /** Small label top right: the part of the site the page belongs to. */
  section?: string;
};

// The home page keeps its own share card; every other page gets a generated one.
export function cardImageUrl(path: string): string {
  if (path === "/") return `${SITE_URL}/opengraph-image.jpg`;
  return `${SITE_URL}/og${path.replace(/\/$/, "")}.png`;
}

// The card's alt text says what the image shows: whose it is, the section and
// the headline ("Covenant documentation: Concepts", "Covenant: $CVNT").
export function cardAlt(title: string | undefined, section?: string): string {
  if (!title) return SITE_NAME;
  if (section) {
    const owner = /^covenant\b/i.test(section) ? section : `${SITE_NAME} ${section.toLowerCase()}`;
    return `${owner}: ${title}`;
  }
  return /\bcovenant\b/i.test(title) ? title : `${SITE_NAME}: ${title}`;
}

export function plainTitle(title: Metadata["title"]): string | undefined {
  if (!title) return undefined;
  if (typeof title === "string") return title;
  if ("absolute" in title && title.absolute) return title.absolute;
  if ("default" in title) return title.default;
  return undefined;
}

export function pageMetadata(
  path: string,
  card: PageCard,
  metadata: Metadata,
  { url = path === "/" ? SITE_URL : `${SITE_URL}${path}` }: { url?: string } = {},
): Metadata {
  const title = plainTitle(metadata.title);
  const og = metadata.openGraph ?? {};
  const socialTitle = plainTitle(og.title) ?? title;
  const description = og.description ?? metadata.description ?? undefined;
  const cardTitle = card.title ?? title;
  const image = { url: cardImageUrl(path), ...CARD_SIZE, alt: cardAlt(cardTitle, card.section) };

  return {
    ...metadata,
    alternates: { canonical: url, ...metadata.alternates },
    openGraph: {
      type: "website",
      siteName: SITE_NAME,
      url,
      title: socialTitle,
      description,
      ...og,
      images: [image],
    } as Metadata["openGraph"],
    twitter: {
      card: "summary_large_image",
      site: X_HANDLE,
      creator: X_HANDLE,
      title: socialTitle,
      description,
      ...metadata.twitter,
      images: [{ url: image.url, alt: image.alt }],
    } as Metadata["twitter"],
    other: {
      ...metadata.other,
      ...(cardTitle ? { "covenant:card-title": cardTitle } : {}),
      ...(card.section ? { "covenant:card-section": card.section } : {}),
    },
  };
}
