import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

export type NightlyRecord = {
  date: string;
  scope: "in_diff" | "weekly_full";
  total_mutations: number;
  caught: number;
  missed: number;
  score: number;
};

export function readNightlyRecords(
  dir = join(process.cwd(), "public", "mutants"),
): NightlyRecord[] {
  let names: string[] = [];
  try {
    names = readdirSync(dir);
  } catch {
    return [];
  }
  const out: NightlyRecord[] = [];
  for (const name of names) {
    if (!/^\d{4}-\d{2}-\d{2}\.json$/.test(name)) continue;
    try {
      const r = JSON.parse(readFileSync(join(dir, name), "utf8")) as NightlyRecord;
      out.push(r);
    } catch {
      // skip malformed entries
    }
  }
  return out.sort((a, b) => (a.date < b.date ? 1 : -1));
}
