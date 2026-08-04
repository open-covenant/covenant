// Tests for the bundle loader (loadBundles) and the paid /x402/myrad/signal
// handler (myrad-route.ts). The handler is mounted on a bare express app, the
// same shape it sees in server.ts after @x402/express has verified payment, so
// these cover the free-vs-charged boundary (>= 400 cancels settlement) without
// any facilitator or payment machinery.
import { after, before, describe, test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import type { Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import express from "express";
import { loadBundles, makeSignalHandler } from "../myrad-route.js";

const ISSUED = {
  resource: "covenant.myrad.signal.bundle.v1",
  signal: { cohort_id: "netflix_high_engagement_drama", contributors: 6 },
  receipt: {
    receipt: {
      schema: "covenant.myrad.signal.v1",
      signal: { cohort_id: "netflix_high_engagement_drama" },
      integrity: { status: "pass" },
    },
    root_hash_hex: "ab".repeat(32),
    signature_b64: "sig",
  },
};

function bundleFile(contents: unknown): string {
  const path = join(mkdtempSync(join(tmpdir(), "myrad-")), "bundle.json");
  writeFileSync(path, JSON.stringify(contents));
  return path;
}

/** Serve the given bundles on a real express app, as server.ts does. */
async function serve(path: string | undefined) {
  const app = express();
  app.get("/x402/myrad/signal/:cohort", makeSignalHandler(loadBundles(path), !path));
  const server: Server = await new Promise((resolve) => {
    const s = app.listen(0, "127.0.0.1", () => resolve(s));
  });
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  return { server, get: (cohort: string) => fetch(`${base}/x402/myrad/signal/${cohort}`) };
}

describe("loadBundles", () => {
  test("an array of bundles is keyed by cohort", () => {
    const second = { ...ISSUED, signal: { cohort_id: "netflix_casual_basic_drama", contributors: 9 } };
    const bundles = loadBundles(bundleFile([ISSUED, second]));
    assert.deepEqual(
      [...bundles.keys()].sort(),
      ["netflix_casual_basic_drama", "netflix_high_engagement_drama"],
    );
  });

  test("the cohort id is read from the receipt when the signal omits it", () => {
    const bundles = loadBundles(bundleFile({ receipt: ISSUED.receipt }));
    assert.ok(bundles.has("netflix_high_engagement_drama"));
  });

  test("two bundles for one cohort are rejected rather than silently overwritten", () => {
    const clash = { ...ISSUED, signal: { cohort_id: "netflix_high_engagement_drama", contributors: 99 } };
    assert.throws(() => loadBundles(bundleFile([ISSUED, clash])), /two bundles for cohort/);
  });

  // Every case here is one the boot path turns into process.exit(1).
  test("a bundle with no cohort id is rejected at load", () => {
    assert.throws(() => loadBundles(bundleFile({ resource: "covenant.myrad.signal.bundle.v1" })), /cohort_id/);
  });

  test("an empty list is rejected at load", () => {
    assert.throws(() => loadBundles(bundleFile([])), /contains no bundles/);
  });

  test("a missing file is rejected at load", () => {
    assert.throws(() => loadBundles("/nonexistent/bundle.json"), /ENOENT/);
  });

  test("malformed JSON is rejected at load", () => {
    const path = join(mkdtempSync(join(tmpdir(), "myrad-")), "bundle.json");
    writeFileSync(path, "{not json");
    assert.throws(() => loadBundles(path), SyntaxError);
  });
});

describe("GET /x402/myrad/signal/:cohort", () => {
  let configured: Awaited<ReturnType<typeof serve>>;
  let placeholder: Awaited<ReturnType<typeof serve>>;

  before(async () => {
    configured = await serve(bundleFile(ISSUED));
    placeholder = await serve(undefined);
  });

  after(async () => {
    for (const { server } of [configured, placeholder]) {
      await new Promise<void>((resolve, reject) => server.close((e) => (e ? reject(e) : resolve())));
    }
  });

  test("a configured cohort is served verbatim", async () => {
    const res = await configured.get("netflix_high_engagement_drama");
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), ISSUED);
  });

  test("an unknown cohort is a 404, so settlement is canceled and the buyer is not charged", async () => {
    const res = await configured.get("no_such_cohort");
    assert.equal(res.status, 404);
    assert.deepEqual(await res.json(), { error: "unknown cohort" });
  });

  test("the 404 does not echo the catalog, which would be free to enumerate", async () => {
    const body = (await (await configured.get("no_such_cohort")).json()) as Record<string, unknown>;
    assert.equal(body.available, undefined);
  });

  test("a hostile cohort param is a plain 404, not a prototype lookup", async () => {
    for (const cohort of ["__proto__", "constructor", "toString", "hasOwnProperty"]) {
      const res = await configured.get(cohort);
      assert.equal(res.status, 404, cohort);
    }
  });

  test("without a configured bundle the route is 503 and never settles", async () => {
    const res = await placeholder.get("reference_cohort");
    assert.equal(res.status, 503);
    const body = (await res.json()) as { error: string; note: string };
    assert.equal(body.error, "no issued bundle configured");
    assert.match(body.note, /COVENANT_MYRAD_BUNDLE/);
  });
});
