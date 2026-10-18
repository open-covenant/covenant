import { proxyWork } from "../_proxy";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function GET(request: Request) { return proxyWork(new URL(request.url).searchParams); }
