import { proxyStatus } from "../_proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request) {
  return proxyStatus(new URL(request.url).searchParams);
}
