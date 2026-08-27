'use client';

import Image from 'next/image';
import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { useEffect, useState } from 'react';
import { githubAuthErrorMessage, normalizeAccount, workbenchAuthHref } from '@/lib/workbench';
import {
  logoutWorkbench,
  onWorkbenchUnauthorized,
  useWorkbenchResource,
} from '@/lib/workbench-client';
import { paymentApplicationBuild } from '@/lib/payment';

export type WorkbenchNavigationItem = {
  href: string;
  label: string;
  icon: string;
  exact?: boolean;
};

export const primaryNavigation: WorkbenchNavigationItem[] = [
  { href: '/app', label: 'Overview', icon: '01', exact: true },
  { href: '/app/repositories', label: 'Repositories', icon: '02' },
  { href: '/app/jobs', label: 'Jobs', icon: '03' },
  { href: '/app/bounties', label: 'Bounties', icon: '04' },
  { href: '/app/billing', label: 'Payments & refunds', icon: '05' },
];

export const secondaryNavigation: WorkbenchNavigationItem[] = [
  { href: '/app/integrations', label: 'Integrations', icon: '06' },
  { href: '/app/settings', label: 'Settings', icon: '07' },
];

export function WorkbenchShell({
  children,
  walletControl,
}: {
  children: React.ReactNode;
  walletControl: React.ReactNode;
}) {
  const pathname = usePathname();
  const [authExpired, setAuthExpired] = useState(false);
  const [authError, setAuthError] = useState<string>();
  const [returnTo, setReturnTo] = useState(pathname);
  const [logoutPending, setLogoutPending] = useState(false);
  const [logoutError, setLogoutError] = useState<string>();
  const account = useWorkbenchResource('/v1/account', normalizeAccount);

  useEffect(() => onWorkbenchUnauthorized(() => setAuthExpired(true)), []);
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const message = githubAuthErrorMessage(params.get('auth_error'));
    params.delete('auth_error');
    const cleanPath = `${pathname}${params.size ? `?${params}` : ''}`;
    setAuthError(message);
    setReturnTo(cleanPath);
    if (message && `${window.location.pathname}${window.location.search}` !== cleanPath) {
      window.history.replaceState(window.history.state, '', cleanPath);
    }
  }, [pathname]);

  if (account.status === 'loading') return <WorkbenchShellLoading />;
  if (account.status === 'unauthorized' || authExpired) {
    return <WorkbenchSignIn returnTo={returnTo} authError={authError} />;
  }
  if (account.status === 'error') {
    return (
      <WorkbenchAccessState
        mark="!"
        title="Workbench could not confirm your GitHub account"
        detail="No payment or repository action was attempted. Try loading your account again."
        action={<button onClick={account.refresh}>Try again</button>}
      />
    );
  }

  async function logout() {
    setLogoutPending(true);
    setLogoutError(undefined);
    try {
      await logoutWorkbench(() => {
        window.location.replace('/app');
      });
    } catch {
      setLogoutError('Sign-out could not be confirmed. This page remains signed in; try again.');
    } finally {
      setLogoutPending(false);
    }
  }

  return (
    <div className="workbench-shell">
      <aside className="workbench-rail">
        <a
          className="workbench-covenant-brand"
          href="https://opencovenant.org"
          aria-label="Covenant home"
        >
          <Image src="/covenant-mark.svg" alt="Covenant" width={1140} height={1050} priority />
        </a>
        <Link className="workbench-brand" href="/app" aria-label="Mizuki Workbench home">
          <Image
            src="/mizuki-mark.svg"
            alt=""
            width={1470}
            height={1050}
            className="mizuki-mark"
            priority
          />
          <span>
            <strong>Mizuki</strong>
            <small>Maintenance workbench</small>
          </span>
        </Link>

        <Link className="workbench-new-job" href="/app/jobs/new">
          <span className="workbench-new-job-icon" aria-hidden="true">
            +
          </span>
          <span>New job</span>
        </Link>

        <WorkbenchNavigation pathname={pathname} items={primaryNavigation} />
        <div className="workbench-rail-spacer" />
        <WorkbenchNavigation pathname={pathname} items={secondaryNavigation} />

        <div className="workbench-account">
          <div className="workbench-account-mark" aria-hidden="true">
            {account.data.githubLogin.slice(0, 1).toUpperCase()}
          </div>
          <div>
            <strong>@{account.data.githubLogin}</strong>
            <button type="button" onClick={() => void logout()} disabled={logoutPending}>
              {logoutPending ? 'Signing out…' : 'Sign out'}
            </button>
            {logoutError && (
              <span className="workbench-logout-error" role="alert">
                {logoutError}
              </span>
            )}
          </div>
        </div>
      </aside>

      <WorkbenchHeader walletControl={walletControl} />

      <section className="workbench-content">
        {authError && (
          <div className="workbench-auth-alert" role="alert">
            <span>{authError}</span>
            <button type="button" onClick={() => setAuthError(undefined)} aria-label="Dismiss">
              Dismiss
            </button>
          </div>
        )}
        {children}
        <footer className="workbench-diagnostic" aria-label="Workbench build">
          Build {shortBuildId(paymentApplicationBuild())}
        </footer>
      </section>

      <nav className="workbench-mobile-nav" aria-label="Workbench navigation">
        {primaryNavigation.slice(0, 4).map((item) => (
          <WorkbenchNavLink item={item} pathname={pathname} key={item.href} />
        ))}
        <WorkbenchNavLink
          item={{ href: '/app/settings', label: 'More', icon: '07' }}
          pathname={pathname}
        />
      </nav>
    </div>
  );
}

