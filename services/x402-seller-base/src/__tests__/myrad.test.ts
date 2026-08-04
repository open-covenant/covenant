import { strict as assert } from "node:assert";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { Request, Response } from "express";
import { loadBundles, makeSignalHandler } from "../myrad-route.js";

function fakeRes() {
  const captured: { status: number; body: unknown } = { status: 200, body: undefined };
  const res = {
    status(code: number) {
      captured.status = code;
      return this;
    },
    json(body: unknown) {
      captured.body = body;
      return this;
    },
  };
  return { res: res as unknown as Response, captured };
}

function req(cohort: string) {
  return { params: { cohort } } as unknown as Request;
}

function bundleFile(contents: unknown): string {
  const path = join(mkdtempSync(join(tmpdir(), "myrad-")), "bundle.json");
  writeFileSync(path, JSON.stringify(contents));
  return path;
}

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

test("without a configured bundle it serves a labelled synthetic cohort", () => {
  const bundles = loadBundles(undefined);
  const { res, captured } = fakeRes();
  makeSignalHandler(bundles, true)(req("reference_cohort"), res);

  assert.equal(captured.status, 200);
  const body = captured.body as Record<string, unknown>;
  assert.match(String(body.note), /synthetic/);
});

test("a configured bundle is served verbatim, with no synthetic note", () => {
  const bundles = loadBundles(bundleFile(ISSUED));
  const { res, captured } = fakeRes();
  makeSignalHandler(bundles, false)(req("netflix_high_engagement_drama"), res);

  assert.equal(captured.status, 200);
  assert.deepEqual(captured.body, ISSUED);
});

test("an unknown cohort is a 404, so settlement is cancelled and the buyer is not charged", () => {
  const bundles = loadBundles(bundleFile(ISSUED));
  const { res, captured } = fakeRes();
  makeSignalHandler(bundles, false)(req("no_such_cohort"), res);

  assert.equal(captured.status, 404);
  const body = captured.body as { error: string; available: string[] };
  assert.equal(body.error, "unknown cohort");
  assert.deepEqual(body.available, ["netflix_high_engagement_drama"]);
});

test("an array of bundles is keyed by cohort", () => {
  const second = {
    ...ISSUED,
    signal: { cohort_id: "netflix_casual_basic_drama", contributors: 9 },
  };
  const bundles = loadBundles(bundleFile([ISSUED, second]));
  assert.deepEqual(
    [...bundles.keys()].sort(),
    ["netflix_casual_basic_drama", "netflix_high_engagement_drama"],
  );
});

test("a bundle with no cohort id is rejected at load rather than served", () => {
  assert.throws(() => loadBundles(bundleFile({ resource: "covenant.myrad.signal.bundle.v1" })), /cohort_id/);
});

test("the cohort id is read from the receipt when the signal omits it", () => {
  const receiptOnly = { receipt: ISSUED.receipt };
  const bundles = loadBundles(bundleFile(receiptOnly));
  assert.ok(bundles.has("netflix_high_engagement_drama"));
});
