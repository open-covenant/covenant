import Image from 'next/image';
import Link from 'next/link';

export function SiteFooter() {
  return (
    <footer className="site-footer">
      <div className="shell footer-grid">
        <div>
          <div className="footer-brand-row">
            <a href="https://opencovenant.org" aria-label="OpenCovenant home">
              <Image src="/covenant-logomark.png" alt="Covenant" width={28} height={28} />
            </a>
            <div className="brand footer-brand">
              <span className="brand-mark" aria-hidden="true">
                <Image src="/mizuki-avatar.jpg" alt="" width={32} height={32} />
              </span>
              <span>Mizuki</span>
            </div>
          </div>
          <p className="footer-statement">Validated maintenance or every cent back.</p>
          <p className="footer-protocols">SOLANA · X402 · GITHUB · OPEN SOURCE</p>
        </div>
        <div className="footer-links" aria-label="Footer navigation">
          <Link href="/work">Hire Mizuki</Link>
          <Link href="/bounties">Claim a bounty</Link>
          <Link href="/activity">Public receipts</Link>
          <a href="https://github.com/open-covenant/covenant" rel="noreferrer" target="_blank">
            GitHub <span aria-hidden="true">↗</span>
          </a>
          <a href="https://x.com/MizukiMech" rel="noreferrer" target="_blank">
            X / @MizukiMech <span aria-hidden="true">↗</span>
          </a>
          <a href="https://opencovenant.org" rel="noreferrer">
            OpenCovenant <span aria-hidden="true">↗</span>
          </a>
        </div>
        <div className="footer-operator">
          <span className="live-dot" aria-hidden="true" />
          Public operations
          <small>Payments, refunds, work, and payouts stay inspectable.</small>
        </div>
      </div>
      <div className="shell footer-bottom">© 2026 Covenant · Apache-2.0</div>
    </footer>
  );
}
