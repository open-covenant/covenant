import { afterEach, describe, expect, it, vi } from "vitest";
import { bus } from "@/lib/agentBus.mjs";
import { POST } from "../route";

// POST /api/agent/ingest is the bearer-gated intake that fans live loop events
// out to the public agent stream. It is fail-closed on three fronts: disabled
// unless AGENT_INGEST_TOKEN is set, rejected unless the bearer matches, and it
// only republishes payload entries shaped like an event (object carrying a
// `type`). These tests pin each gate and the array/object normalization so a
// dropped auth check or a loosened shape guard cannot inject unstructured data
// into the stream.

const TOKEN = "s3cret-token";

function post(body: string, auth?: string) {
  const headers: Record<string, string> = {};
  if (auth !== undefined) headers.authorization = auth;
  return POST(new Request("http://x/api/agent/ingest", { method: "POST", headers, body }));
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllEnvs();
});

describe("agent ingest route", () => {
  it("503s when no ingest token is configured", async () => {
    vi.stubEnv("AGENT_INGEST_TOKEN", "");
    const res = await post(JSON.stringify({ type: "x" }), `Bearer ${TOKEN}`);
    expect(res.status).toBe(503);
    expect(await res.text()).toBe("ingest disabled");
  });

  it("401s a missing or mismatched bearer", async () => {
    vi.stubEnv("AGENT_INGEST_TOKEN", TOKEN);
    const pub = vi.spyOn(bus, "publish").mockImplementation(() => {});

    const missing = await post(JSON.stringify({ type: "x" }));
    expect(missing.status).toBe(401);
    const wrong = await post(JSON.stringify({ type: "x" }), "Bearer nope");
    expect(wrong.status).toBe(401);

    expect(pub).not.toHaveBeenCalled();
  });

  it("400s a body that is not valid JSON", async () => {
    vi.stubEnv("AGENT_INGEST_TOKEN", TOKEN);
    const pub = vi.spyOn(bus, "publish").mockImplementation(() => {});

    const res = await post("{not json", `Bearer ${TOKEN}`);
    expect(res.status).toBe(400);
    expect(await res.text()).toBe("bad json");
    expect(pub).not.toHaveBeenCalled();
  });

  it("publishes a single well-formed event and 204s", async () => {
    vi.stubEnv("AGENT_INGEST_TOKEN", TOKEN);
    const pub = vi.spyOn(bus, "publish").mockImplementation(() => {});

    const event = { type: "agent_event", phase: "in_progress" };
    const res = await post(JSON.stringify(event), `Bearer ${TOKEN}`);

    expect(res.status).toBe(204);
    expect(pub).toHaveBeenCalledTimes(1);
    expect(pub).toHaveBeenCalledWith(event);
  });

  it("normalizes an array and only republishes entries shaped like an event", async () => {
    vi.stubEnv("AGENT_INGEST_TOKEN", TOKEN);
    const pub = vi.spyOn(bus, "publish").mockImplementation(() => {});

    const res = await post(
      JSON.stringify([{ type: "a" }, { missing: "type" }, null, "string", 7, { type: "b" }]),
      `Bearer ${TOKEN}`,
    );

    expect(res.status).toBe(204);
    // Only the two typed objects survive the shape guard; the untyped object,
    // null, string, and number are dropped rather than forwarded raw.
    expect(pub.mock.calls.map((c) => c[0])).toEqual([{ type: "a" }, { type: "b" }]);
  });
});
