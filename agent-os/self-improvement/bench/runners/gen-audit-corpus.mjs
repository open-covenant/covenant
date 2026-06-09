#!/usr/bin/env node
// Seeded generator for the audit-kernel fuel corpus. Deterministic: same
// seed -> byte-identical container. Container format consumed by
// kernel_bench.wasm: u32le case count, then per case u32le events-len,
// events bytes, u32le anchors-len, anchors bytes.
//
// Anchor math mirrors covenant-audit exactly: event_hash = sha256(line),
// chain_hash = sha256(`${prev}\n${event_hash}`), hex lowercase, zero root.
//
// Usage: gen-audit-corpus.mjs <out.bin> [--seed 1]

import { createHash } from 'node:crypto';
import { writeFileSync } from 'node:fs';

const ZERO = '0'.repeat(64);
const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a |= 0; a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const rnd = mulberry32(Number(process.argv.includes('--seed')
  ? process.argv[process.argv.indexOf('--seed') + 1]
  : 1));

const hex = (n) => Array.from({ length: n }, () => '0123456789abcdef'[Math.floor(rnd() * 16)]).join('');
const uuid = () => `${hex(8)}-${hex(4)}-4${hex(3)}-a${hex(3)}-${hex(12)}`;

const KINDS = [
  () => ({ type: 'intent_dispatched', intent_id: uuid(), intent_text: `task ${hex(12)}`, matched_agent: rnd() < 0.5 ? `agent-${hex(4)}` : null, result_hash_hex: hex(64), status: 'ok' }),
  () => ({ type: 'hermes_tool_invoked', intent_id: uuid(), run_id: hex(16), tool: `tool-${hex(6)}`, preview_hash_hex: hex(64) }),
  () => ({ type: 'hermes_tool_completed', intent_id: uuid(), run_id: hex(16), tool: `tool-${hex(6)}`, duration_ms: Math.floor(rnd() * 10000), error: rnd() < 0.1 }),
];

let ts = 1700000000000;
function eventLine() {
  ts += Math.floor(rnd() * 50) + 1;
  return JSON.stringify({ id: uuid(), timestamp_ms: ts, issuer: `agent-${hex(6)}`, kind: KINDS[Math.floor(rnd() * KINDS.length)]() });
}

function chain(lines) {
  let prev = ZERO;
  return lines.map((line, index) => {
    const event_hash_hex = sha256(Buffer.from(line, 'binary'));
    const chain_hash_hex = sha256(`${prev}\n${event_hash_hex}`);
    const entry = { index, event_id: '', timestamp_ms: 0, event_hash_hex, previous_hash_hex: prev, chain_hash_hex };
    try {
      const ev = JSON.parse(line);
      entry.event_id = ev.id; entry.timestamp_ms = ev.timestamp_ms;
    } catch { /* malformed lines keep empty id, matching kernel fold */ }
    prev = chain_hash_hex;
    return JSON.stringify(entry);
  });
}

function makeCase(events, mutate) {
  let lines = Array.from({ length: events }, eventLine);
  let anchors = chain(lines);
  if (mutate) {
    const m = mutate(lines, anchors);
    lines = m.evLines ?? lines;
    anchors = m.anchors ?? anchors;
  }
  return {
    events: Buffer.from(lines.join('\n') + '\n', 'binary'),
    anchors: Buffer.from(anchors.join('\n') + '\n', 'binary'),
  };
}

const cases = [
  makeCase(20000),
  makeCase(10000),
  makeCase(5000, (lines, anchors) => ({
    evLines: lines.map((l, i) => (i % 100 === 50 ? l.replace('"status":"ok"', '"status":"forged"') : l)),
    anchors,
  })),
  makeCase(5000, (lines, anchors) => ({
    evLines: lines,
    anchors: anchors.map((a, i) => (i % 100 === 25 ? a.replace(/"chain_hash_hex":"./, (m) => m.slice(0, -1) + (m.endsWith('f') ? '0' : 'f')) : a)),
  })),
  makeCase(5000, (lines, anchors) => ({
    evLines: lines.map((l, i) => (i % 50 === 10 ? '{broken json ' + hex(8) : l)),
    anchors,
  })),
  makeCase(2000, (lines, anchors) => ({
    evLines: lines.map((l, i) => (i % 40 === 5 ? '\xff\xfe binary \x80' + hex(6) : l)),
    anchors,
  })),
  makeCase(2000, (lines, anchors) => ({ evLines: lines, anchors: anchors.slice(0, -200) })),
  makeCase(1000, (lines, anchors) => ({ evLines: lines.slice(0, -100), anchors })),
];

const u32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
const parts = [u32(cases.length)];
for (const c of cases) parts.push(u32(c.events.length), c.events, u32(c.anchors.length), c.anchors);
const out = Buffer.concat(parts);

const target = process.argv[2];
if (!target) { console.error('usage: gen-audit-corpus.mjs <out.bin> [--seed N]'); process.exit(1); }
writeFileSync(target, out);
const total = cases.reduce((s, c) => s + c.events.length + c.anchors.length, 0);
console.log(`wrote ${target}: ${cases.length} cases, ${out.length} bytes (${total} payload), sha256 ${sha256(out)}`);
