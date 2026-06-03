// In-process event bus for the live agent feed. One singleton per server
// process (persisted across HMR via globalThis). Two feeders populate it:
//   - startLocalTail(): tails the real covenant loop artifacts (events.jsonl
//     transitions + new git commits) — used wherever the loop runs (dev/local).
//   - the ingest route: pushes events received from a remote local pusher —
//     used in prod where the loop is not on the same host.
// SSE subscribers fan out from here. All text is redaction-cleaned.

import { execFileSync } from "node:child_process";
import {
  closeSync,
  existsSync,
  openSync,
  readFileSync,
  readSync,
  statSync,
  watchFile,
} from "node:fs";
import { join } from "node:path";
import { clean as cleanLine, findRepoRoot } from "./agentStream.mjs";

export { findRepoRoot } from "./agentStream.mjs";

// Field redactor — blanks anything carrying an identity token (cleanLine
// derives those tokens at runtime and drops to null).
export const clean = (s) => cleanLine(s) ?? "";

const RING_MAX = 200;
const EVENTS_REL = ["agent-os", "autonomy", "events.jsonl"];

const git = (root, args) => {
  try {
    return execFileSync("git", ["-C", root, ...args], {
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    }).trim();
  } catch {
    return "";
  }
};

export function parseTransitionLine(line) {
  let e;
  try {
    e = JSON.parse(line);
  } catch {
    return null;
  }
  if (!e || !e.taskId || !e.to) return null;
  return {
    type: "transition",
    ts: e.timestamp || "",
    taskId: clean(e.taskId),
    from: e.from || "",
    to: e.to,
    actor: e.actorRole || "",
    note: clean(e.note),
  };
}

export function commitEvent(root, hash) {
  const subject = git(root, ["show", "-s", "--format=%s", hash]);
  const stat =
    git(root, ["show", "--shortstat", "--format=", hash])
      .split("\n")
      .filter(Boolean)
      .pop() || "";
  return { type: "commit", hash, subject: clean(subject), stat: clean(stat.trim()) };
}

function readSlice(file, start, end) {
  if (end <= start) return "";
  const fd = openSync(file, "r");
  const buf = Buffer.allocUnsafe(end - start);
  try {
    readSync(fd, buf, 0, end - start, start);
  } finally {
    closeSync(fd);
  }
  return buf.toString("utf8");
}

function createBus() {
  const ring = [];
  const subs = new Set();
  let started = false;

  const publish = (e) => {
    if (!e) return;
    ring.push(e);
    while (ring.length > RING_MAX) ring.shift();
    for (const cb of subs) {
      try {
        cb(e);
      } catch {}
    }
  };

  const subscribe = (cb) => {
    subs.add(cb);
    return () => subs.delete(cb);
  };

  function tailEvents(root) {
    const file = join(root, ...EVENTS_REL);
    if (!existsSync(file)) return;
    try {
      const seed = readFileSync(file, "utf8").split("\n").filter(Boolean).slice(-30);
      for (const l of seed) publish(parseTransitionLine(l));
    } catch {}
    let offset = 0;
    try {
      offset = statSync(file).size;
    } catch {}
    watchFile(file, { interval: 1000 }, (curr) => {
      try {
        if (curr.size < offset) offset = 0;
        if (curr.size <= offset) return;
        const chunk = readSlice(file, offset, curr.size);
        offset = curr.size;
        for (const l of chunk.split("\n").filter(Boolean)) publish(parseTransitionLine(l));
      } catch {}
    });
  }

  function watchCommits(root) {
    const head = join(root, ".git", "logs", "HEAD");
    if (!existsSync(head)) return;
    let last = git(root, ["rev-parse", "HEAD"]);
    watchFile(head, { interval: 1000 }, () => {
      const cur = git(root, ["rev-parse", "HEAD"]);
      if (!cur || cur === last) return;
      const range = last ? `${last}..${cur}` : "HEAD";
      const out = git(root, ["log", range, "--no-merges", "--reverse", "--format=%h"]);
      last = cur;
      for (const h of out.split("\n").filter(Boolean)) publish(commitEvent(root, h));
    });
  }

  function startLocalTail() {
    if (started) return;
    started = true;
    if (process.env.AGENT_LOCAL_TAIL === "false") return;
    const root = findRepoRoot(process.cwd());
    if (!root) return;
    tailEvents(root);
    watchCommits(root);
  }

  return { ring, subscribe, publish, startLocalTail };
}

export const bus = (globalThis.__covAgentBus ??= createBus());
