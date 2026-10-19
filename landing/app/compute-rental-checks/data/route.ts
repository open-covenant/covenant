import { proxyRentalChecks } from "../_proxy";
import { rentalChecksRelease } from "../_release";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function GET(request: Request) {
  if (!rentalChecksRelease.accepted) return new Response(JSON.stringify({error:"Rental checks are not released"}),{status:404,headers:{"content-type":"application/json","cache-control":"no-store"}});
  return proxyRentalChecks(new URL(request.url).searchParams);
}
