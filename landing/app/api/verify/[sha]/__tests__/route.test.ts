import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { GET } from "../route";

// GET is the public witness verify orchestrator (app/api/verify/[sha]): it
// validates the sha, resolves commit metadata through the real git boundary,
// decides whether a commit predates the witness loop, and composes the four
// anchor witnesses, skill-run, and commit header into the API response. The
// anchor checks and git/cutover primitives are unit-tested in lib/verify; these
// tests pin the route-level decisions those units do not see — the sha gate, the
// predates-witness-loop branch, the missing-git-history attestation fallback, and
// the cutover-ancestry consultation — driven against throwaway repos so the wiring
// is proven without depending on this checkout's history.

const COVENANT_EMAIL = "covenant@users.noreply.github.com";
const STRANGER_EMAIL = "stranger@example.test";
const repos: string[] = [];

function makeRepo() {
  const root = mkdtempSync(join(tmpdir(), "cov-verifyget-"));
  repos.push(root);
  const env = { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };
  const sh = (...args: string[]) =>
    execFileSync("git", ["-C", root, "-c", "core.hooksPath=/dev/null", ...args], {
      encoding: "utf8",
      env,
    }).trim();
  sh("init", "-q", "-b", "main");
  sh("config", "user.name", "covtester");
  const commit = (msg: string, email: string) => {
    sh("config", "user.email", email);
    writeFileSync(join(root, "f.txt"), `${msg}\n`);
    sh("add", "-A");
    sh("commit", "-q", "-m", msg);
    return sh("rev-parse", "HEAD");
  };
  return { root, commit };
}

function call(root: string, sha: string) {
  vi.spyOn(process, "cwd").mockReturnValue(root);
  return GET(new Request(`http://x/api/verify/${sha}`), { params: Promise.resolve({ sha }) });
}

afterEach(() => {
  vi.restoreAllMocks();
  while (repos.length) rmSync(repos.pop()!, { recursive: true, force: true });
});

describe("verify route GET orchestrator", () => {
  it("rejects a malformed sha with 400 before touching git", async () => {
    const res = await GET(new Request("http://x/api/verify/zzz"), {
      params: Promise.resolve({ sha: "zzz" }),
    });
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "invalid sha" });
  });

  it("checks anchors for a Covenant-authored commit (predatesWitnessLoop=false)", async () => {
    const { root, commit } = makeRepo();
    const head = commit("witnessed", COVENANT_EMAIL);

    const res = await call(root, head);
    expect(res.status).toBe(200);
    const body = await res.json();

    expect(body.commit.sha).toBe(head);
    expect(body.commit.predatesWitnessLoop).toBe(false);
    expect(body.witnesses).toHaveLength(4);
    // The full branch runs the real anchor checks; none of them carry the
    // predates sentinel, so a flipped email comparison routing this commit to
    // the historical branch would replace these details and fail here.
    expect(body.witnesses.every((w: { detail: string }) => w.detail !== "Predates witness loop.")).toBe(
      true,
    );
    expect(body.skillRun === null || typeof body.skillRun === "object").toBe(true);
    expect(body.fifth.href).toBe("/lineage/mutation-quality");
  });

  it("renders a non-Covenant commit as historical-gray (predatesWitnessLoop=true)", async () => {
    const { root, commit } = makeRepo();
    const head = commit("legacy", STRANGER_EMAIL);

    const res = await call(root, head);
    expect(res.status).toBe(200);
    const body = await res.json();

    expect(body.commit.predatesWitnessLoop).toBe(true);
    expect(body.witnesses).toHaveLength(4);
    expect(
      body.witnesses.every(
        (w: { state: string; detail: string }) =>
          w.state === "gray" && w.detail === "Predates witness loop.",
      ),
    ).toBe(true);
    expect(body.skillRun).toBeNull();
  });

  it("404s a well-formed sha with no git history and no attestation", async () => {
    const { root, commit } = makeRepo();
    commit("witnessed", COVENANT_EMAIL);
    const ghost = "0123456789abcdef0123456789abcdef01234567";

    const res = await call(root, ghost);
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ error: "unknown sha" });
  });

  it("renders a synthetic header for a sha absent from git but carrying an attestation", async () => {
    const { root, commit } = makeRepo();
    commit("witnessed", COVENANT_EMAIL);
    const ghost = "0123456789abcdef0123456789abcdef01234567";
    mkdirSync(join(root, "attestations"), { recursive: true });
    writeFileSync(join(root, "attestations", `${ghost}.json`), "{}");

    const res = await call(root, ghost);
    expect(res.status).toBe(200);
    const body = await res.json();

    expect(body.commit.authorDisplay).toBe("Covenant");
    expect(body.commit.subject).toBe("Witnessed devnet run");
    expect(body.commit.shortSha).toBe(ghost.slice(0, 12));
    expect(body.commit.predatesWitnessLoop).toBe(false);
    expect(body.witnesses).toHaveLength(4);
  });
});

describe("verify route GET cutover gate", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  // With a cutover sha configured, a non-Covenant commit's predates state is
  // decided by ancestry against the cutover, not by author email. WITNESS_CUTOVER_SHA
  // is read at module load, so the route is re-imported under a stubbed env.
  it("consults cutover ancestry for non-Covenant commits when a cutover sha is set", async () => {
    const { root, commit } = makeRepo();
    const before = commit("before", STRANGER_EMAIL);
    const cutover = commit("cutover", STRANGER_EMAIL);
    const after = commit("after", STRANGER_EMAIL);

    vi.resetModules();
    vi.stubEnv("WITNESS_CUTOVER_SHA", cutover);
    const { GET: gatedGet } = await import("../route");

    vi.spyOn(process, "cwd").mockReturnValue(root);
    const ancestor = await gatedGet(new Request(`http://x/api/verify/${before}`), {
      params: Promise.resolve({ sha: before }),
    });
    const descendant = await gatedGet(new Request(`http://x/api/verify/${after}`), {
      params: Promise.resolve({ sha: after }),
    });

    // `before` is an ancestor of the cutover → predates; `after` is not → checked.
    // Dropping the `&& WITNESS_CUTOVER_SHA ? predatesCutover(...)` arm collapses
    // both to the email fallback (true for a stranger), failing the `after` case.
    expect((await ancestor.json()).commit.predatesWitnessLoop).toBe(true);
    expect((await descendant.json()).commit.predatesWitnessLoop).toBe(false);
  });
});
