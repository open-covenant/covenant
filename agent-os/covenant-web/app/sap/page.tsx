import { solscanAddressUrl } from "@/lib/explorer";
import { resolveSynapseStatus, synapseExplorerHref } from "@/lib/synapse";
import { PageHeader } from "../components/PageHeader";

export const dynamic = "force-dynamic";

export default function SynapsePage() {
  const status = resolveSynapseStatus();
  const programHref = synapseExplorerHref("address", status.programId, status);
  const solanaExplorer = solscanAddressUrl(status.programId, status.clusterAlias);
  const agentSynapseHref = status.agentPda
    ? synapseExplorerHref("agent", status.agentPda, status)
    : null;
  const agentSolanaHref = status.agentPda
    ? solscanAddressUrl(status.agentPda, status.clusterAlias)
    : null;

  return (
    <>
      <PageHeader
        eyebrow="on-chain identity, discovery, and attestations"
        title="Synapse"
        subhead={
          status.enabled
            ? "The Synapse bridge is enabled. The daemon may publish identity, mirror tool descriptors, and post audit-root attestations to the Synapse Agent Protocol on Solana."
            : "The Synapse bridge is disabled. The daemon runs fully offline with respect to the Synapse Agent Protocol. Set COVENANT_SAP_ENABLED=1 to opt in."
        }
      />

      <section className="metric-row">
        <article className="metric">
          <span className="eyebrow">bridge</span>
          <span className="value">{status.enabled ? "on" : "off"}</span>
          <span className="caption">opt-in, defaults off</span>
        </article>
        <article className="metric">
          <span className="eyebrow">cluster</span>
          <span className="value small">{status.clusterAlias}</span>
          <span className="caption">{status.cluster}</span>
        </article>
        <article className="metric">
          <span className="eyebrow">program</span>
          <span className="value small">SAP v2</span>
          <span className="caption">
            <a href={solanaExplorer} target="_blank" rel="noreferrer">
              view on solscan ↗
            </a>
          </span>
        </article>
        <article className="metric">
          <span className="eyebrow">explorer</span>
          <span className="value small">oobeprotocol.ai</span>
          <span className="caption">
            <a href={programHref} target="_blank" rel="noreferrer">
              view on synapse explorer
            </a>
          </span>
        </article>
      </section>

      <section className="panel">
        <div className="panel-head">
          <div>
            <p className="eyebrow">resolved config</p>
            <h2>What the daemon sees</h2>
          </div>
        </div>
        <dl className="config">
          <div>
            <dt>Program ID</dt>
            <dd>
              <code>{status.programId}</code>
            </dd>
          </div>
          <div>
            <dt>RPC URL</dt>
            <dd>
              <code>{status.rpcUrl}</code>
            </dd>
          </div>
          <div>
            <dt>Explorer base</dt>
            <dd>
              <code>{status.explorerUrl}</code>
            </dd>
          </div>
        </dl>
      </section>

      <section className="panel">
        <div className="panel-head">
          <div>
            <p className="eyebrow">published identities</p>
            <h2>This daemon&apos;s on-chain presence</h2>
          </div>
        </div>
        {status.agentPda ? (
          <dl className="config">
            <div>
              <dt>Agent PDA</dt>
              <dd>
                <code>{status.agentPda}</code>
              </dd>
            </div>
            <div>
              <dt>Synapse</dt>
              <dd>
                <a href={agentSynapseHref!} target="_blank" rel="noreferrer">
                  view agent on synapse explorer
                </a>
              </dd>
            </div>
            <div>
              <dt>Solana</dt>
              <dd>
                <a href={agentSolanaHref!} target="_blank" rel="noreferrer">
                  view account on solscan ↗
                </a>
              </dd>
            </div>
          </dl>
        ) : (
          <p className="empty">
            No agent published yet. Identity publish is opt-in: run{" "}
            <code>covenant-sap-worker publish-agent</code> and set{" "}
            <code>COVENANT_SAP_AGENT_PDA</code> to the returned address.
          </p>
        )}
      </section>

      <style>{`
        .config {
          display: grid;
          grid-template-columns: 200px minmax(0, 1fr);
          gap: 12px 24px;
          margin: 0;
        }

        .config > div {
          display: contents;
        }

        .config dt {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 11px;
          letter-spacing: 0.16em;
          text-transform: uppercase;
        }

        .config dd {
          margin: 0;
          color: var(--fg);
          font-family: var(--font-mono);
          font-size: 12px;
          overflow-wrap: anywhere;
        }

        .config code {
          color: var(--fg);
        }

        @media (max-width: 600px) {
          .config {
            grid-template-columns: minmax(0, 1fr);
          }
        }
      `}</style>
    </>
  );
}
