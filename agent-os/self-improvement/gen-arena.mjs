#!/usr/bin/env node
// Transforms the optimizer ledger into the public arena scoreboard consumed
// by opencovenant.org/arena. Sanitized: no local paths, reasons truncated.
// Run after each optimizer round; optimize-code.mjs invokes it automatically.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const ledgerPath = join(here, "kernel-archive", "ledger.json");
const outPath = resolve(here, "..", "..", "landing", "public", "arena.json");

const BASELINE_FUEL = 5867618602;
const PROPOSER_LABELS = { fable: "Claude", grok: "Grok" };

const ledger = JSON.parse(readFileSync(ledgerPath, "utf8"));

const rounds = new Map();
let order = 0;
for (const v of ledger.versions) {
  const key = v.version.split(":")[0];
  if (!rounds.has(key)) rounds.set(key, { order: order++, entries: [] });
  rounds.get(key).entries.push({
    proposer: PROPOSER_LABELS[v.proposer] ?? (v.proposer ? v.proposer[0].toUpperCase() + v.proposer.slice(1) : "Claude"),
    model: v.model ?? "claude",
    scalar: v.candidate ?? 0,
    incumbent: v.incumbent,
    gain: v.gain,
    promoted: !!v.promoted,
    commit: v.commit ?? null,
    handle: v.handle ?? null,
    reason: v.promoted ? null : String(v.reason ?? "").slice(0, 140),
  });
}

// k1-k8 = the loop's solo run (Claude as sole proposer); the tournament
// starts at k9 (shakedown) / k10 (Round 1). The vs-tally counts only
// tournament rounds; the curve keeps the whole history of the kernel.
const TOURNAMENT_START = 9;
const meta = (key) => {
  if (key.startsWith("c")) return { era: "challenge", display: `Challenge ${key.slice(1)}` };
  const n = parseInt(key.slice(1), 10);
  if (n < TOURNAMENT_START) return { era: "solo", display: `Run ${n}` };
  if (n === TOURNAMENT_START) return { era: "tournament", display: "Shakedown" };
  return { era: "tournament", display: `Round ${n - TOURNAMENT_START}` };
};

const tally = { Claude: 0, Grok: 0, rejectedRounds: 0 };
const solo = { promotions: 0, rejected: 0, finalScalar: 1 };
const community = { ships: 0 };
const curve = [{ round: 0, scalar: 1 }];
let incumbentScalar = 1;
for (const [key, group] of [...rounds.entries()].sort((a, b) => a[1].order - b[1].order)) {
  const { era } = meta(key);
  const winner = group.entries.find((e) => e.promoted);
  if (winner) {
    incumbentScalar = winner.scalar;
    curve.push({ round: key, scalar: winner.scalar, proposer: winner.proposer });
    if (era === "tournament") tally[winner.proposer] = (tally[winner.proposer] ?? 0) + 1;
    else if (era === "challenge") community.ships += 1;
    else { solo.promotions += 1; solo.finalScalar = winner.scalar; }
  } else if (era === "tournament") {
    tally.rejectedRounds += 1;
  } else if (era === "solo") {
    solo.rejected += 1;
  }
}

const payload = {
  updatedAt: new Date().toISOString(),
  task: "audit-kernel-fuel",
  baselineFuel: BASELINE_FUEL,
  incumbent: {
    scalar: incumbentScalar,
    fuelCutPct: Math.round((1 - 1 / incumbentScalar) * 1000) / 10,
  },
  tally,
  curve,
  solo,
  community,
  rounds: [...rounds.entries()]
    .sort((a, b) => b[1].order - a[1].order)
    .map(([key, group]) => ({ round: key, display: meta(key).display, era: meta(key).era, entries: group.entries })),
};

writeFileSync(outPath, JSON.stringify(payload, null, 2) + "\n");
console.log(`arena.json: ${payload.rounds.length} rounds, incumbent ${incumbentScalar}, Claude ${tally.Claude} / Grok ${tally.Grok}`);
