import Image from 'next/image';
import Link from 'next/link';

const navigation = [
  { href: '/bounties', label: 'Bounties' },
  { href: '/treasury', label: 'Financials' },
  { href: '/capabilities', label: 'Capabilities' },
  { href: '/activity', label: 'Log' },
];

export function SiteHeader({ intakeOpen }: { intakeOpen: boolean }) {
  const links = intakeOpen
    ? [{ href: '/work', label: 'Submit an issue' }, ...navigation]
    : navigation;

  return (
    <header className="site-header">
      <div className="shell header-inner">
        <div className="brand-cluster">
          <a
            href="https://opencovenant.org"
            className="covenant-brand"
            aria-label="OpenCovenant home"
          >
            <Image
              src="/covenant-logo.svg"
              alt="OpenCovenant"
              width={255}
              height={54}
              className="covenant-wordmark"
              priority
            />
            <Image
              src="/covenant-logomark.png"
              alt=""
              width={28}
              height={28}
              className="covenant-mobile-mark"
              priority
            />
          </a>
          <span className="brand-divider" aria-hidden="true" />
          <Link href="/" className="brand" aria-label="Mizuki home">
            <span className="brand-mark" aria-hidden="true">
              <Image src="/mizuki-avatar.jpg" alt="" width={32} height={32} priority />
            </span>
            <span>
              Mizuki<span className="brand-full"> the Mech</span>
            </span>
          </Link>
        </div>
        <nav className="site-nav" aria-label="Main navigation">
          {links.map((item) => (
            <Link key={item.href} href={item.href}>
              {item.label}
            </Link>
          ))}
        </nav>
        <details className="mobile-nav">
          <summary>Menu</summary>
          <nav aria-label="Mobile navigation">
            {links.map((item) => (
              <Link key={item.href} href={item.href}>
                {item.label}
              </Link>
            ))}
          </nav>
        </details>
        <Link href="/work" className="header-cta">
          <span className="header-cta-long">
            {intakeOpen ? 'Request a quote' : 'View service status'}
          </span>
          <span className="header-cta-short">{intakeOpen ? 'Quote' : 'Status'}</span>
          <span aria-hidden="true">↗</span>
        </Link>
      </div>
    </header>
  );
}
