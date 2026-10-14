import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { BrandNav } from "./BrandNav";

/**
 * The brand guidelines shell: the site header and footer around a sidebar of
 * sections and the open page. Every /brand page renders inside it.
 */
export default function BrandLayout({ children }: { children: React.ReactNode }) {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-clip bg-ground">
      <SiteHeader />
      <div className="mx-auto w-full max-w-[90rem] px-5 pt-[calc(var(--header-h)+env(safe-area-inset-top))] sm:px-8 lg:grid lg:grid-cols-[14rem_minmax(0,1fr)] lg:gap-14 xl:gap-20">
        <BrandNav />
        <div className="min-w-0 max-w-[70rem] pb-28 pt-10 lg:pt-16">{children}</div>
      </div>
      <SiteFooter />
    </main>
  );
}
