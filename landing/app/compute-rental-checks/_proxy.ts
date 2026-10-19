import { COMPUTE_ORIGIN, COMPUTE_TIMEOUT_MS, readComputeBody } from "../og/_compute";
import { RENTAL_CHECKS_MAX_BYTES, validateRentalChecks } from "./_status";

const headers = { "content-type":"application/json; charset=utf-8", "cache-control":"private, no-store, max-age=0", "x-content-type-options":"nosniff" };
const unavailable = () => new Response(JSON.stringify({error:"Rental-check data temporarily unavailable"}),{status:503,headers});
export async function proxyRentalChecks(query: URLSearchParams, fetcher: typeof fetch = fetch): Promise<Response> {
  if (query.size) return new Response(JSON.stringify({error:"Rental-check queries are not supported"}),{status:400,headers});
  try {
    const upstream = await fetcher(`${COMPUTE_ORIGIN}/rental-checks`,{redirect:"manual",credentials:"omit",cache:"no-store",headers:{accept:"application/json","user-agent":"Covenant-Rental-Checks/1.0"},signal:AbortSignal.timeout(COMPUTE_TIMEOUT_MS)});
    if (upstream.status !== 200 || upstream.redirected || upstream.headers.has("set-cookie") || upstream.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") {
      await upstream.body?.cancel();
      return unavailable();
    }
    const data = validateRentalChecks(JSON.parse(await readComputeBody(upstream,RENTAL_CHECKS_MAX_BYTES)));
    return new Response(JSON.stringify(data),{headers});
  } catch { return unavailable(); }
}
