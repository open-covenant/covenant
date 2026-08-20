import { afterEach, describe, expect, it } from "vitest";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readNightlyRecords, type NightlyRecord } from "../_records";

const dirs: string[] = [];

function fixture(files: Record<string, string>): string {
  const dir = mkdtempSync(join(tmpdir(), "mutants-"));
  dirs.push(dir);
  for (const [name, content] of Object.entries(files)) {
    writeFileSync(join(dir, name), content);
  }
  return dir;
}

function record(date: string, over: Partial<NightlyRecord> = {}): string {
  const r: NightlyRecord = {
    date,
    scope: "in_diff",
    total_mutations: 10,
    caught: 9,
    missed: 1,
    score: 0.9,
    ...over,
  };
  return JSON.stringify(r);
}

afterEach(() => {
  while (dirs.length) rmSync(dirs.pop()!, { recursive: true, force: true });
});

describe("readNightlyRecords", () => {
  it("returns an empty trend when the directory is missing", () => {
    const dir = join(fixture({}), "absent");
    expect(readNightlyRecords(dir)).toEqual([]);
  });

  it("admits only YYYY-MM-DD .json files", () => {
    const dir = fixture({
      "2026-06-27.json": record("2026-06-27"),
      "index.json": record("9999-99-99"),
      "2026-1-1.json": record("2026-1-1"),
      "2026-06-28.json.bak": record("2026-06-28"),
    });
    expect(readNightlyRecords(dir).map((r) => r.date)).toEqual(["2026-06-27"]);
  });

  it("skips a malformed entry without dropping its valid neighbours", () => {
    const dir = fixture({
      "2026-06-25.json": record("2026-06-25"),
      "2026-06-26.json": "{ not valid json",
      "2026-06-27.json": record("2026-06-27"),
    });
    expect(readNightlyRecords(dir).map((r) => r.date)).toEqual([
      "2026-06-27",
      "2026-06-25",
    ]);
  });

  it("orders records by date descending so records[0] is the latest", () => {
    const dir = fixture({
      "2026-06-27.json": record("2026-06-27"),
      "2026-06-29.json": record("2026-06-29"),
      "2026-06-25.json": record("2026-06-25"),
    });
    expect(readNightlyRecords(dir).map((r) => r.date)).toEqual([
      "2026-06-29",
      "2026-06-27",
      "2026-06-25",
    ]);
  });
});
