import { proxyPriceData } from "../../../compute-prices/_proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request, { params }: { params: Promise<{ job_id: string }> }) {
  return proxyPriceData(`/prices/proofs/${(await params).job_id}`, new URL(request.url).searchParams);
}
