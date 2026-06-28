import { afterEach, describe, expect, it, vi } from "vitest";
import { POST } from "../route";

// POST /api/contact validates a contact submission and relays it through
// Resend. It is fail-closed: 503 until both RESEND_API_KEY and CONTACT_TO are
// set, 400 on non-JSON or on a missing/invalid name/email/message, a silent
// {ok:true} for honeypot (company) submissions, and 502 when Resend rejects the
// send. These tests pin each arm and the relayed payload with a stubbed fetch so
// no submission is dropped or forwarded unvalidated.

const KEY = "test-key";
const TO = "ops@example.com";
const VALID = { name: "Alice", email: "alice@example.com", message: "hello there" };

function post(body: unknown) {
  return POST(
    new Request("http://x/api/contact", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: typeof body === "string" ? body : JSON.stringify(body),
    }),
  );
}

function configured() {
  vi.stubEnv("RESEND_API_KEY", KEY);
  vi.stubEnv("CONTACT_TO", TO);
}

function stubFetch(ok: boolean) {
  const f = vi.fn().mockResolvedValue({ ok });
  vi.stubGlobal("fetch", f);
  return f;
}

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("contact route", () => {
  it("503s until both key and recipient are configured", async () => {
    vi.stubEnv("RESEND_API_KEY", "");
    vi.stubEnv("CONTACT_TO", TO);
    const noKey = await post(VALID);
    expect(noKey.status).toBe(503);

    vi.stubEnv("RESEND_API_KEY", KEY);
    vi.stubEnv("CONTACT_TO", "");
    const noTo = await post(VALID);
    expect(noTo.status).toBe(503);
    expect(await noTo.json()).toEqual({ error: "contact endpoint not configured" });
  });

  it("400s a body that is not valid JSON", async () => {
    configured();
    const res = await post("{ not json");
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "invalid request" });
  });

  it("silently accepts a honeypot submission without relaying it", async () => {
    configured();
    const f = stubFetch(true);
    const res = await post({ ...VALID, company: "spam-bot" });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });
    expect(f).not.toHaveBeenCalled();
  });

  it("400s a missing field or an invalid email and never relays", async () => {
    configured();
    const f = stubFetch(true);

    const missing = await post({ name: "Alice", email: "alice@example.com" });
    expect(missing.status).toBe(400);

    const badEmail = await post({ ...VALID, email: "not-an-email" });
    expect(badEmail.status).toBe(400);
    expect(await badEmail.json()).toEqual({
      error: "name, valid email, and message are required",
    });

    expect(f).not.toHaveBeenCalled();
  });

  it("relays a valid submission to Resend and returns ok", async () => {
    configured();
    const f = stubFetch(true);
    const res = await post(VALID);

    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });
    expect(f).toHaveBeenCalledTimes(1);
    const [url, init] = f.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("https://api.resend.com/emails");
    expect((init.headers as Record<string, string>).Authorization).toBe(`Bearer ${KEY}`);
    const payload = JSON.parse(init.body as string);
    expect(payload.to).toEqual([TO]);
    expect(payload.reply_to).toBe(VALID.email);
  });

  it("502s when Resend rejects the send", async () => {
    configured();
    stubFetch(false);
    const res = await post(VALID);
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({ error: "delivery failed" });
  });
});
