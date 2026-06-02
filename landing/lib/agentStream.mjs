// Shared covenant git → terminal-stream generator. Used by the build-time
// snapshot script (scripts/gen-agent-stream.mjs) and the live route
// (app/agent-stream/route.ts). Real history only — subjects, short hashes,
// capped diff hunks. No author/email/date is emitted; home paths are
// redacted and any banned token drops the line, so output stays clean.

import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import os from "node:os";
import { dirname, resolve } from "node:path";

const DEFAULTS = {
  maxCommits: 26,
  maxFilesPerCommit: 6,
  maxBodyLines: 46,
  maxTotalLines: 1500,
};

const SKIP_DIFF =
  /(^|\/)(pnpm-lock\.yaml|package-lock\.json|Cargo\.lock|.*\.(png|jpe?g|gif|webp|ico|svg|woff2?|pdf|mp4))$/i;
// Identity tokens are derived at runtime — never hardcoded — so no real-name
// string is ever stored in source or shipped in the snapshot.
function leakTokens() {
  const out = new Set();
  const add = (v) => {
    const s = (v ?? "").toString().trim();
    if (
      s.length >= 4 &&
      !["root", "node", "user", "home", "admin", "guest", "local"].includes(s.toLowerCase())
    ) {
      out.add(s);
    }
  };
  add(process.env.USER);
  add(process.env.HOME);
  try {
    add(os.userInfo().username);
  } catch {}
  try {
    const h = os.hostname();
    if (h && h !== "localhost") {
      add(h);
      add(h.replace(/\.local$/i, ""));
    }
  } catch {}
  for (const key of ["user.name", "user.email"]) {
    try {
      add(execFileSync("git", ["config", "--global", "--get", key], { encoding: "utf8" }));
    } catch {}
  }
  return [...out];
}

const LEAK_TOKENS = leakTokens();
const HOME_PATH = /\/Users\/[A-Za-z0-9._-]+/g;

export const clean = (s) => {
  if (s == null) return null;
  const t = String(s).replace(/\r$/, "").replace(HOME_PATH, "~");
  for (const tok of LEAK_TOKENS) {
    if (t.includes(tok)) return null;
  }
  return t;
};

export function findRepoRoot(startDir) {
  let dir = resolve(startDir || ".");
  for (let i = 0; i < 8; i += 1) {
    if (existsSync(resolve(dir, ".git"))) return dir;
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return null;
}

const git = (repoRoot, ...args) =>
  execFileSync("git", ["-C", repoRoot, ...args], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });

function parseShow(repoRoot, hash) {
  const lines = git(repoRoot, "show", hash, "--patch", "--format=%s", "--unified=2").split("\n");
  const firstDiff = lines.findIndex((l) => l.startsWith("diff --git "));
  const subject =
    (firstDiff === -1 ? lines : lines.slice(0, firstDiff)).find((l) => l.trim().length)?.trim() ??
    "(no subject)";
  if (firstDiff === -1) return { subject, files: [] };

  const files = [];
  let cur = null;
  for (const line of lines.slice(firstDiff)) {
    if (line.startsWith("diff --git ")) {
      if (cur) files.push(cur);
      const m = line.match(/ b\/(.+)$/);
      cur = { path: m ? m[1] : "?", status: "modified", binary: false, add: 0, del: 0, body: [] };
      continue;
    }
    if (!cur) continue;
    if (line.startsWith("new file mode")) cur.status = "added";
    else if (line.startsWith("deleted file mode")) cur.status = "deleted";
    else if (line.startsWith("rename ")) cur.status = "renamed";
    else if (line.startsWith("Binary files")) cur.binary = true;
    else if (line.startsWith("@@")) cur.body.push({ k: "hunk", t: line.replace(/@@(.*?)@@.*/, "@@$1@@") });
    else if (
      line.startsWith("+++") ||
      line.startsWith("---") ||
      line.startsWith("index ") ||
      line.startsWith("similarity ") ||
      line.startsWith("old mode") ||
      line.startsWith("new mode")
    )
      continue;
    else if (line.startsWith("+")) {
      cur.add += 1;
      cur.body.push({ k: "add", t: line });
    } else if (line.startsWith("-")) {
      cur.del += 1;
      cur.body.push({ k: "del", t: line });
    } else if (line.startsWith(" ")) cur.body.push({ k: "ctx", t: line });
  }
  if (cur) files.push(cur);
  return { subject, files };
}

export function generateStream(opts = {}) {
  const o = { ...DEFAULTS, ...opts };
  const repoRoot = o.repoRoot || findRepoRoot(process.cwd());
  if (!repoRoot) return { commits: 0, lines: [] };

  const hashes = git(repoRoot, "log", "--no-merges", "--format=%h", "-n", String(o.maxCommits))
    .split("\n")
    .filter(Boolean)
    .reverse();

  const lines = [];
  const push = (k, t) => {
    if (t === undefined) {
      lines.push({ k, t: "" });
      return;
    }
    const c = clean(t);
    if (c !== null) lines.push({ k, t: c });
  };

  let stop = false;
  for (const hash of hashes) {
    if (stop) break;
    const { subject, files } = parseShow(repoRoot, hash);
    if (!files.length) continue;

    push("cmd", `git show ${hash}  # ${subject}`);
    push("blank");

    for (const f of files.slice(0, o.maxFilesPerCommit)) {
      const sign = `+${f.add}/-${f.del}`;
      push("write", `Writing ${f.path} from ${hash} (${f.status}, ${sign})`);
      push("blank");
      push("diff", `diff --git a/${f.path} b/${f.path}`);

      if (f.binary || SKIP_DIFF.test(f.path)) {
        push("meta", `# ${f.status} ${sign} (diff omitted)`);
      } else {
        push("meta", `# ${f.status} +${f.add} -${f.del}`);
        for (const b of f.body.slice(0, o.maxBodyLines)) push(b.k, b.t);
        if (f.body.length > o.maxBodyLines) push("ctx", " … (truncated)");
      }
      push("blank");
      if (lines.length >= o.maxTotalLines) {
        stop = true;
        break;
      }
    }

    push("commit", `commited ${hash}`);
    push("blank");
  }

  return { commits: hashes.length, lines };
}
