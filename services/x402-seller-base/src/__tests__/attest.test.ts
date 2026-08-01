// Tests for the attestation core (attest.ts) and the paid /x402/attest handler
// (attest-route.ts). The handler is mounted on a bare express app with only the
// JSON body parser — exactly what it sees in server.ts after @x402/express has
// verified payment — so these tests cover the resource validation boundary (a
// rejected payload returns >= 400) without facilitator or payment machinery. The
// handler test makes no assertion about settlement.
//
// Note: the signature scheme is ed25519, which has no public-key recovery
// (ecrecover is a secp256k1/EVM concept). The signer-binding property tested here
// is the ed25519 equivalent: the attestation carries pubkey_b58, it must equal
// an expected key pinned outside the response, and verification must fail under
// any other key.
import { after, before, describe, test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import type { Server } from "node:http";
import type { AddressInfo } from "node:net";
import express from "express";
import bs58 from "bs58";
import {
  ATTEST_DOMAIN,
  Attestor,
  verifyAttestation,
  type Attestation,
} from "../attest.js";
import { makeAttestHandler } from "../attest-route.js";
import { mayUseEphemeralAttestor } from "../attestor-policy.js";

describe("attestation round-trip", () => {
  const attestor = Attestor.generate();

  test("sign then verify succeeds and binds the signer's pubkey", () => {
    const att = attestor.attest("did:example:agent-1", { delivered: true }, 1_700_000_000);
    assert.equal(att.alg, "ed25519");
    assert.equal(att.domain, ATTEST_DOMAIN);
    assert.equal(att.pubkey_b58, attestor.pubkeyB58);
    assert.deepEqual(att.payload, {
      subject: "did:example:agent-1",
      claim: { delivered: true },
      ts: 1_700_000_000,
    });
    assert.equal(verifyAttestation(att, attestor.pubkeyB58), true);
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
    const tampered: Attestation = {
      ...att,
      payload: { ...att.payload, claim: { paid: false } },
    };
    assert.equal(verifyAttestation(tampered, attestor.pubkeyB58), false);
  });

  test("tampered digest fails verification", () => {
    const att = attestor.attest("subject", "claim", 1);
    assert.equal(
      verifyAttestation(
        { ...att, digest_sha256_hex: att.digest_sha256_hex.replace(/^./, "f") },
        attestor.pubkeyB58,
      ),
      false,
    );
  });

  test("attestation does not verify under a different signer's pubkey", () => {
    const att = attestor.attest("subject", "claim", 1);
    assert.equal(
      verifyAttestation(
        { ...att, pubkey_b58: Attestor.generate().pubkeyB58 },
        attestor.pubkeyB58,
      ),
      false,
    );
  });

  test("signature forged by another key fails against the expected pubkey", () => {
    const att = attestor.attest("subject", "claim", 1);
    const forged = Attestor.generate().attest("subject", "claim", 1);
    assert.equal(
      verifyAttestation(
        { ...att, signature_b58: forged.signature_b58 },
        attestor.pubkeyB58,
      ),
      false,
    );
  });

  test("self-signed data from another key fails the pinned-key check", () => {
    const forged = Attestor.generate().attest("subject", "claim", 1);
    assert.equal(verifyAttestation(forged, attestor.pubkeyB58), false);
  });
});

describe("Attestor key material", () => {
  test("accepts a 32-byte seed and a matching 64-byte seed+pubkey", () => {
    const seed = Array.from({ length: 32 }, (_, index) => index);
    const fromSeed = new Attestor(seed);
    const publicKey = [...Buffer.from(bs58.decode(fromSeed.pubkeyB58))];
    const fromKeypair = new Attestor([...seed, ...publicKey]);
    assert.equal(fromKeypair.pubkeyB58, fromSeed.pubkeyB58);
  });

  for (const [name, key] of [
    ["short", Array(31).fill(1)],
    ["long", Array(65).fill(1)],
    ["negative byte", [...Array(31).fill(1), -1]],
    ["oversized byte", [...Array(31).fill(1), 256]],
    ["fractional byte", [...Array(31).fill(1), 1.5]],
  ] as const) {
    test(`rejects ${name} key material`, () => {
      assert.throws(
        () => new Attestor(key),
        /32-byte seed or 64-byte seed\+pubkey/,
      );
    });
  }

  test("rejects a 64-byte keypair whose public half does not match the seed", () => {
    assert.throws(
      () => new Attestor([...Array(32).fill(7), ...Array(32).fill(9)]),
      /public half does not match/,
    );
  });
});

describe("ephemeral attestor policy", () => {
  test("requires explicit testnet development opt-in", () => {
    assert.equal(mayUseEphemeralAttestor("base-sepolia", "development", "true"), true);
    assert.equal(mayUseEphemeralAttestor("base-sepolia", "development", undefined), false);
  });

  test("cannot be overridden on mainnet or in production", () => {
    assert.equal(mayUseEphemeralAttestor("base", "development", "true"), false);
    assert.equal(mayUseEphemeralAttestor("EIP155:8453", "development", "true"), false);
    assert.equal(mayUseEphemeralAttestor("base-sepolia", "production", "true"), false);
    assert.equal(mayUseEphemeralAttestor("base-sepolia", " Production ", "true"), false);
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
    assert.ok(
      Math.abs(att.payload.ts - Date.now() / 1000) < 60,
      "ts is issued-at, seconds since epoch",
    );
    // The signed bytes verify against a publisher key pinned outside the wire.
    assert.equal(verifyAttestation(att, attestor.pubkeyB58), true);
  });

  test("boundary: 256-char subject and null claim are accepted", async () => {
    const res = await post(JSON.stringify({ subject: "x".repeat(256), claim: null }));
    assert.equal(res.status, 200);
    const att = (await res.json()) as Attestation;
    assert.equal(att.payload.claim, null);
    assert.equal(verifyAttestation(att, attestor.pubkeyB58), true);
  });
});
