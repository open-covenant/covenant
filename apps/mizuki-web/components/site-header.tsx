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
        <Link href="/" className="brand" aria-label="Mizuki home">
          <span className="brand-mark" aria-hidden="true">
            M
          </span>
          <span>Mizuki</span>
        </Link>
        <nav className="site-nav" aria-label="Primary navigation">
          {navigation.map((item) => (
            <Link key={item.href} href={item.href}>
              {item.label}
            </Link>
          ))}
        </nav>
        <Link href="/work" className="header-cta">
          Submit an issue
          <span aria-hidden="true">↗</span>
        </Link>
      </div>
    </header>
  );
}
