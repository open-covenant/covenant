// The Covenant palette with every value a designer or developer asks for.
// CMYK is a straight conversion from sRGB: a starting point for print, to be
// proofed on a dark stock. Contrast is the WCAG ratio against Background.
// `token` is the public CSS custom property, as published in the brand kit.

export type PaletteColor = {
  name: string;
  token: string;
  hex: string;
  rgb: [number, number, number];
  cmyk: [number, number, number, number];
  oklch: string;
  contrast: number;
  role: string;
};

export const NEUTRALS: PaletteColor[] = [
  { name: "Background", token: "--covenant-background", hex: "#000000", rgb: [0, 0, 0], cmyk: [0, 0, 0, 100], oklch: "oklch(0% 0 0)", contrast: 1, role: "The page color. Every screen and printed page starts here." },
  { name: "Inset", token: "--covenant-inset", hex: "#08070E", rgb: [8, 7, 14], cmyk: [43, 50, 0, 95], oklch: "oklch(13.4% 0.016 291.5)", contrast: 1.05, role: "One step lighter than the background and one step darker than Surface: code, terminals and recessed areas." },
  { name: "Surface", token: "--covenant-surface", hex: "#14111F", rgb: [20, 17, 31], cmyk: [35, 45, 0, 88], oklch: "oklch(18.9% 0.028 293.3)", contrast: 1.13, role: "One step lighter: cards, inputs and panels." },
  { name: "Border subtle", token: "--covenant-border-subtle", hex: "#1D1A2B", rgb: [29, 26, 43], cmyk: [33, 40, 0, 83], oklch: "oklch(23.0% 0.033 291.7)", contrast: 1.23, role: "Quiet dividers inside a card." },
  { name: "Border", token: "--covenant-border", hex: "#2A2640", rgb: [42, 38, 64], cmyk: [34, 41, 0, 75], oklch: "oklch(28.6% 0.047 290.1)", contrast: 1.45, role: "The default border and divider." },
  { name: "Border strong", token: "--covenant-border-strong", hex: "#3D3757", rgb: [61, 55, 87], cmyk: [30, 37, 0, 66], oklch: "oklch(35.8% 0.055 292.0)", contrast: 1.88, role: "Outlines that need to stand out: inputs and secondary buttons." },
];

export const TEXT_COLORS: PaletteColor[] = [
  { name: "Primary text", token: "--covenant-text", hex: "#F3F1FA", rgb: [243, 241, 250], cmyk: [3, 4, 0, 2], oklch: "oklch(96.2% 0.012 296.4)", contrast: 18.77, role: "Headlines, names and anything that must be read first." },
  { name: "Secondary text", token: "--covenant-text-secondary", hex: "#DEDAEE", rgb: [222, 218, 238], cmyk: [7, 8, 0, 7], oklch: "oklch(89.8% 0.027 295.0)", contrast: 15.37, role: "Subheads and strong secondary text." },
  { name: "Body text", token: "--covenant-text-body", hex: "#C4BFD8", rgb: [196, 191, 216], cmyk: [9, 12, 0, 15], oklch: "oklch(81.7% 0.035 294.8)", contrast: 11.79, role: "Running text in documentation and long reads." },
  { name: "Muted text", token: "--covenant-text-muted", hex: "#A49EC0", rgb: [164, 158, 192], cmyk: [15, 18, 0, 25], oklch: "oklch(71.6% 0.049 293.2)", contrast: 8.23, role: "Supporting copy, introductions and captions." },
  { name: "Subtle text", token: "--covenant-text-subtle", hex: "#8079A0", rgb: [128, 121, 160], cmyk: [20, 24, 0, 37], oklch: "oklch(59.7% 0.059 292.7)", contrast: 5.16, role: "Labels and metadata. The minimum for anything meant to be read." },
  { name: "Faint text", token: "--covenant-text-faint", hex: "#5C5675", rgb: [92, 86, 117], cmyk: [21, 26, 0, 54], oklch: "oklch(47.1% 0.050 293.3)", contrast: 3.04, role: "Decorative only: index numbers, arrows and markers." },
];

export const BRAND_COLORS: PaletteColor[] = [
  { name: "Violet", token: "--covenant-violet", hex: "#A03BF0", rgb: [160, 59, 240], cmyk: [33, 75, 0, 6], oklch: "oklch(58.5% 0.253 304.9)", contrast: 4.38, role: "The start of the gradient." },
  { name: "Blue", token: "--covenant-blue", hex: "#4C7FDE", rgb: [76, 127, 222], cmyk: [66, 43, 0, 13], oklch: "oklch(60.8% 0.155 261.7)", contrast: 5.4, role: "The middle of the gradient; link hover and selection." },
  { name: "Mint", token: "--covenant-mint", hex: "#1DE3AE", rgb: [29, 227, 174], cmyk: [87, 0, 23, 11], oklch: "oklch(81.5% 0.161 167.8)", contrast: 12.65, role: "The end of the gradient; focus rings and live states." },
];

export const STATUS: PaletteColor[] = [
  { name: "Success", token: "--covenant-success", hex: "#1DE3AE", rgb: [29, 227, 174], cmyk: [87, 0, 23, 11], oklch: "oklch(81.5% 0.161 167.8)", contrast: 12.65, role: "Live, verified, passed." },
  { name: "Warning", token: "--covenant-warning", hex: "#E0B968", rgb: [224, 185, 104], cmyk: [0, 17, 54, 12], oklch: "oklch(80.3% 0.110 84.2)", contrast: 11.31, role: "In progress or needs attention." },
  { name: "Error", token: "--covenant-error", hex: "#E18383", rgb: [225, 131, 131], cmyk: [0, 42, 42, 12], oklch: "oklch(71.1% 0.116 20.4)", contrast: 7.78, role: "Failed, refused or missing." },
];

export const GRADIENT_CSS = "linear-gradient(115deg, #A03BF0, #4C7FDE, #1DE3AE)";
