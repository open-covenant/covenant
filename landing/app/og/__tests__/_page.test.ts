import { describe, expect, it } from "vitest";
import { decodeEntities, readCardData } from "../_page";

const head = (tags: string) => `<!DOCTYPE html><html><head><title>Color: Covenant brand guidelines</title>${tags}</head><body></body></html>`;

describe("readCardData", () => {
  it("reads the card title, section and page address the page declares", () => {
    const html = head(
      '<meta property="og:title" content="Color · Covenant brand guidelines"/>' +
        '<meta property="og:url" content="https://opencovenant.org/brand/color"/>' +
        '<meta name="covenant:card-title" content="Color"/>' +
        '<meta name="covenant:card-section" content="Brand guidelines"/>',
    );
    expect(readCardData(html)).toEqual({
      title: "Color",
      section: "Brand guidelines",
      url: "https://opencovenant.org/brand/color",
    });
  });

  it("falls back to the social title, then the document title", () => {
    expect(readCardData(head('<meta property="og:title" content="Covenant is now multi-chain. The token isn&#x27;t."/>'))?.title).toBe(
      "Covenant is now multi-chain. The token isn't.",
    );
    expect(readCardData(head(""))?.title).toBe("Color: Covenant brand guidelines");
  });

  it("returns null when the page has no title at all", () => {
    expect(readCardData("<html><head></head><body></body></html>")).toBeNull();
  });

  it("reads and decodes the description, preferring social metadata", () => {
    const html = head('<meta name="description" content="Fallback"/><meta property="og:description" content="Quotes &amp; settled rates"/>');
    expect(readCardData(html)?.description).toBe("Quotes & settled rates");
    expect(readCardData(head('<meta name="description" content="Hourly quotes"/>'))?.description).toBe("Hourly quotes");
  });
});

describe("decodeEntities", () => {
  it("decodes named and numeric entities", () => {
    expect(decodeEntities("Partners &amp; integrations &#x2014; &#39;x&#39; &quot;y&quot;")).toBe(`Partners & integrations — 'x' "y"`);
  });
});
