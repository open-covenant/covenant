import Link from "next/link";
import { HeaderShell } from "./HeaderShell";
import { HeaderStats } from "./HeaderStats";
import { SiteMenu } from "./SiteMenu";
import { CovenantLockup, SOCIAL_LINKS } from "./_brand";

/**
 * Site-wide header bar: the lockup on the left; live stats (from xl), the
 * social links (from sm) and the menu button on the right. Navigation lives in
 * the side menu at every width, and so do the socials on phones.
 * Single source of truth: home and every top-level subpage mount this.
 */
export function SiteHeader() {
  return (
    <HeaderShell>
      <div className="flex h-(--header-h) w-full items-center justify-between gap-4 px-[clamp(20px,3.4vw,50px)]">
        <Link href="/" aria-label="Covenant home" className="shrink-0 text-fg">
          <CovenantLockup markClassName="h-[26px] w-auto sm:h-[30px]" />
        </Link>

        <div className="flex shrink-0 items-center gap-6">
          <div className="hidden items-center gap-6 xl:flex">
            <HeaderStats />
            <span aria-hidden className="h-4 w-px bg-line" />
          </div>
          <div className="flex items-center gap-2">
            <div className="hidden items-center sm:flex">
              {SOCIAL_LINKS.map(({ label, href, Icon }) => (
                <a
                  key={href}
                  href={href}
                  target="_blank"
                  rel="noopener noreferrer"
                  aria-label={`Covenant on ${label}`}
                  className="inline-flex h-10 w-10 items-center justify-center rounded-full text-muted transition-colors hover:bg-raised hover:text-fg"
                >
                  <Icon className="h-4 w-4" />
                </a>
              ))}
            </div>
            <SiteMenu />
          </div>
        </div>
      </div>
    </HeaderShell>
  );
}
