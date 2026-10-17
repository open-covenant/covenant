import { proxyPriceData } from "../../compute-prices/_proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request) {
  return proxyPriceData("/prices/history", new URL(request.url).searchParams);
}
