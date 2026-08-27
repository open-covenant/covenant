import Image from 'next/image';
import Link from 'next/link';
import { SectionEdge } from './section-edge';
import { TokenNote } from './token-disclosure';

export function SiteFooter({ intakeOpen }: { intakeOpen: boolean }) {
  return (
    <footer className="site-footer">
      <SectionEdge position="top" seed={4.1} />
      <div className="shell footer-grid">
        <div className="footer-intro">
          <div className="footer-brand-row">
            <a href="https://opencovenant.org" aria-label="OpenCovenant home">
              <Image src="/covenant-mark.svg" alt="OpenCovenant" width={1140} height={1050} className="covenant-mark-inline" />
            </a>
            <div className="brand footer-brand">
              <span className="brand-mark" aria-hidden="true">
                <Image src="/mizuki-mark.svg" alt="" width={1470} height={1050} className="mizuki-mark" />
              </span>
              <span>Mizuki the Mech</span>
            </div>
          </div>
          <p className="footer-statement">
            A validated pull request or a full refund of the quoted USDC payment.
          </p>
          <p className="footer-protocols">
            Public GitHub maintenance · USDC payments on Solana · AI execution through UsePod
          </p>
        </div>
        <div className="footer-column">
          <h3>Service</h3>
          <nav aria-label="Service links">
            <Link href="/app">Workbench</Link>
            <Link href="/work">{intakeOpen ? 'Submit an issue' : 'Service status'}</Link>
            <Link href="/bounties">Bounties</Link>
            <Link href="/activity">Log</Link>
          </nav>
        </div>
        <div className="footer-column">
          <h3>Evidence</h3>
          <nav aria-label="Evidence links">
            <Link href="/treasury">Financials</Link>
            <Link href="/capabilities">Capabilities</Link>
            <Link href="/activity">Public activity</Link>
          </nav>
        </div>
        <div className="footer-column">
          <h3>Network</h3>
          <nav aria-label="Network links">
            <a
              href="https://clawpump.tech/marketplace/agents/711fa8b1-5f37-4451-b7a7-bfcb9a021f6d"
              rel="noreferrer"
              target="_blank"
            >
              ClawPump
            </a>
            <a href="https://usepod.ai" rel="noreferrer" target="_blank">
              UsePod
            </a>
            <a
              href="https://pump.fun/coin/DwquZcs2JtPe2w9xfyqF9wDnySQXLBHTMawusJ8Uk1mi"
              rel="noreferrer"
              target="_blank"
            >
              $MIZUKI
            </a>
          </nav>
        </div>
        <div className="footer-column">
          <h3>Developers</h3>
          <nav aria-label="Developer links">
            <a href="https://github.com/open-covenant/covenant" rel="noreferrer" target="_blank">
              GitHub
            </a>
            <Link href="/security">Security</Link>
            <Link href="/support">Support</Link>
          </nav>
        </div>
        <div className="footer-column">
          <h3>Company</h3>
          <nav aria-label="Company links">
            <a href="https://opencovenant.org">OpenCovenant</a>
            <a href="https://x.com/MizukiMech" rel="noreferrer" target="_blank">
              Follow on X
            </a>
            <Link href="/terms">Terms</Link>
            <Link href="/privacy">Privacy</Link>
          </nav>
        </div>
      </div>
      <div className="shell">
        <TokenNote />
      </div>
      <div className="shell footer-bottom">
        <span>© 2026 OpenCovenant</span>
        <a
          href="https://github.com/open-covenant/covenant/blob/main/LICENSE"
          target="_blank"
          rel="noreferrer"
        >
          Source code license: Apache-2.0 <span aria-hidden="true">↗</span>
        </a>
      </div>
    </footer>
  );
}
