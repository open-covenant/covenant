import Link from "next/link";
import { HeaderShell } from "./HeaderShell";
import { HeaderStats } from "./HeaderStats";
import { SiteMenu } from "./SiteMenu";
import { CovenantLockup } from "./_brand";

/**
 * Site-wide header bar: the lockup on the left; live stats (from xl) and the
 * menu button on the right. Navigation lives in the side menu at every width.
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
          <SiteMenu />
        </div>
      </div>
    </HeaderShell>
  );
}
