import type { Metadata } from "next";

const DESCRIPTION =
  "Covenant: verifiable trust and safe execution for AI agents on Solana. One-pager.";

// Unlinked by design — not in any nav/footer, and noindex so it is only reached
// by people given the direct link. Also the print/PDF source: the @media print
// block renders a full-bleed A4 page.
export const metadata: Metadata = {
  title: "Covenant — one pager",
  description: DESCRIPTION,
  alternates: { canonical: "/onepager" },
  robots: { index: false, follow: false, googleBot: { index: false, follow: false } },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/onepager",
    title: "Covenant — verifiable trust & safe execution for AI agents on Solana",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
};

const CSS = `
@page { size: A4; margin: 0; }
#op {
  --fg: #ededed; --muted: #8a8a8a; --faint: #6f6f6f; --line: #262626; --accent: #3a3a3a;
  --mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  background: #030303; color: var(--fg); min-height: 100dvh;
  -webkit-print-color-adjust: exact; print-color-adjust: exact;
}
#op .page { max-width: 880px; margin: 0 auto; padding: clamp(28px, 5vw, 60px); background: #030303; }
#op .mast { text-align: center; padding-bottom: 9px; }
#op .wordmark { font-weight: 200; font-size: 26px; letter-spacing: 0.42em; text-indent: 0.42em; margin: 0; }
#op .submark { font-family: var(--mono); font-size: 8px; letter-spacing: 0.34em; text-indent: 0.34em; color: var(--muted); margin: 7px 0 0; text-transform: uppercase; }
#op hr { border: 0; border-top: 1px solid var(--line); margin: 13px 0 16px; }
#op .deck { text-align: center; font-family: var(--mono); font-size: 9.5px; letter-spacing: 0.26em; text-indent: 0.26em; color: #cfcfcf; text-transform: uppercase; margin: 0 0 16px; }
#op .lede { font-size: 12px; line-height: 1.62; color: #cdcdcd; margin: 0 0 22px; }
#op .lede strong { color: var(--fg); font-weight: 600; }
#op .cols { display: flex; gap: 34px; }
#op .col { flex: 1; min-width: 0; }
#op .seclabel { font-family: var(--mono); font-size: 9px; letter-spacing: 0.3em; text-transform: uppercase; color: var(--muted); margin: 0 0 11px; }
#op .seclabel .n { color: var(--faint); margin-right: 9px; }
#op .intro { color: var(--muted); font-size: 11px; line-height: 1.55; margin: 0 0 14px; }
#op .guarantee { border-left: 1px solid var(--accent); padding: 0 0 0 12px; margin: 0 0 13px; }
#op .guarantee p { margin: 0; font-size: 11px; line-height: 1.55; color: var(--muted); }
#op .guarantee b { color: var(--fg); font-weight: 600; }
#op ul.live { list-style: none; margin: 0; padding: 0; }
#op ul.live li { position: relative; padding-left: 16px; margin: 0 0 13px; font-size: 11px; line-height: 1.55; color: var(--muted); }
#op ul.live li::before { content: ""; position: absolute; left: 0; top: 5px; width: 6px; height: 6px; background: #cfcfcf; }
#op ul.live b { color: var(--fg); font-weight: 600; }
#op .block { margin-top: 22px; }
#op .block p { font-size: 11px; line-height: 1.62; color: var(--muted); margin: 0; }
#op .block p b { color: var(--fg); font-weight: 600; }
#op .block p .lit { color: #cdcdcd; }
#op .foot { display: flex; justify-content: space-between; align-items: center; margin-top: 26px; padding-top: 12px; border-top: 1px solid var(--line); font-family: var(--mono); font-size: 9px; letter-spacing: 0.16em; color: var(--faint); text-transform: uppercase; }
#op .foot .site { color: var(--muted); }
#op .foot .tag { font-family: var(--mono); color: #bdbdbd; margin-right: 6px; }
@media (max-width: 640px) { #op .cols { flex-direction: column; gap: 22px; } }
@media print {
  #op { min-height: 0; }
  #op .page { width: 210mm; height: 297mm; max-width: none; overflow: hidden; padding: 15mm 16mm 12mm; }
  #op .cols { flex-direction: row; gap: 34px; }
}
`;

