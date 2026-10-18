import { proxyWork } from "../../_proxy";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function GET(request: Request, { params }: { params: Promise<{ "work-id": string }> }) {
  return proxyWork(new URL(request.url).searchParams, "detail", (await params)["work-id"]);
}
