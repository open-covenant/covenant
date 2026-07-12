import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

export type SkillRunTx = { sig: string; cluster: "devnet" | "mainnet"; slot: number | null };
export type SkillRun = {
  skill: { name: string; digest: string };
  capabilities: string[];
  tx: SkillRunTx | null;
};

// A skill-driven run anchored to this commit, sourced from
// landing/public/witness/skill/<sha>.json. Null for an ordinary code commit, a
// manifest missing the required skill name/digest, or unreadable JSON. The
// shape is validated field-by-field so a malformed manifest never renders a
// partial or mistyped skill-run on the public witness surface.
export function checkSkillRun(repoRoot: string, sha: string): SkillRun | null {
  const manifest = join(repoRoot, "landing", "public", "witness", "skill", `${sha}.json`);
  if (!existsSync(manifest)) return null;
  try {
    const raw = JSON.parse(readFileSync(manifest, "utf8")) as Record<string, unknown>;
    const skill = (raw.skill ?? {}) as Record<string, unknown>;
    const name = typeof skill.name === "string" ? skill.name : "";
    const digest = typeof skill.digest === "string" ? skill.digest : "";
    if (!name || !digest) return null;
    const capabilities = Array.isArray(raw.capabilities)
      ? raw.capabilities.filter((c): c is string => typeof c === "string")
      : [];
    const txRaw = (raw.tx ?? null) as Record<string, unknown> | null;
    const tx: SkillRunTx | null =
      txRaw && typeof txRaw.sig === "string" && txRaw.sig
        ? {
            sig: txRaw.sig,
            cluster: txRaw.cluster === "mainnet" ? "mainnet" : "devnet",
            slot: typeof txRaw.slot === "number" ? txRaw.slot : null,
          }
        : null;
    return { skill: { name, digest }, capabilities, tx };
  } catch {
    return null;
  }
}