export default function OnePagerPage() {
  return (
    <main id="op">
      <style dangerouslySetInnerHTML={{ __html: CSS }} />
      <div className="page" id="main-content">
        <div className="mast">
          <p className="wordmark">COVENANT</p>
          <p className="submark">Human intent {"</>"} agent protocol</p>
        </div>
        <hr />
        <p className="deck">Verifiable trust &amp; safe execution for AI agents on Solana</p>

        <p className="lede">
          Autonomous agents are getting wallets, spend authority, and the ability to act on chain. The barrier to that
          future is trust: no one hands an agent real money or authority if it can be front-run, over-leveraged, drained,
          or tricked into signing a malicious transaction.{" "}
          <strong>Covenant is the layer that makes an agent&rsquo;s actions safe by construction and verifiable by anyone.</strong>{" "}
          It is the neutral trust and execution layer for AI agents on Solana.
        </p>

        <div className="cols">
          <div className="col">
            <p className="seclabel"><span className="n">01</span>What Covenant is</p>
            <p className="intro">
              A daemon and eight on-chain primitives turn an agent into one that signs Solana transactions under controls
              it cannot bypass. Four guarantees hold on every action:
            </p>
            <div className="guarantee"><p><b>Capability gating.</b> An agent runs only pre-authorized actions. Anything outside its grant is refused before signing.</p></div>
            <div className="guarantee"><p><b>Simulate before sign.</b> No transaction is signed unless it simulates clean against live state.</p></div>
            <div className="guarantee"><p><b>Hash-chained audit, on-chain receipts.</b> Every action is recorded and anchored, with a signed receipt that verifies independently and fails on tampering.</p></div>
            <div className="guarantee"><p><b>Neutral attestation.</b> As a third party, a Covenant Verified mark carries weight an agent&rsquo;s own claims cannot.</p></div>
          </div>

          <div className="col">
            <p className="seclabel"><span className="n">02</span>Live today</p>
            <ul className="live">
              <li><b>Alpha released and cryptographically signed.</b> Public, keyless-signed build (v0.1.0-alpha.1).</li>
              <li><b>Settlement program deployed on Solana mainnet.</b> On-chain audit and attestation, not a promise.</li>
              <li><b>Safe-execution skills running real mainnet transactions.</b> Swap, stake, perps, and prediction, each gated and receipted.</li>
              <li><b>x402 seller infrastructure live.</b> Paid, independently verifiable endpoints in production, settled in USDC on Solana mainnet.</li>
            </ul>
          </div>
        </div>

        <div className="block">
          <p className="seclabel"><span className="n">03</span>Traction</p>
          <p>
            The same trust layer is already reused across the Solana agent stack. Agent identity and on-chain attestations
            run on mainnet through <span className="lit">Metaplex</span> (the MPL Agent registry and MPL Core); safe swap,
            perps, and prediction skills execute real mainnet transactions through <span className="lit">Jupiter</span>;
            agent reputation is live in the <span className="lit">PayAI</span> bazaar alongside x402 payments, never
            touching them; and credit delegation is proven on <span className="lit">MagicBlock</span>&rsquo;s mainnet
            ephemeral rollups. Identity resolves through <span className="lit">Solana Name Service</span> (.sol), paid and
            independently verifiable attestation endpoints are in review for the <span className="lit">Solana Foundation</span>&rsquo;s
            pay.sh registry, and a flagship safe-execution partnership with <span className="lit">Xona</span> is forming.
            Further integrations span HatcherLabs, ClawVille, Synapse/SAP, and more &mdash; the same trust layer, reused
            everywhere agents act.
          </p>
        </div>

        <div className="block">
          <p className="seclabel"><span className="n">04</span>Business model</p>
          <p>
            Covenant runs x402 seller infrastructure today. Execution skills monetize as a{" "}
            <b>per-action fee assessed at signing</b>, with a premium tier for verifiable receipts and neutral
            attestation. Covenant never touches a partner&rsquo;s payment rails or token.{" "}
            <b>Revenue scales directly with agent on-chain activity</b>, the one metric every consumer agent roadmap is
            built to grow. $CVNT aligns the network through real-yield staking from protocol fees.
          </p>
        </div>

        <div className="foot">
          <span className="site"><span className="tag">{"</>"}</span>opencovenant.org</span>
          <span>Prepared by Mizuki Hayashi, Covenant</span>
        </div>
      </div>
    </main>
  );
}
