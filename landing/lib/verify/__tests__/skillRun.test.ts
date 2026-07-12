import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { checkSkillRun } from "../skillRun";

// checkSkillRun parses the per-commit skill-run manifest
// (landing/public/witness/skill/<sha>.json) for the public verify surface. It
// fails closed: a missing manifest, a manifest without a skill name/digest, or
// unreadable JSON yields null, and each optional field is validated by type so a
// malformed manifest never renders a partial or mistyped skill-run.
describe("checkSkillRun", () => {
  let root: string;
  const dir = () => join(root, "landing", "public", "witness", "skill");
  const write = (sha: string, body: unknown) => {
    mkdirSync(dir(), { recursive: true });
    writeFileSync(join(dir(), `${sha}.json`), typeof body === "string" ? body : JSON.stringify(body));
  };
  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), "cov-skillrun-"));
  });
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  it("returns null when no manifest exists for the sha", () => {
    expect(checkSkillRun(root, "absent")).toBeNull();
  });

  it("returns null when the manifest is unreadable JSON", () => {
    write("badjson", "{not valid json");
    expect(checkSkillRun(root, "badjson")).toBeNull();
  });

  it("returns null when only the skill name is present", () => {
    write("noname", { skill: { digest: "d1" } });
    expect(checkSkillRun(root, "noname")).toBeNull();
  });

  it("returns null when only the skill digest is present", () => {
    write("nodigest", { skill: { name: "demo" } });
    expect(checkSkillRun(root, "nodigest")).toBeNull();
  });

  it("returns null when the name is present but not a string", () => {
    write("namenum", { skill: { name: 1, digest: "abc" } });
    expect(checkSkillRun(root, "namenum")).toBeNull();
  });

  it("returns null when the digest is present but not a string", () => {
    write("digestbool", { skill: { name: "demo", digest: true } });
    expect(checkSkillRun(root, "digestbool")).toBeNull();
  });

  it("parses a minimal manifest and defaults capabilities and tx", () => {
    write("minimal", { skill: { name: "demo", digest: "abc" } });
    expect(checkSkillRun(root, "minimal")).toEqual({
      skill: { name: "demo", digest: "abc" },
      capabilities: [],
      tx: null,
    });
  });

  it("keeps only string capabilities and drops non-string entries", () => {
    write("caps", { skill: { name: "demo", digest: "abc" }, capabilities: ["a", 2, "b", null] });
    expect(checkSkillRun(root, "caps")?.capabilities).toEqual(["a", "b"]);
  });

  it("treats a non-array capabilities field as empty", () => {
    write("capsobj", { skill: { name: "demo", digest: "abc" }, capabilities: { a: 1 } });
    expect(checkSkillRun(root, "capsobj")?.capabilities).toEqual([]);
  });

  it("parses a full tx and preserves the mainnet cluster and numeric slot", () => {
    write("txfull", {
      skill: { name: "demo", digest: "abc" },
      tx: { sig: "SIG", cluster: "mainnet", slot: 42 },
    });
    expect(checkSkillRun(root, "txfull")?.tx).toEqual({ sig: "SIG", cluster: "mainnet", slot: 42 });
  });

  it("defaults an unknown cluster to devnet and a non-numeric slot to null", () => {
    write("txdefault", {
      skill: { name: "demo", digest: "abc" },
      tx: { sig: "SIG", cluster: "testnet", slot: "soon" },
    });
    expect(checkSkillRun(root, "txdefault")?.tx).toEqual({ sig: "SIG", cluster: "devnet", slot: null });
  });

  it("yields a null tx when the tx sig is missing or empty", () => {
    write("txnosig", { skill: { name: "demo", digest: "abc" }, tx: { cluster: "mainnet" } });
    expect(checkSkillRun(root, "txnosig")?.tx).toBeNull();
    write("txempty", { skill: { name: "demo", digest: "abc" }, tx: { sig: "" } });
    expect(checkSkillRun(root, "txempty")?.tx).toBeNull();
  });
});