function shortBuildId(value: string): string {
  return value === 'development' ? value : value.slice(0, 12);
}

export function WorkbenchHeader({ walletControl }: { walletControl: React.ReactNode }) {
  return (
    <header className="workbench-header">
      <Link className="workbench-header-brand" href="/app" aria-label="Mizuki Workbench home">
        <Image
          className="workbench-header-covenant"
          src="/covenant-mark.svg"
          alt=""
          width={24}
          height={24}
        />
        <span className="workbench-header-divider" aria-hidden="true" />
        <Image src="/mizuki-mark.svg" alt="" width={1470} height={1050} className="mizuki-mark" />
        <strong>Mizuki</strong>
      </Link>
      <div className="workbench-header-context">
        <span>Authenticated console</span>
        <strong>Maintenance workbench</strong>
      </div>
      <div className="workbench-header-actions">
        <Link className="workbench-header-new-job" href="/app/jobs/new">
          New job
        </Link>
        {walletControl}
      </div>
    </header>
  );
}

function WorkbenchNavigation({
  pathname,
  items,
}: {
  pathname: string;
  items: WorkbenchNavigationItem[];
}) {
  return (
    <nav className="workbench-navigation" aria-label="Workbench sections">
      {items.map((item) => (
        <WorkbenchNavLink item={item} pathname={pathname} key={item.href} />
      ))}
    </nav>
  );
}

export function WorkbenchNavLink({
  item,
  pathname,
}: {
  item: WorkbenchNavigationItem;
  pathname: string;
}) {
  const current = item.exact ? pathname === item.href : pathname.startsWith(item.href);
  return (
    <Link href={item.href} aria-current={current ? 'page' : undefined}>
      <span className="workbench-nav-icon" aria-hidden="true">
        {item.icon}
      </span>
      <span>{item.label}</span>
    </Link>
  );
}

function WorkbenchSignIn({ returnTo, authError }: { returnTo: string; authError?: string }) {
  return (
    <WorkbenchAccessState
      mark="01"
      title="Sign in to Mizuki Workbench"
      detail={
        authError ??
        'Use GitHub to manage public repositories, request fixed quotes, and track pull requests or refunds.'
      }
      action={
        <a href={workbenchAuthHref(returnTo)}>
          Continue with GitHub <span aria-hidden="true">↗</span>
        </a>
      }
      secondary={<Link href="/work">Learn how paid maintenance works</Link>}
    />
  );
}

function WorkbenchAccessState({
  mark,
  title,
  detail,
  action,
  secondary,
}: {
  mark: string;
  title: string;
  detail: string;
  action: React.ReactNode;
  secondary?: React.ReactNode;
}) {
  return (
    <div className="workbench-access">
      <a
        className="workbench-access-covenant"
        href="https://opencovenant.org"
        aria-label="Covenant home"
      >
        <Image src="/covenant-mark.svg" alt="Covenant" width={1140} height={1050} priority />
      </a>
      <div className="workbench-access-card">
        <Link className="workbench-access-brand" href="/">
          <Image
            src="/mizuki-mark.svg"
            alt=""
            width={1470}
            height={1050}
            className="mizuki-mark"
            priority
          />
          <span>
            <strong>Mizuki the Mech</strong>
            <small>Maintenance workbench</small>
          </span>
        </Link>
        <span className="workbench-access-mark" aria-hidden="true">
          {mark}
        </span>
        <h1>{title}</h1>
        <p>{detail}</p>
        <div className="workbench-access-actions">
          {action}
          {secondary}
        </div>
      </div>
    </div>
  );
}

function WorkbenchShellLoading() {
  return (
    <div className="workbench-shell workbench-shell-loading" aria-busy="true">
      <aside className="workbench-rail">
        <div className="workbench-skeleton brand-skeleton" />
        <div className="workbench-skeleton action-skeleton" />
        {Array.from({ length: 7 }, (_, index) => (
          <div className="workbench-skeleton nav-skeleton" key={index} />
        ))}
      </aside>
      <header className="workbench-header">
        <div className="workbench-skeleton header-skeleton" />
      </header>
      <section className="workbench-content">
        <div className="workbench-skeleton heading-skeleton" />
        <div className="workbench-skeleton panel-skeleton" />
      </section>
    </div>
  );
}
