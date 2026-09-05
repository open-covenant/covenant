#!/usr/bin/env node
/**
 * Renew the E2B sandbox tariff evidence the coding gateway validates.
 *
 * The gateway refuses to be ready unless this document sits inside a validity
 * window of at most seven days. It was renewed by hand, and when that stopped
 * the document expired, the gateway answered 503, and Mizuki could neither
 * admit paid work nor open bounty claims. A dated file with an expiry needs a
 * clock behind it, not a habit.
 *
 * Rates and sandbox identity are carried forward from the newest existing
 * document. Only the window moves. If E2B changes its pricing, update the rates
 * in a reviewed commit; this script will not invent them.
 */

import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const EVIDENCE_DIR = 'infra/mizuki/evidence';
const WINDOW_DAYS = 6; // one day inside the gateway's seven-day ceiling

const existing = readdirSync(EVIDENCE_DIR)
  .filter((name) => /^e2b-tariff-\d{4}-\d{2}-\d{2}\.json$/.test(name))
  .sort();
if (existing.length === 0) throw new Error(`no tariff evidence found in ${EVIDENCE_DIR}`);

const newest = JSON.parse(readFileSync(join(EVIDENCE_DIR, existing.at(-1)), 'utf8'));
const now = new Date();
const stamp = (date) => `${date.toISOString().slice(0, 19)}Z`;

const renewed = {
  ...newest,
  effectiveAt: stamp(now),
  validUntil: stamp(new Date(now.getTime() + WINDOW_DAYS * 24 * 60 * 60 * 1000)),
};

const day = now.toISOString().slice(0, 10);
const target = join(EVIDENCE_DIR, `e2b-tariff-${day}.json`);
const body = `${JSON.stringify(renewed, null, 2)}\n`;
writeFileSync(target, body);

console.log(`file=${target}`);
console.log(`validUntil=${renewed.validUntil}`);
// The digest is deliberately not computed here. Repository formatting rewrites
// this file (prettier normalises exponents such as 1.4e-05 to 1.4e-5), so a
// hash taken now would not match the bytes that get committed. The caller
// formats first, then hashes.
