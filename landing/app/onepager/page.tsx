import type { Metadata } from "next";

const DESCRIPTION =
  "This Covenant one-pager is retired while its public claims are rewritten against the current implementation boundary.";

// Unlinked by design — not in any nav/footer, and noindex so it is only reached
// by people given the direct link. Also the print/PDF source: the @media print
// block renders a full-bleed A4 page.
export const metadata: Metadata = {
  title: "Covenant — retired one-pager",
  description: DESCRIPTION,
  alternates: { canonical: "/onepager" },
  robots: { index: false, follow: false, googleBot: { index: false, follow: false } },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/onepager",
    title: "Covenant — retired one-pager",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
};

const CSS = `
@page { size: A4; margin: 0; }
/* Sizes below are the SCREEN sizes (matched to the rest of the site: ~14px body,
   11px tracked labels, 15px lede). The @media print block at the bottom compacts
   everything back down so the PDF still fits one A4 page. */
#op {
  --fg: #ededed; --muted: #9a9a9a; --faint: #6f6f6f; --line: #262626; --accent: #3a3a3a;
  --mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  background: #030303; color: var(--fg); min-height: 100dvh;
  -webkit-print-color-adjust: exact; print-color-adjust: exact;
}
#op .page { max-width: 920px; margin: 0 auto; padding: clamp(32px, 6vw, 72px); background: #030303; }
#op .mast { text-align: center; padding-bottom: 12px; }
#op .logo { display: block; width: 360px; max-width: 72%; height: auto; margin: 0 auto; }
#op hr { border: 0; border-top: 1px solid var(--line); margin: 16px 0 20px; }
#op .deck { text-align: center; font-family: var(--mono); font-size: 12px; letter-spacing: 0.24em; text-indent: 0.24em; color: #d4d4d4; text-transform: uppercase; margin: 0 0 22px; }
#op .lede { font-size: 15px; line-height: 1.72; color: #d0d0d0; margin: 0 0 30px; }
#op .lede strong { color: var(--fg); font-weight: 600; }
#op .cols { display: flex; gap: 44px; }
#op .col { flex: 1; min-width: 0; }
#op .seclabel { font-family: var(--mono); font-size: 11px; letter-spacing: 0.32em; text-transform: uppercase; color: var(--muted); margin: 0 0 15px; }
#op .seclabel .n { color: var(--faint); margin-right: 10px; }
#op .intro { color: var(--muted); font-size: 14px; line-height: 1.65; margin: 0 0 18px; }
#op .guarantee { border-left: 1px solid var(--accent); padding: 0 0 0 14px; margin: 0 0 17px; }
#op .guarantee p { margin: 0; font-size: 14px; line-height: 1.6; color: var(--muted); }
#op .guarantee b { color: var(--fg); font-weight: 600; }
#op ul.live { list-style: none; margin: 0; padding: 0; }
#op ul.live li { position: relative; padding-left: 18px; margin: 0 0 17px; font-size: 14px; line-height: 1.6; color: var(--muted); }
#op ul.live li::before { content: ""; position: absolute; left: 0; top: 8px; width: 6px; height: 6px; background: #cfcfcf; }
#op ul.live b { color: var(--fg); font-weight: 600; }
#op .block { margin-top: 30px; }
#op .block p { font-size: 14px; line-height: 1.7; color: var(--muted); margin: 0; }
#op .block p b { color: var(--fg); font-weight: 600; }
#op .block p .lit { color: #d0d0d0; }
#op .foot { display: flex; justify-content: space-between; align-items: center; margin-top: 36px; padding-top: 16px; border-top: 1px solid var(--line); font-family: var(--mono); font-size: 11px; letter-spacing: 0.16em; color: var(--faint); text-transform: uppercase; }
#op .foot .site { color: var(--muted); }
#op .foot .tag { font-family: var(--mono); color: #bdbdbd; margin-right: 6px; }
@media (max-width: 640px) { #op .cols { flex-direction: column; gap: 28px; } }
@media print {
  #op { min-height: 0; }
  #op .page { width: 210mm; height: 297mm; max-width: none; overflow: hidden; padding: 15mm 16mm 12mm; }
  #op .mast { padding-bottom: 9px; }
  #op .logo { width: 232px; }
  #op hr { margin: 13px 0 16px; }
  #op .deck { font-size: 9.5px; letter-spacing: 0.26em; text-indent: 0.26em; margin-bottom: 16px; }
  #op .lede { font-size: 12px; line-height: 1.62; margin-bottom: 22px; }
  #op .cols { flex-direction: row; gap: 34px; }
  #op .seclabel { font-size: 9px; letter-spacing: 0.3em; margin-bottom: 11px; }
  #op .intro { font-size: 11px; line-height: 1.55; margin-bottom: 14px; }
  #op .guarantee { padding-left: 12px; margin-bottom: 13px; }
  #op .guarantee p { font-size: 11px; line-height: 1.55; }
  #op ul.live li { padding-left: 16px; margin-bottom: 13px; font-size: 11px; line-height: 1.55; }
  #op ul.live li::before { top: 5px; }
  #op .block { margin-top: 22px; }
  #op .block p { font-size: 11px; line-height: 1.62; }
  #op .foot { margin-top: 26px; padding-top: 12px; font-size: 9px; }
}
`;

export default function OnePagerPage() {
  return (
    <main id="op">
      <style dangerouslySetInnerHTML={{ __html: CSS }} />
      <div className="page" id="main-content">
        <div className="mast">
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src="/logo.svg" alt="Covenant — human intent / agent protocol" className="logo" />
        </div>
        <hr />
        <p className="deck">Retired one-pager</p>

        <p className="lede">
          This document is outdated and must not be distributed as a description
          of the current product. It overstated wallet-level signing
          enforcement, universal transaction simulation, receipt coverage,
          partner status, and the meaning of payment and attestation evidence.
        </p>

        <div className="block">
          <p className="seclabel">
            <span className="n">01</span>Current boundary
          </p>
          <p>
            Covenant is a local-first daemon with capability, audit, memory, and
            settlement primitives. Capability checks cover operations routed
            through implemented daemon boundaries. Wallet-level pre-sign
            enforcement, production isolation for hostile agent code, and a
            complete multi-peer trust boundary remain work in progress.
          </p>
        </div>

        <div className="block">
          <p className="seclabel">
            <span className="n">02</span>Evidence semantics
          </p>
          <p>
            Hash chains make later changes detectable but do not prove a
            compromised writer logged every event. Signatures authenticate a
            publisher and payload but do not establish claim truth. Registration
            and settlement observations do not prove identity, delivery,
            quality, or reputation.
          </p>
        </div>

        <div className="foot">
          <a
            className="site"
            href="https://github.com/open-covenant/covenant#readme"
          >
            <span className="tag">{"</>"}</span>Read the current implementation
            boundary
          </a>
        </div>
      </div>
    </main>
  );
}
