import { cardAlt, cardImageUrl, CARD_SIZE, pageMetadata, plainTitle, type PageCard } from "./_metadata";

const escape = (value: string) => value.replace(/[&<>"']/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[char]!);
const attr = (tag: string, name: string) => tag.match(new RegExp(`\\b${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s>]+))`, "i"))?.slice(1).find((value) => value !== undefined);

// Route handlers serving full HTML use the same page metadata contract as
// React pages. Replace source tags in the head so crawlers see one canonical
// page and image, while preserving the existing page's scripts/styles/body.
export function applyPageMetadata(html: string, path: string, card: PageCard, details: { title: string; description: string }): string {
  const metadata = pageMetadata(path, card, details);
  const og = metadata.openGraph!;
  const title = plainTitle(metadata.title)!;
  const image = cardImageUrl(path);
  const alt = cardAlt(card.title ?? title, card.section);
  const properties: Record<string, string> = {
    "og:type": "website", "og:site_name": "Covenant", "og:url": String(og.url),
    "og:title": title, "og:description": details.description,
    "og:image": image, "og:image:secure_url": image, "og:image:type": "image/png",
    "og:image:width": String(CARD_SIZE.width), "og:image:height": String(CARD_SIZE.height), "og:image:alt": alt,
  };
  const names: Record<string, string> = {
    description: details.description, "twitter:card": "summary_large_image", "twitter:site": "@OpenCovenant",
    "twitter:creator": "@OpenCovenant", "twitter:title": title, "twitter:description": details.description,
    "twitter:image": image, "twitter:image:alt": alt, "covenant:card-title": card.title ?? title,
    ...(card.section ? { "covenant:card-section": card.section } : {}),
  };
  const tags = `<title>${escape(title)}</title>\n<link rel="canonical" href="${escape(String(metadata.alternates!.canonical))}">\n` +
    Object.entries(properties).map(([key, value]) => `<meta property="${key}" content="${escape(value)}">`).join("\n") + "\n" +
    Object.entries(names).map(([key, value]) => `<meta name="${key}" content="${escape(value)}">`).join("\n");
  let replaced = false;
  const result = html.replace(/(<head\b[^>]*>)([\s\S]*?)(<\/head\s*>)/i, (_, start: string, head: string, end: string) => {
    replaced = true;
    const clean = head.replace(/<!--[\s\S]*?-->|<(script|style)\b[^>]*>[\s\S]*?<\/\1\s*>|<title\b[^>]*>[\s\S]*?<\/title\s*>|<(?:meta|link)\b[^>]*>/gi, (tag) => {
      if (/^<!--|^<(?:script|style)\b/i.test(tag)) return tag;
      if (/^<title\b/i.test(tag)) return "";
      if (/^<meta\b/i.test(tag)) return /^(?:og:|twitter:|covenant:card-|description$)/i.test(attr(tag, "property") ?? attr(tag, "name") ?? "") ? "" : tag;
      return /\bcanonical\b/i.test(attr(tag, "rel") ?? "") ? "" : tag;
    });
    return `${start}${clean}\n${tags}\n${end}`;
  });
  if (!replaced) throw new Error("Public HTML source has no head");
  return result;
}
