import { generateKeyPairSync, sign as edSign } from "node:crypto";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { checkAnchor4VerifierSig } from "../verifierSig";

const DOMAIN = "covenant.witness-verdict.v2";
const SCHEMA = "covenant.witness-verdict.v2";
const ROOT_HEX =
  "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0";
const STEP = '{"id":"event-1"}';

function canonical(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.entries(value as Record<string, unknown>)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, item]) => `${JSON.stringify(key)}:${canonical(item)}`)
    .join(",")}}`;
}

describe("checkAnchor4VerifierSig", () => {
  const roots: string[] = [];
  const mkRoot = () => {
    const root = mkdtempSync(join(tmpdir(), "cov-vsig-"));
    roots.push(root);
    return root;
  };
  afterAll(() =>
    roots.forEach((root) => rmSync(root, { recursive: true, force: true })),
  );

  const keypair = () => {
    const { publicKey, privateKey } = generateKeyPairSync("ed25519");
    const x = (publicKey.export({ format: "jwk" }) as { x: string }).x;
    return { x, privateKey };
  };
  const statement = (
    pubkey: string,
    overrides: Record<string, unknown> = {},
  ) => ({
    schema: SCHEMA,
    domain: DOMAIN,
    audit_root_hex: ROOT_HEX,
    event_count: 1,
    verdict: "pass",
    refutations: [],
    verifier_pubkey: pubkey,
    ...overrides,
  });
  const sign = (
    privateKey: ReturnType<typeof keypair>["privateKey"],
    value: Record<string, unknown>,
  ) =>
    edSign(
      null,
      Buffer.from(`${DOMAIN}\n${canonical(value)}`, "utf8"),
      privateKey,
    ).toString("base64url");
  const writeFixture = (
    root: string,
    sha: string,
    x: string,
    privateKey: ReturnType<typeof keypair>["privateKey"],
    value = statement(x),
    outer: Record<string, unknown> = {},
  ) => {
    const attDir = join(root, "attestations");
    const witnessDir = join(root, "landing", "public", "witness");
    mkdirSync(attDir, { recursive: true });
    mkdirSync(witnessDir, { recursive: true });
    writeFileSync(
      join(attDir, `${sha}.json`),
      JSON.stringify({
        audit_root_hex: ROOT_HEX,
        event_count: 1,
        steps: [STEP],
        verifier_statement: value,
        ...outer,
      }),
    );
    writeFileSync(join(attDir, `${sha}.verifier.sig`), sign(privateKey, value));
    writeFileSync(join(witnessDir, "verifier-pubkey.txt"), x);
  };

  it("stays yellow when no verifier signature is published", () => {
    const result = checkAnchor4VerifierSig(mkRoot(), "absent");
    expect(result.state).toBe("yellow");
    expect(result.detail).toContain("No v2 verifier statement");
  });

  it("reports a valid pass as self-published attribution, never green", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    writeFixture(root, "pass", x, privateKey);
    const result = checkAnchor4VerifierSig(root, "pass");
    expect(result).toMatchObject({
      label: "Self-published verifier statement",
      state: "yellow",
    });
    expect(result.detail).toContain("not an externally pinned trust root");
  });

  it("reports a signed refutation as red", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    const value = statement(x, {
      verdict: "refute",
      refutations: [{ signed_event: "signed-1", after_untrusted: "input-1" }],
    });
    writeFixture(root, "refute", x, privateKey, value);
    const result = checkAnchor4VerifierSig(root, "refute");
    expect(result.state).toBe("red");
    expect(result.detail).toContain("1 configured event-order refutation");
  });

  it("rejects a verdict mutation even when the root is unchanged", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    const signed = statement(x, {
      verdict: "refute",
      refutations: [{ signed_event: "signed-1", after_untrusted: "input-1" }],
    });
    writeFixture(root, "mutated", x, privateKey, signed);
    const path = join(root, "attestations", "mutated.json");
    const artifact = JSON.parse(readFileSync(path, "utf8"));
    artifact.verifier_statement.verdict = "pass";
    artifact.verifier_statement.refutations = [];
    writeFileSync(path, JSON.stringify(artifact));
    expect(checkAnchor4VerifierSig(root, "mutated").state).toBe("red");
  });

  it("rejects missing or unknown verdict values", () => {
    for (const [sha, verdict] of [
      ["missing", undefined],
      ["unknown", "attest"],
    ] as const) {
      const root = mkRoot();
      const { x, privateKey } = keypair();
      const value = statement(x);
      if (verdict === undefined)
        delete (value as { verdict?: unknown }).verdict;
      else (value as { verdict?: unknown }).verdict = verdict;
      writeFixture(root, sha, x, privateKey, value);
      expect(checkAnchor4VerifierSig(root, sha).state).toBe("red");
    }
  });

  it("rejects outer root, count, step, or key mismatches", () => {
    for (const [sha, outer] of [
      ["root", { audit_root_hex: "0".repeat(64) }],
      ["count", { event_count: 2 }],
      ["steps", { steps: [] }],
    ] as const) {
      const root = mkRoot();
      const { x, privateKey } = keypair();
      writeFixture(root, sha, x, privateKey, statement(x), outer);
      expect(checkAnchor4VerifierSig(root, sha).state).toBe("red");
    }

    const root = mkRoot();
    const signer = keypair();
    const published = keypair();
    writeFixture(
      root,
      "key",
      published.x,
      signer.privateKey,
      statement(signer.x),
    );
    expect(checkAnchor4VerifierSig(root, "key").state).toBe("red");
  });

  it("rejects legacy root-only signatures explicitly", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    const attDir = join(root, "attestations");
    const witnessDir = join(root, "landing", "public", "witness");
    mkdirSync(attDir, { recursive: true });
    mkdirSync(witnessDir, { recursive: true });
    writeFileSync(
      join(attDir, "legacy.json"),
      JSON.stringify({
        audit_root_hex: ROOT_HEX,
        verdict: "pass",
        steps: [STEP],
      }),
    );
    writeFileSync(
      join(attDir, "legacy.verifier.sig"),
      edSign(
        null,
        Buffer.from(`covenant.witness.v1\n${ROOT_HEX}`),
        privateKey,
      ).toString("base64url"),
    );
    writeFileSync(join(witnessDir, "verifier-pubkey.txt"), x);
    const result = checkAnchor4VerifierSig(root, "legacy");
    expect(result.state).toBe("red");
    expect(result.detail).toContain("legacy or malformed");
  });
});
