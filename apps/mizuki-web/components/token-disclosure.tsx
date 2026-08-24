export const mizukiTokenMint = 'DwquZcs2JtPe2w9xfyqF9wDnySQXLBHTMawusJ8Uk1mi';

const pumpFunUrl = `https://pump.fun/coin/${mizukiTokenMint}`;
const solscanUrl = `https://solscan.io/token/${mizukiTokenMint}`;

export function TokenDisclosure() {
  return (
    <section className="section token-section" aria-labelledby="token-disclosure-title">
      <div className="shell token-grid">
        <div>
          <p className="eyebrow">Token disclosure</p>
          <h2 id="token-disclosure-title">$MIZUKI</h2>
        </div>
        <p>
          The token does not control customer jobs and does not provide a claim on revenue. Creator
          fees are reported separately in SOL and are excluded from work revenue, margin, and USDC
          refund capacity.
        </p>
        <div className="token-details">
          <span>Contract address</span>
          <a href={solscanUrl} target="_blank" rel="noreferrer" className="token-contract">
            <code>{mizukiTokenMint}</code>
            <span aria-hidden="true">↗</span>
          </a>
          <a href={pumpFunUrl} className="button button-secondary" target="_blank" rel="noreferrer">
            View on pump.fun <span aria-hidden="true">↗</span>
          </a>
        </div>
      </div>
    </section>
  );
}
