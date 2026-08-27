import Image from 'next/image';
import Link from 'next/link';

const navigation = [
  { href: '/bounties', label: 'Bounties' },
  { href: '/treasury', label: 'Financials' },
  { href: '/capabilities', label: 'Capabilities' },
  { href: '/activity', label: 'Log' },
];

export function SiteHeader() {
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
              src="/covenant-mark.svg"
              alt="OpenCovenant"
              width={1140}
              height={1050}
              className="covenant-wordmark"
              priority
            />
            <Image
              src="/covenant-mark.svg"
              alt=""
              width={1140}
              height={1050}
              className="covenant-mobile-mark"
              priority
            />
          </a>
          <span className="brand-divider" aria-hidden="true" />
          <Link href="/" className="brand" aria-label="Mizuki home">
            <span className="brand-mark" aria-hidden="true">
              <Image
                src="/mizuki-mark.svg"
                alt=""
                width={1470}
                height={1050}
                className="mizuki-mark"
                priority
              />
            </span>
            <span>
              Mizuki<span className="brand-full"> the Mech</span>
            </span>
          </Link>
        </div>
        <nav className="site-nav" aria-label="Main navigation">
          {navigation.map((item) => (
            <Link key={item.href} href={item.href}>
              {item.label}
            </Link>
          ))}
        </nav>
        <details className="mobile-nav">
          <summary>Menu</summary>
          <nav aria-label="Mobile navigation">
            {navigation.map((item) => (
              <Link key={item.href} href={item.href}>
                {item.label}
              </Link>
            ))}
          </nav>
        </details>
        <Link href="/app" className="header-cta">
          <span className="header-cta-long">Open Workbench</span>
          <span className="header-cta-short">Workbench</span>
          <span aria-hidden="true">↗</span>
        </Link>
      </div>
    </header>
  );
}
