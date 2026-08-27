export const mizukiTokenMint = 'DwquZcs2JtPe2w9xfyqF9wDnySQXLBHTMawusJ8Uk1mi';

const solscanUrl = `https://solscan.io/token/${mizukiTokenMint}`;
const clawPumpAgentUrl =
  'https://clawpump.tech/marketplace/agents/711fa8b1-5f37-4451-b7a7-bfcb9a021f6d';

export function TokenNote() {
  return (
    <div className="token-note">
      <p>
        <span className="token-note-tag">$MIZUKI</span> does not control customer jobs and carries
        no claim on revenue. Creator fees are reported separately in SOL and are excluded from work
        revenue, margin, and USDC refund capacity.
      </p>
      <div className="token-note-links">
        <a href={solscanUrl} target="_blank" rel="noreferrer">
          <code>{mizukiTokenMint}</code>
          <span aria-hidden="true">↗</span>
        </a>
        <a href={clawPumpAgentUrl} target="_blank" rel="noreferrer">
          See the agent on ClawPump <span aria-hidden="true">↗</span>
        </a>
      </div>
    </div>
  );
}
