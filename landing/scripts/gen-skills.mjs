// Writes the committed catalog snapshot the /skills page renders
// (landing/public/skills.json). It mirrors the daemon's
// `covenant skill list --json` shape by walking the in-repo Agent Skills
// under covenant-skill/ and computing the same content digest that
// covenant-skills' parser.rs pins at install — so the digest on the page is
// the real one a daemon would verify, not a placeholder.
//
// Re-run after editing a skill:  node scripts/gen-skills.mjs

import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { findRepoRoot } from "../lib/agentStream.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = findRepoRoot(resolve(here, "..", ".."));
const outFile = resolve(here, "..", "public", "skills.json");

// Digest contract, mirrored from covenant-skills/src/parser.rs: sha256 over,
// for each tracked file (SKILL.md + references/** recursive) sorted by relpath,
// "{relpath}\t{sha256(normalize(content))}\n". normalize: CRLF->LF, trim
// trailing whitespace per line, trim trailing newlines.
const sha256Hex = (data) => createHash("sha256").update(data).digest("hex");

// Rust's str::trim_end() trims the Unicode White_Space set, which is NOT the
// same as JS /\s/ (NEL U+0085 is White_Space but not /\s/; BOM U+FEFF is /\s/
// but not White_Space). Match Rust's set exactly, minus \n (handled by split),
// so an author-introduced trailing space character can't make this digest
// diverge from the one the daemon pins. Built from an ASCII source string so the
// class holds no invisible literals.
const TRAILING_WS = new RegExp(
  "[\\t\\u000B\\f\\r \\u0085\\u00A0\\u1680\\u2000-\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000]+$",
  "u",
);

function normalize(buf) {
  const text = buf.toString("utf8").replace(/\r\n/g, "\n");
  let out = text
    .split("\n")
    .map((line) => line.replace(TRAILING_WS, ""))
    .join("\n");
  while (out.endsWith("\n")) out = out.slice(0, -1);
  return Buffer.from(out, "utf8");
}

function referencePaths(skillDir) {
  const out = [];
  const walk = (dir) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const abs = join(dir, entry.name);
      if (entry.isDirectory()) walk(abs);
      else if (entry.isFile()) out.push(relative(skillDir, abs).split(sep).join("/"));
    }
  };
  walk(join(skillDir, "references"));
  return out.sort();
}

function contentDigest(skillDir, references) {
  const entries = [["SKILL.md", readFileSync(join(skillDir, "SKILL.md"))]];
  for (const rel of references) entries.push([rel, readFileSync(join(skillDir, rel.split("/").join(sep)))]);
  entries.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  const composite = createHash("sha256");
  for (const [rel, bytes] of entries) composite.update(`${rel}\t${sha256Hex(normalize(bytes))}\n`);
  return `sha256:${composite.digest("hex")}`;
}

// Minimal reader for the controlled SKILL.md frontmatter (flat scalars plus a
// one-level `metadata:` map). Matches parser.rs delimiter handling: the first
// line and the close must each equal `---` after trimming. Quotes are stripped;
// values stay single-line.
function frontmatter(md) {
  const lines = md.replace(/\r\n/g, "\n").split("\n");
  if (lines[0]?.trimEnd() !== "---") return {};
  const close = lines.findIndex((line, i) => i > 0 && line.trimEnd() === "---");
  if (close === -1) return {};
  const out = { metadata: {} };
  let inMetadata = false;
  for (const line of lines.slice(1, close)) {
    if (!line.trim() || line.trimStart().startsWith("#")) continue;
    const indented = /^\s+/.test(line);
    const m = line.match(/^\s*([A-Za-z0-9_-]+):\s?(.*)$/);
    if (!m) continue;
    const [, key, raw] = m;
    const value = raw.trim().replace(/^["']|["']$/g, "");
    if (!indented) {
      inMetadata = key === "metadata" && value === "";
      if (!inMetadata) out[key] = value;
    } else if (inMetadata) {
      out.metadata[key] = value;
    }
  }
  return out;
}

function manifest(skillDir) {
  const fm = frontmatter(readFileSync(join(skillDir, "SKILL.md"), "utf8"));
  if (!fm.name) return null;
  const references = referencePaths(skillDir);
  return {
    name: fm.name,
    version: fm.metadata?.version ?? "",
    description: fm.description ?? "",
    digest: contentDigest(skillDir, references),
    // Source coordinates are pinned at release (tag + resolved commit). This
    // skill is unreleased, so they stay empty rather than pin an ephemeral
    // branch commit whose tree URL would 404 once the branch is squash-merged.
    // The content digest above is the durable, location-independent anchor.
    source: { url: "", tag: "", commit: "" },
    references,
    declared_capabilities: [],
    declared_programs: [],
    sends_tx: false,
  };
}

function findSkillDirs(root) {
  const found = [];
  const walk = (dir) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    if (entries.some((e) => e.isFile() && e.name === "SKILL.md")) found.push(dir);
    for (const entry of entries) {
      if (entry.isDirectory() && entry.name !== "node_modules" && entry.name !== "references") {
        walk(join(dir, entry.name));
      }
    }
  };
  walk(root);
  return found.sort();
}

function readExisting() {
  try {
    return JSON.parse(readFileSync(outFile, "utf8"));
  } catch {
    return { kind: "skill_list", skills: [] };
  }
}

const existing = readExisting();
let skills = [];
try {
  if (repoRoot && existsSync(join(repoRoot, "covenant-skill"))) {
    skills = findSkillDirs(join(repoRoot, "covenant-skill"))
      .map(manifest)
      .filter(Boolean)
      .sort((a, b) => a.name.localeCompare(b.name));
  }
} catch (err) {
  console.warn(`gen-skills: walk failed (${err.message}); keeping committed snapshot`);
  process.exit(0);
}

// Never clobber a populated committed snapshot with an empty one — a shallow or
// skill-less build runtime would otherwise blank the catalog. Don't fail either way.
if (!skills.length && existing.skills?.length) {
  console.warn("gen-skills: no skills found on disk; keeping committed snapshot");
  process.exit(0);
}

writeFileSync(outFile, `${JSON.stringify({ kind: "skill_list", skills }, null, 2)}\n`);
console.log(`gen-skills: wrote ${skills.length} skill(s) to ${relative(process.cwd(), outFile)}`);
