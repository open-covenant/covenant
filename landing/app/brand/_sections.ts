// The brand guidelines' table of contents: one entry per page, each with the
// parts its sidebar lists and links to.

export type BrandPart = { id: string; title: string };

export type BrandSection = {
  slug: string;
  href: string;
  n: string;
  title: string;
  summary: string;
  parts: BrandPart[];
};

export const BRAND_SECTIONS: BrandSection[] = [
  {
    slug: "introduction",
    href: "/brand",
    n: "01",
    title: "Introduction",
    summary: "Principles, contents and downloads.",
    parts: [
      { id: "sections", title: "Sections" },
      { id: "principles", title: "Principles" },
      { id: "assets", title: "Brand assets" },
    ],
  },
  {
    slug: "messaging",
    href: "/brand/messaging",
    n: "02",
    title: "Messaging",
    summary: "Positioning, voice and vocabulary.",
    parts: [
      { id: "positioning", title: "Positioning" },
      { id: "voice", title: "Voice" },
      { id: "tone", title: "Tone by audience" },
      { id: "conventions", title: "Style conventions" },
      { id: "taglines", title: "Taglines" },
      { id: "boilerplate", title: "Boilerplate" },
      { id: "vocabulary", title: "Vocabulary" },
      { id: "examples", title: "In practice" },
    ],
  },
  {
    slug: "logo",
    href: "/brand/logo",
    n: "03",
    title: "Logo",
    summary: "The mark and the lockup, and how to use them.",
    parts: [
      { id: "mark", title: "The mark" },
      { id: "construction", title: "Construction" },
      { id: "core", title: "The core" },
      { id: "color", title: "Color" },
      { id: "lockup", title: "Lockup" },
      { id: "lockup-construction", title: "Lockup construction" },
      { id: "clear-space", title: "Clear space" },
      { id: "sizes", title: "Minimum sizes" },
      { id: "icons", title: "Favicon & app icons" },
      { id: "misuse", title: "Misuse" },
      { id: "files", title: "Files" },
    ],
  },
  {
    slug: "color",
    href: "/brand/color",
    n: "04",
    title: "Color",
    summary: "Violet, blue and mint against a deep, dark background.",
    parts: [
      { id: "dark-backgrounds", title: "Dark backgrounds" },
      { id: "neutrals", title: "Neutrals" },
      { id: "text", title: "Text colors" },
      { id: "gradient", title: "The gradient" },
      { id: "status", title: "Status" },
      { id: "proportion", title: "Proportion" },
      { id: "contrast", title: "Contrast" },
      { id: "tokens", title: "Tokens" },
      { id: "misuse", title: "Misuse" },
    ],
  },
  {
    slug: "typography",
    href: "/brand/typography",
    n: "05",
    title: "Typography",
    summary: "Archivo for display, JetBrains Mono for text.",
    parts: [
      { id: "typefaces", title: "Typefaces" },
      { id: "display", title: "Archivo Expanded" },
      { id: "mono", title: "JetBrains Mono" },
      { id: "tester", title: "Type tester" },
      { id: "scale", title: "Type scale" },
      { id: "characters", title: "Character set" },
      { id: "pairing", title: "In combination" },
      { id: "fallbacks", title: "Fallbacks & licensing" },
      { id: "misuse", title: "Misuse" },
    ],
  },
  {
    slug: "imagery",
    href: "/brand/imagery",
    n: "06",
    title: "Imagery",
    summary: "A moving field built from the shapes of the mark.",
    parts: [
      { id: "origin", title: "Built from the mark" },
      { id: "field", title: "The field" },
      { id: "forming", title: "Forming the mark" },
      { id: "transitions", title: "Between sections" },
      { id: "shape-size", title: "Shape size" },
      { id: "color", title: "Color in the field" },
      { id: "misuse", title: "Misuse" },
    ],
  },
  {
    slug: "sub-brands",
    href: "/brand/sub-brands",
    n: "07",
    title: "Sub-brands",
    summary: "Naming and lockups for products.",
    parts: [
      { id: "structure", title: "Structure" },
      { id: "lockups", title: "Lockups" },
      { id: "construction", title: "Construction" },
      { id: "in-product", title: "In product" },
      { id: "misuse", title: "Misuse" },
    ],
  },
  {
    slug: "motion",
    href: "/brand/motion",
    n: "08",
    title: "Motion",
    summary: "Easing, timing and transitions.",
    parts: [
      { id: "principles", title: "Principles" },
      { id: "easing", title: "The curve" },
      { id: "durations", title: "Durations" },
      { id: "reveal", title: "Word reveal" },
      { id: "opening", title: "Opening animation" },
      { id: "forming", title: "Forming the mark" },
      { id: "interaction", title: "Interaction" },
      { id: "reduced", title: "Reduced motion" },
      { id: "misuse", title: "Misuse" },
    ],
  },
  {
    slug: "partnerships",
    href: "/brand/partnerships",
    n: "09",
    title: "Partnerships",
    summary: "Co-branding with partners.",
    parts: [
      { id: "lockup", title: "Partner lockup" },
      { id: "construction", title: "Construction" },
      { id: "order", title: "Who leads" },
      { id: "announcements", title: "Announcements" },
      { id: "misuse", title: "Misuse" },
    ],
  },
  {
    slug: "in-use",
    href: "/brand/in-use",
    n: "10",
    title: "In use",
    summary: "The identity across surfaces.",
    parts: [
      { id: "website", title: "Website" },
      { id: "social", title: "Social cards" },
      { id: "profile", title: "Profiles" },
      { id: "docs", title: "Documentation" },
      { id: "print", title: "Print" },
      { id: "slides", title: "Slides" },
      { id: "poster", title: "Poster" },
    ],
  },
];

export const BRAND_ASSETS_ZIP = "/brand/covenant-brand-assets.zip";

export function brandSection(slug: string): BrandSection {
  const found = BRAND_SECTIONS.find((s) => s.slug === slug);
  if (!found) throw new Error(`Unknown brand section: ${slug}`);
  return found;
}

export function brandNeighbours(slug: string) {
  const i = BRAND_SECTIONS.findIndex((s) => s.slug === slug);
  return {
    prev: i > 0 ? BRAND_SECTIONS[i - 1] : undefined,
    next: i >= 0 && i < BRAND_SECTIONS.length - 1 ? BRAND_SECTIONS[i + 1] : undefined,
  };
}
