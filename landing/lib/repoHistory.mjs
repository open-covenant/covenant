// The commit total and the loop's latest commit, for the header stats. A deploy
// checkout is shallow (Render clones depth=1), so when the local history is
// incomplete both come from a blobless clone of the public repository instead.
// Never throws: unknown values are null, with git's own reason when the clone
// failed.

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const REPO_URL = "https://github.com/open-covenant/covenant.git";
// The autonomous loop authors as "Covenant"; the header links its latest commit.
export const LOOP_AUTHOR = "covenant@users.noreply.github.com";
// The clone normally takes a second or two; a stalled network should not hold
// up a deploy for longer than this.
const TIMEOUT = 30_000;

const git = (args) =>
  execFileSync("git", args, {
    encoding: "utf8",
    timeout: TIMEOUT,
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, GIT_TERMINAL_PROMPT: "0" },
  }).trim();

function read(base, ref) {
  const commits = Number(git([...base, "rev-list", "--count", ref])) || null;
  const sha = git([...base, "log", `--author=${LOOP_AUTHOR}`, "-1", "--format=%H", ref]);
  return { commits, head: sha ? sha.slice(0, 7) : null };
}

export function readHistory({ repoRoot = null, url = REPO_URL } = {}) {
  if (repoRoot) {
    try {
      if (git(["-C", repoRoot, "rev-parse", "--is-shallow-repository"]) === "false") {
        return read(["-C", repoRoot], "HEAD");
      }
    } catch {}
  }

  let dir = null;
  try {
    dir = mkdtempSync(join(tmpdir(), "covenant-history-"));
    git(["clone", "--quiet", "--bare", "--filter=blob:none", "--single-branch", "--branch", "main", url, dir]);
    return read(["--git-dir", dir], "main");
  } catch (error) {
    const lines = String(error?.stderr || error?.message || error).trim().split("\n");
    const reason = lines.find((line) => line.startsWith("fatal:")) ?? lines[0];
    return { commits: null, head: null, reason };
  } finally {
    if (dir) rmSync(dir, { recursive: true, force: true });
  }
}
