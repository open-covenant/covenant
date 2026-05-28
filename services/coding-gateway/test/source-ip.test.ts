import { describe, it, expect } from "vitest";
import type { IncomingMessage } from "node:http";
import { sourceIp } from "../src/source-ip.js";

function mockReq(
  remoteAddress: string | undefined,
  xff?: string | string[],
): IncomingMessage {
  const headers: Record<string, string | string[] | undefined> = {};
  if (xff !== undefined) headers["x-forwarded-for"] = xff;
  return {
    headers,
    socket: { remoteAddress } as IncomingMessage["socket"],
  } as IncomingMessage;
}

describe("sourceIp", () => {
  it("ignores X-Forwarded-For when trustedHops is 0", () => {
    const req = mockReq("10.0.0.42", "1.2.3.4, 5.6.7.8");
    expect(sourceIp(req, 0)).toBe("10.0.0.42");
  });

  it("uses the right-most XFF entry when trustedHops is 1", () => {
    // Chain: client → edge → gateway. With trustedHops=1, the edge is
    // trusted, the right-most XFF entry is the client.
    const req = mockReq("10.0.0.42", "1.2.3.4");
    expect(sourceIp(req, 1)).toBe("1.2.3.4");
  });

  it("walks left by trustedHops to skip trusted proxies", () => {
    // Chain: client → edge → internal LB → gateway. With trustedHops=2,
    // two right-most entries are trusted; the remaining left entry is
    // the client. XFF written as client, edge, LB (proxies append on
    // the right).
    const req = mockReq("10.0.0.42", "1.2.3.4, 9.9.9.1, 9.9.9.2");
    expect(sourceIp(req, 2)).toBe("1.2.3.4");
  });

  it("clamps trustedHops to the chain length so a too-large value never reads off the end", () => {
    // Operator misconfigures TRUSTED_PROXY_HOPS=5 with a 2-hop chain.
    // Safer to under-attribute (treat the left-most as the source)
    // than to fall back to the socket peer (which a proxy in front of
    // the gateway would always surface as the proxy's own loopback).
    const req = mockReq("127.0.0.1", "1.2.3.4, 9.9.9.1");
    expect(sourceIp(req, 5)).toBe("1.2.3.4");
  });

  it("falls back to socket address when XFF is absent", () => {
    const req = mockReq("10.0.0.42");
    expect(sourceIp(req, 2)).toBe("10.0.0.42");
  });

  it("strips IPv4-in-IPv6 ::ffff: prefix from the socket fallback", () => {
    const req = mockReq("::ffff:1.2.3.4");
    expect(sourceIp(req, 0)).toBe("1.2.3.4");
  });

  it("strips IPv4-in-IPv6 ::ffff: prefix from XFF too so the bucket cannot be split by alternating forms", () => {
    // Some node-based reverse proxies forward the mapped form on XFF.
    // Without this strip, the same client would key two distinct
    // bucket entries (`::ffff:1.2.3.4` vs `1.2.3.4`) and bypass the
    // per-IP cap by alternating between direct and through-proxy
    // submissions. Pin the symmetry.
    const req = mockReq("10.0.0.42", "::ffff:1.2.3.4");
    expect(sourceIp(req, 1)).toBe("1.2.3.4");
  });

  it("returns 'unknown' when neither the socket nor a trusted XFF entry is present", () => {
    const req = mockReq(undefined);
    expect(sourceIp(req, 0)).toBe("unknown");
  });

  it("tolerates whitespace and array-form X-Forwarded-For", () => {
    const req = mockReq("10.0.0.42", ["1.2.3.4 ", " 9.9.9.1"]);
    expect(sourceIp(req, 1)).toBe("1.2.3.4");
  });

  it("ignores empty XFF entries that proxies sometimes emit", () => {
    const req = mockReq("10.0.0.42", "1.2.3.4, , 9.9.9.1");
    expect(sourceIp(req, 1)).toBe("1.2.3.4");
  });
});
