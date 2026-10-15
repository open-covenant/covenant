import { describe, expect, it } from "vitest";
import { fitTitle, measure, wordSpacing } from "../_fit";

const WIDTH = 1072;

describe("fitTitle", () => {
  it("sets a short title on one line at the largest size", () => {
    expect(fitTitle("SDK", { width: WIDTH })).toEqual({ size: 112, lines: ["SDK"] });
  });

  it("sets titles in capitals", () => {
    expect(fitTitle("Brand guidelines", { width: WIDTH }).lines.join(" ")).toBe("BRAND GUIDELINES");
  });

  it("sets apostrophes as typographic ones", () => {
    expect(fitTitle("The token isn't", { width: WIDTH }).lines.join(" ")).toBe("THE TOKEN ISN’T");
  });

  it("breaks a long title into at most three lines that fit the measure", () => {
    const { size, lines } = fitTitle("The model you paid for is not always the model that ran", { width: WIDTH });
    expect(lines.length).toBeLessThanOrEqual(3);
    expect(lines.join(" ")).toBe("THE MODEL YOU PAID FOR IS NOT ALWAYS THE MODEL THAT RAN");
    for (const line of lines) expect(measure(line, size)).toBeLessThanOrEqual(WIDTH);
    expect(size).toBeLessThan(112);
  });

  it("shrinks rather than break a word that is wider than the measure", () => {
    const { size, lines } = fitTitle("Interoperabilities", { width: 900 });
    expect(lines).toEqual(["INTEROPERABILITIES"]);
    expect(measure(lines[0], size)).toBeLessThanOrEqual(900);
    expect(size).toBeLessThan(112);
  });

  it("ends an overlong title with an ellipsis at the smallest size", () => {
    const title = Array.from({ length: 40 }, (_, i) => `word${i}`).join(" ");
    const { size, lines } = fitTitle(title, { width: WIDTH });
    expect(size).toBe(48);
    expect(lines).toHaveLength(3);
    expect(lines[2].endsWith("…")).toBe(true);
    for (const line of lines) expect(measure(line, size)).toBeLessThanOrEqual(WIDTH);
  });
});

describe("measure", () => {
  it("applies the font's kerning", () => {
    // A, Y and the AY pair: 0.908 + 0.861 - 0.115, less tracking on each letter.
    expect(measure("AY", 100)).toBeCloseTo((0.908 + 0.861 - 0.115 - 0.1) * 100, 6);
  });
});

describe("wordSpacing", () => {
  it("closes the gap a kerned word would leave and sets an even space between words", () => {
    const [payment, rail] = wordSpacing("PAYMENT RAIL");
    // PA -0.07 and AY -0.115, then a space less tracking.
    expect(payment.word).toBe("PAYMENT");
    expect(payment.after).toBeCloseTo(-0.185 + 0.27, 6);
    expect(rail).toEqual({ word: "RAIL", after: 0 });
  });

  it("keeps the line exactly as wide as measured", () => {
    const line = "COVENANT IS NOW MULTI-CHAIN.";
    const unkerned = (w: string) => [...w].reduce((em, ch) => em + measure(ch, 1), 0);
    const set = wordSpacing(line).reduce((em, { word, after }) => em + unkerned(word) + after, 0);
    expect(set).toBeCloseTo(measure(line, 1), 6);
  });
});
