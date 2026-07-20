import { test } from "node:test";
import assert from "node:assert/strict";
import { Attestor, verifyAttestation } from "../attest.js";
import {
  canonicalSha256Hex,
  looksLikeReceipt,
  verifyReceipt,
  type CallReceipt,
} from "../receipt.js";
import { makeVerifyHandler } from "../verify-route.js";

function receipt(overrides: Partial<CallReceipt> = {}): CallReceipt {
  return {
    provider: "blockrun",
    endpoint: "/v1/chat/completions",
    modelRequested: "gpt-4o",
    modelServed: "gpt-4o-mini",
    verdict: "substituted",
    inputSha256: "aaaa",
    outputSha256: "bbbb",
    routing: { model: "gpt-4o-mini", savingsPct: 78 },
    payment: {
      network: "eip155:8453",
      asset: "0x8335",
      amount: "3000",
      amountUsdc: 0.003,
      payTo: "0xe903",
      tx: "0xabc",
    },
    ...overrides,
  };
}

test("digest matches the cross-language vector (Rust + TS + Python)", () => {
  // Identical to the covenant-blockrun crate canary and both SDK vectors. If this
  // changes, receipts stop verifying across the stack.
  assert.equal(
    canonicalSha256Hex(receipt()),
    "cff938c0db919fe334bc4e70b8ac545e2485537be66e381491af68276ad218a9",
  );
});

test("whole-number-float receipt matches the shared vector", () => {
  // savingsPct and amountUsdc as integral floats; the ".0" must drop to match
  // Rust and the SDKs.
  const r = {
    provider: "blockrun",
    endpoint: "/v1/chat/completions",
    modelRequested: "gpt-4o-mini",
    modelServed: "openai/gpt-4o-mini",
    verdict: "delivered",
    inputSha256: "aaaa",
    outputSha256: "bbbb",
    routing: { model: "openai/gpt-4o-mini", savingsPct: 78.0 },
    payment: {
      network: "eip155:8453",
      asset: "0x8335",
      amount: "1000000",
      amountUsdc: 1.0,
      payTo: "0xe903",
      tx: "0xabc",
    },
  };
  assert.equal(
    canonicalSha256Hex(r),
    "8328699dcfa1ab8c2d8e5cfca12378999ccf94cffe68fa2ee5cefec818041baf",
  );
});

test("verify accepts a consistent receipt and its digest", () => {
  const r = receipt();
  const digest = canonicalSha256Hex(r);
  const res = verifyReceipt(r, digest);
  assert.equal(res.valid, true);
  assert.equal(res.verdictConsistent, true);
  assert.equal(res.digestMatches, true);
  assert.equal(res.settled, true);
});

test("verify catches a tampered verdict", () => {
  const r = receipt({ verdict: "delivered" }); // served != requested, so should be substituted
  const res = verifyReceipt(r);
  assert.equal(res.verdictConsistent, false);
  assert.equal(res.expectedVerdict, "substituted");
  assert.equal(res.valid, false);
});

test("looksLikeReceipt rejects junk", () => {
  assert.equal(looksLikeReceipt({ foo: 1 }), false);
  assert.equal(looksLikeReceipt(receipt()), true);
});

test("handler returns a signed attestation over the result", () => {
  const attestor = Attestor.generate();
  const handler = makeVerifyHandler(attestor);
  let status = 200;
  let body: any;
  const res: any = {
    status(s: number) {
      status = s;
      return this;
    },
    json(b: any) {
      body = b;
      return this;
    },
  };
  handler({ body: { receipt: receipt() } } as any, res);
  assert.equal(status, 200);
  assert.equal(body.result.valid, true);
  assert.equal(verifyAttestation(body.attestation), true);
  // The attestation's subject is the receipt digest.
  assert.equal(body.attestation.payload.subject, body.result.digest);
});

test("handler rejects a malformed receipt without charging (>=400)", () => {
  const handler = makeVerifyHandler(Attestor.generate());
  let status = 200;
  const res: any = {
    status(s: number) {
      status = s;
      return this;
    },
    json() {
      return this;
    },
  };
  handler({ body: { receipt: { not: "a receipt" } } } as any, res);
  assert.equal(status, 400);
});
