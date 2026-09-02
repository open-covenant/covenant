/**
 * The priced repository assessment, served under the public Mizuki origin.
 *
 * The assessment itself runs on the Covenant evidence service. This route
 * exists so the capability is reachable at the same origin as the rest of the
 * Mizuki API, which is what agents and the public catalogs expect: an endpoint
 * is addressed relative to the service they already know.
 *
 * Payment is unchanged by passing through here. The challenge names the
 * evidence service as its resource, and that service accepts an authorization
 * only for its own URL, so a caller settles there directly. The catch-all proxy
 * next to this file forwards to the job runtime instead, which does not serve
 * this path.
 */

export const dynamic = 'force-dynamic';
export const runtime = 'nodejs';

const EVIDENCE_URL = (
  process.env.COVENANT_EVIDENCE_URL ?? 'https://covenant-x402-seller.onrender.com'
).replace(/\/$/, '');

/** GitHub owner and repository names, so a path segment cannot redirect the request. */
const NAME = /^[A-Za-z0-9._-]{1,100}$/;

const forwardedRequestHeaders = ['accept', 'payment-signature', 'user-agent'];
const forwardedResponseHeaders = [
  'content-type',
  'payment-required',
  'payment-response',
  'cache-control',
];

export async function GET(
  request: Request,
  context: { params: Promise<{ owner: string; repo: string }> },
): Promise<Response> {
  const { owner, repo } = await context.params;
  if (!NAME.test(owner) || !NAME.test(repo) || owner === '..' || repo === '..') {
    return Response.json({ error: 'owner and repo must be GitHub names' }, { status: 400 });
  }

  const target = `${EVIDENCE_URL}/x402/mizuki/assess/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}`;
  const headers = new Headers();
  for (const name of forwardedRequestHeaders) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }

  let upstream: Response;
  try {
    upstream = await fetch(target, {
      method: 'GET',
      headers,
      redirect: 'error',
      signal: AbortSignal.timeout(20_000),
    });
  } catch {
    return Response.json({ error: 'assessment service is unavailable' }, { status: 502 });
  }

  const response = new Headers();
  for (const name of forwardedResponseHeaders) {
    const value = upstream.headers.get(name);
    if (value) response.set(name, value);
  }
  return new Response(upstream.body, { status: upstream.status, headers: response });
}
