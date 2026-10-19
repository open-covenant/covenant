import { proxyOperator } from "../_operator-proxy";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function GET(request: Request) { return proxyOperator(new URL(request.url).searchParams); }
