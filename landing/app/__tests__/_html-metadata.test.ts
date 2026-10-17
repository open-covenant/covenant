import { describe, expect, it } from "vitest";
import { applyPageMetadata } from "../_html-metadata";

const card = { title: "GPU price history", section: "Compute" };
const details = { title: "GPU prices <now> & forever", description: 'Quotes "today" & tomorrow' };

describe("HTML route sharing metadata", () => {
  it("replaces duplicate source tags and escapes values while retaining the page body", () => {
    const body = '<body><script>const template = \'<meta property="og:image" content="body">\';</script><main>Prices</main></body>';
    const original = `<html><head><title>old</title><META PROPERTY='og:image' CONTENT='old'><meta name="twitter:image" content="old"><link rel='canonical' href='old'><link rel="stylesheet" href="keep.css"><meta name="viewport" content="width=device-width"></head>${body}</html>`;
    const result = applyPageMetadata(original, "/compute-prices", card, details);
    const head = result.split("</head>")[0];
    expect(head.match(/property="og:image"/g)).toHaveLength(1);
    expect(head.match(/rel="canonical"/g)).toHaveLength(1);
    expect(head).toContain('href="https://opencovenant.org/compute-prices"');
    expect(head).toContain('content="https://opencovenant.org/og/compute-prices.png"');
    expect(head).toContain("GPU prices &lt;now&gt; &amp; forever");
    expect(head).toContain("Quotes &quot;today&quot; &amp; tomorrow");
    expect(head).toContain('href="keep.css"');
    expect(head).toContain('name="viewport"');
    expect(result).toContain(body);
  });

  it("rejects a non-page source with no head", () => {
    expect(() => applyPageMetadata("no page", "/compute-prices", card, details)).toThrow("no head");
  });

  it("leaves metadata-like text inside head scripts, styles and comments unchanged", () => {
    const script = '<script>const example = \'<meta property="og:title" content="example">\';</script>';
    const style = '<style>.hint::after { content: "<title>example</title>"; }</style>';
    const comment = '<!-- <link rel="canonical" href="comment"> -->';
    const result = applyPageMetadata(`<html><head>${script}${style}${comment}</head><body></body></html>`, "/compute-prices", card, details);
    expect(result).toContain(script);
    expect(result).toContain(style);
    expect(result).toContain(comment);
  });
});
