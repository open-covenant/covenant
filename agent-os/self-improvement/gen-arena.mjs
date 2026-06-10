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
for (const v of ledger.versions) {
  const n = parseInt(v.version.match(/^k(\d+)/)?.[1] ?? 0, 10);
  if (!rounds.has(n)) rounds.set(n, []);
  rounds.get(n).push({
    proposer: PROPOSER_LABELS[v.proposer] ?? (v.proposer ? v.proposer[0].toUpperCase() + v.proposer.slice(1) : "Claude"),
    model: v.model ?? "claude",
    scalar: v.candidate ?? 0,
    incumbent: v.incumbent,
    gain: v.gain,
    promoted: !!v.promoted,
    commit: v.commit ?? null,
    reason: v.promoted ? null : String(v.reason ?? "").slice(0, 140),
  });
}

// k1-k8 = the loop's solo run (Claude as sole proposer); the tournament
// starts at k9 (shakedown) / k10 (Round 1). The vs-tally counts only
// tournament rounds; the curve keeps the whole history of the kernel.
const TOURNAMENT_START = 9;
const display = (n) => (n < TOURNAMENT_START ? `Run ${n}` : n === TOURNAMENT_START ? "Shakedown" : `Round ${n - TOURNAMENT_START}`);

const tally = { Claude: 0, Grok: 0, rejectedRounds: 0 };
const solo = { promotions: 0, rejected: 0, finalScalar: 1 };
const curve = [{ round: 0, scalar: 1 }];
let incumbentScalar = 1;
for (const [n, entries] of [...rounds.entries()].sort((a, b) => a[0] - b[0])) {
  const winner = entries.find((e) => e.promoted);
  const inTournament = n >= TOURNAMENT_START;
  if (winner) {
    incumbentScalar = winner.scalar;
    curve.push({ round: n, scalar: winner.scalar, proposer: winner.proposer });
    if (inTournament) tally[winner.proposer] = (tally[winner.proposer] ?? 0) + 1;
    else { solo.promotions += 1; solo.finalScalar = winner.scalar; }
  } else if (inTournament) {
    tally.rejectedRounds += 1;
  } else {
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
  rounds: [...rounds.entries()]
    .sort((a, b) => b[0] - a[0])
    .map(([n, entries]) => ({ round: n, display: display(n), era: n >= TOURNAMENT_START ? "tournament" : "solo", entries })),
};

writeFileSync(outPath, JSON.stringify(payload, null, 2) + "\n");
console.log(`arena.json: ${payload.rounds.length} rounds, incumbent ${incumbentScalar}, Claude ${tally.Claude} / Grok ${tally.Grok}`);
