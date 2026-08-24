import Image from 'next/image';
import Link from 'next/link';

const navigation = [
  { href: '/work', label: 'Hire Mizuki' },
  { href: '/bounties', label: 'Bounties' },
  { href: '/treasury', label: 'Treasury' },
  { href: '/capabilities', label: 'Capabilities' },
  { href: '/activity', label: 'Activity' },
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
            <Image src="/covenant-logo.svg" alt="Covenant" width={255} height={54} priority />
          </a>
          <span className="brand-divider" aria-hidden="true" />
          <Link href="/" className="brand" aria-label="Mizuki home">
            <span className="brand-mark" aria-hidden="true">
              <Image src="/mizuki-avatar.jpg" alt="" width={32} height={32} priority />
            </span>
            <span>Mizuki</span>
          </Link>
        </div>
        <nav className="site-nav" aria-label="Primary navigation">
          {navigation.map((item) => (
            <Link key={item.href} href={item.href}>
              {item.label}
            </Link>
          ))}
        </nav>
        <Link href="/work" className="header-cta">
          Open work
          <span aria-hidden="true">↗</span>
        </Link>
      </div>
    </header>
  );
}
