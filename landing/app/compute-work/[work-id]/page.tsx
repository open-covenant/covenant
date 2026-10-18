import { cache } from "react";
import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { pageMetadata } from "../../_metadata";
import { SiteHeader } from "../../SiteHeader";
import { SiteFooter } from "../../SiteFooter";
import { WorkArtifact } from "../WorkArtifact";
import { readWorkDetail } from "../_proxy";
import { cardHeadline, WORK_ID } from "../_work";

export const dynamic = "force-dynamic";
const getDetail = cache(readWorkDetail);
type Context = { params: Promise<{ "work-id": string }> };

export async function generateMetadata({ params }: Context): Promise<Metadata> {
  const id = (await params)["work-id"];
  if (!WORK_ID.test(id)) return { title: "Work record unavailable · Covenant", robots: { index: false } };
  const data = await getDetail(id);
  return pageMetadata(`/compute-work/${id}`, { title: data ? cardHeadline(data.work, data.mode) : "Public work record unavailable", section: "Compute / Public work" }, {
    title: data ? `${data.work.title} · Covenant Compute` : "Public work record unavailable · Covenant Compute",
    description: data?.work.summary ?? "This public contribution record could not be retrieved. No funding or completion can be confirmed.",
    ...(!data ? { robots: { index: false, follow: false } } : {}),
  });
}

export default async function WorkPage({ params }: Context) {
  const id = (await params)["work-id"];
  if (!WORK_ID.test(id)) notFound();
  const data = await getDetail(id);
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground"><SiteHeader /><div className="page-container"><a href="/compute-work" className="mb-10 inline-block text-[12px] text-muted underline decoration-line-strong underline-offset-4">← All public work</a>
    {data ? <WorkArtifact data={data} /> : <section className="border-y border-line py-8"><h1 className="type-display text-[2rem] text-fg">Public work record unavailable</h1><p className="mt-5 max-w-2xl text-[14px] leading-[1.8] text-fg-soft">This record could not be retrieved. No funding, execution or completion can be confirmed. Refresh to retry.</p></section>}
  </div><SiteFooter className="pb-8" /></main>;
}
