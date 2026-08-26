'use client';

import { usePathname } from 'next/navigation';
import { SiteFooter } from './site-footer';
import { SiteHeader } from './site-header';

export function SiteFrame({
  children,
  intakeOpen,
}: {
  children: React.ReactNode;
  intakeOpen: boolean;
}) {
  const pathname = usePathname();
  const workbench = pathname === '/app' || pathname.startsWith('/app/');

  if (workbench) {
    return (
      <>
        <a href="#main" className="skip-link">
          Skip to content
        </a>
        <main id="main" className="workbench-root">
          {children}
        </main>
      </>
    );
  }

  return (
    <>
      <a href="#main" className="skip-link">
        Skip to content
      </a>
      <SiteHeader />
      <main id="main">{children}</main>
      <SiteFooter intakeOpen={intakeOpen} />
    </>
  );
}
