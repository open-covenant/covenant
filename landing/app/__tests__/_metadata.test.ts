import { describe, expect, it } from "vitest";
import { cardAlt, cardImageUrl, pageMetadata } from "../_metadata";

describe("cardImageUrl", () => {
  it("points every page but home at its generated card", () => {
    expect(cardImageUrl("/brand/color")).toBe("https://opencovenant.org/og/brand/color.png");
    expect(cardImageUrl("/docs")).toBe("https://opencovenant.org/og/docs.png");
    expect(cardImageUrl("/")).toBe("https://opencovenant.org/opengraph-image.jpg");
  });
});

describe("cardAlt", () => {
  it("names Covenant, the section and the headline", () => {
    expect(cardAlt("Color", "Brand guidelines")).toBe("Covenant brand guidelines: Color");
    expect(cardAlt("Positions", "Covenant Stake")).toBe("Covenant Stake: Positions");
    expect(cardAlt("$CVNT")).toBe("Covenant: $CVNT");
    expect(cardAlt("About Covenant")).toBe("About Covenant");
    expect(cardAlt(undefined)).toBe("Covenant");
  });
});

describe("pageMetadata", () => {
  it("fills in the canonical address, social tags and the card", () => {
    const m = pageMetadata("/sdk", { title: "Covenant SDK" }, { title: "SDK", description: "The TypeScript SDK." });
    const image = "https://opencovenant.org/og/sdk.png";

    expect(m.alternates?.canonical).toBe("https://opencovenant.org/sdk");
    expect(m.openGraph).toMatchObject({ url: "https://opencovenant.org/sdk", title: "SDK", description: "The TypeScript SDK." });
    expect(m.openGraph?.images).toEqual([{ url: image, width: 1200, height: 630, alt: "Covenant SDK" }]);
    expect(m.twitter).toMatchObject({ card: "summary_large_image", site: "@OpenCovenant", title: "SDK" });
    expect((m.twitter as { images: Array<{ url: string }> }).images[0].url).toBe(image);
    expect(m.other).toEqual({ "covenant:card-title": "Covenant SDK" });
  });

  it("keeps what the page declares and replaces only the images", () => {
    const m = pageMetadata("/blog/compute", { section: "Blog" }, {
      title: "The model you paid for is not always the model that ran",
      alternates: { canonical: "/blog/compute" },
      openGraph: { type: "article", title: "Social title", description: "Social description", images: "/old.jpg" },
      twitter: { description: "Short description.", images: "/old.jpg" },
    });

    expect(m.alternates?.canonical).toBe("/blog/compute");
    expect(m.openGraph).toMatchObject({ type: "article", title: "Social title", description: "Social description" });
    expect(m.openGraph?.images).toEqual([
      { url: "https://opencovenant.org/og/blog/compute.png", width: 1200, height: 630, alt: "Covenant blog: The model you paid for is not always the model that ran" },
    ]);
    expect(m.twitter).toMatchObject({ title: "Social title", description: "Short description." });
    expect(m.other).toEqual({
      "covenant:card-title": "The model you paid for is not always the model that ran",
      "covenant:card-section": "Blog",
    });
  });
});
