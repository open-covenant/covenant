// Tests for the attestation core (attest.ts) and the paid /x402/attest handler
// (attest-route.ts). The handler is mounted on a bare express app with only the
// JSON body parser — exactly what it sees in server.ts after @x402/express has
// verified payment — so these tests cover the free-vs-charged boundary (>= 400
// cancels settlement) without any facilitator or payment machinery.
//
// Note: the signature scheme is ed25519, which has no public-key recovery
// (ecrecover is a secp256k1/EVM concept). The signer-binding property tested here
// is the ed25519 equivalent: the attestation carries pubkey_b58, it must equal
// the attestor's published key, and verification must fail under any other key.
import { after, before, describe, test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import type { Server } from "node:http";
import type { AddressInfo } from "node:net";
import express from "express";
import { ATTEST_DOMAIN, Attestor, verifyAttestation, type Attestation } from "../attest.js";
import { makeAttestHandler } from "../attest-route.js";

describe("attestation round-trip", () => {
  const attestor = Attestor.generate();

  test("sign then verify succeeds and binds the signer's pubkey", () => {
    const att = attestor.attest("did:example:agent-1", { delivered: true }, 1_700_000_000);
    assert.equal(att.alg, "ed25519");
    assert.equal(att.domain, ATTEST_DOMAIN);
    assert.equal(att.pubkey_b58, attestor.pubkeyB58);
    assert.deepEqual(att.payload, { subject: "did:example:agent-1", claim: { delivered: true }, ts: 1_700_000_000 });
    assert.equal(verifyAttestation(att), true);
  });

  test("digest follows the published recipe: sha256 of key-sorted canonical JSON", () => {
    const att = attestor.attest("subject", { b: 1, a: [null, "x"] }, 42);
    const canonical = '{"claim":{"a":[null,"x"],"b":1},"subject":"subject","ts":42}';
    assert.equal(att.digest_sha256_hex, createHash("sha256").update(canonical, "utf8").digest("hex"));
  });

  test("canonicalization is key-order independent (ed25519 signing is deterministic)", () => {
    const a = attestor.attest("s", { b: 1, a: 2 }, 7);
    const b = attestor.attest("s", { a: 2, b: 1 }, 7);
    assert.equal(a.digest_sha256_hex, b.digest_sha256_hex);
    assert.equal(a.signature_b58, b.signature_b58);
  });

  test("tampered claim fails verification", () => {
    const att = attestor.attest("subject", { paid: true }, 1);
    const tampered: Attestation = { ...att, payload: { ...att.payload, claim: { paid: false } } };
    assert.equal(verifyAttestation(tampered), false);
  });

  test("tampered digest fails verification", () => {
    const att = attestor.attest("subject", "claim", 1);
    assert.equal(verifyAttestation({ ...att, digest_sha256_hex: att.digest_sha256_hex.replace(/^./, "f") }), false);
  });

  test("attestation does not verify under a different signer's pubkey", () => {
    const att = attestor.attest("subject", "claim", 1);
    assert.equal(verifyAttestation({ ...att, pubkey_b58: Attestor.generate().pubkeyB58 }), false);
  });

  test("signature forged by another key fails against the published pubkey", () => {
    const att = attestor.attest("subject", "claim", 1);
    const forged = Attestor.generate().attest("subject", "claim", 1);
    assert.equal(verifyAttestation({ ...att, signature_b58: forged.signature_b58 }), false);
  });
});

describe("POST /x402/attest handler", () => {
  const attestor = Attestor.generate();
  let server: Server;
  let base: string;

  before(async () => {
    const app = express();
    app.use(express.json());
    app.post("/x402/attest", makeAttestHandler(attestor));
    await new Promise<void>((resolve) => {
      server = app.listen(0, "127.0.0.1", resolve);
    });
    base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  });

  after(() => new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve()))));

  const post = (body?: string, headers: Record<string, string> = { "content-type": "application/json" }) =>
    fetch(`${base}/x402/attest`, { method: "POST", headers, body });

  const BAD_REQUEST = { error: "subject (1-256 char string) and claim are required" };

  test("missing body → 400 with the validation error", async () => {
    const res = await post(undefined, {});
    assert.equal(res.status, 400);
    assert.deepEqual(await res.json(), BAD_REQUEST);
  });

  test("malformed JSON → 400 from the body parser", async () => {
    const res = await post("{not json");
    assert.equal(res.status, 400);
  });

  for (const [name, body] of [
    ["empty object", {}],
    ["missing claim", { subject: "s" }],
    ["empty subject", { subject: "", claim: true }],
    ["non-string subject", { subject: 5, claim: true }],
    ["subject over 256 chars", { subject: "x".repeat(257), claim: true }],
  ] as const) {
    test(`${name} → 400`, async () => {
      const res = await post(JSON.stringify(body));
      assert.equal(res.status, 400);
      assert.deepEqual(await res.json(), BAD_REQUEST);
    });
  }

  test("valid request → 200 with a verifiable attestation", async () => {
    const res = await post(JSON.stringify({ subject: "did:example:buyer", claim: { delivered: true, order: 7 } }));
    assert.equal(res.status, 200);
    const att = (await res.json()) as Attestation;
    assert.equal(att.alg, "ed25519");
    assert.equal(att.pubkey_b58, attestor.pubkeyB58);
    assert.equal(att.payload.subject, "did:example:buyer");
    assert.deepEqual(att.payload.claim, { delivered: true, order: 7 });
    assert.ok(Math.abs(att.payload.ts - Date.now() / 1000) < 60, "ts is issued-at, seconds since epoch");
    // The property the product sells: verifiable from the wire format alone,
    // with no trust in the server.
    assert.equal(verifyAttestation(att), true);
  });

  test("boundary: 256-char subject and null claim are accepted", async () => {
    const res = await post(JSON.stringify({ subject: "x".repeat(256), claim: null }));
    assert.equal(res.status, 200);
    const att = (await res.json()) as Attestation;
    assert.equal(att.payload.claim, null);
    assert.equal(verifyAttestation(att), true);
  });
});
