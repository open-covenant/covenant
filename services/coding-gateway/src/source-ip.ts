import type { IncomingMessage } from "node:http";

/**
 * Resolve the source IP for admission control. The right-most
 * `trustedHops` entries of `X-Forwarded-For` are added by proxies the
 * operator controls; anything further left was added by the client and
 * can be forged trivially. With `trustedHops = 0` (the default) we
 * ignore X-Forwarded-For entirely and use the socket peer — safe for
 * any deployment, but every visitor behind shared NAT or a single edge
 * collapses to one address.
 *
 * Operators set `TRUSTED_PROXY_HOPS` to the number of proxies between
 * the gateway and the public internet (one for a single Cloudflare,
 * Fly.io, or Render edge; two for an edge + an internal LB). Picking
 * the value too high means a client controls part of the chain and can
 * rotate IPs trivially — failure mode 1 of the parent task.
 */
export function sourceIp(req: IncomingMessage, trustedHops: number): string {
  if (trustedHops > 0) {
    const header = req.headers["x-forwarded-for"];
    const raw = Array.isArray(header) ? header.join(",") : header;
    if (typeof raw === "string" && raw.length > 0) {
      const hops = raw
        .split(",")
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
      if (hops.length > 0) {
        // The right-most entry is added by our nearest trusted proxy;
        // walk `trustedHops` slots leftward to reach the original
        // client. If the chain is shorter than `trustedHops`, treat the
        // left-most as the source — under-counted trust is safer than
        // mis-attributed (we never deeper into a client-controlled
        // segment).
        const idx = Math.max(0, hops.length - 1 - trustedHops);
        const candidate = hops[idx];
        if (candidate) return stripV4InV6(candidate);
      }
    }
  }
  const remote = req.socket.remoteAddress;
  if (!remote) return "unknown";
  return stripV4InV6(remote);
}

// Strip the IPv4-in-IPv6 `::ffff:` prefix so the bucket keys on the same
// string whether the client hit the socket directly (some Node versions
// surface `::ffff:1.2.3.4`) or a proxy forwarded the mapped form on
// X-Forwarded-For. Without this, the same client would key two buckets
// and bypass the per-IP cap by alternating between the two forms.
function stripV4InV6(ip: string): string {
  return ip.startsWith("::ffff:") ? ip.slice(7) : ip;
}
