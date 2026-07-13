#!/usr/bin/env node
// reanchor-line-refs.mjs — systematic re-anchor of in-source line-number citations.
//
// Session #32 census (project_session_2026_07_03_libref_drift_census_gated):
// drift-check prose inside comments and assertion strings cites file:line
// positions that rot as code moves. Three citation styles are swept:
//   1. explicit crate paths   covenant-memory/src/lib.rs:384
//   2. bare self/crate refs   lib.rs:4861   main.rs:3495
//   3. prose refs             at line 342   at lines 315-318
//
// Strategy is CONTENT re-anchoring via git history: the citing line's blame
// commit is when the citation was written (presumed correct then). We read the
// cited line's content from the target file AT that commit (`git show`), then
// locate that exact content in today's file, disambiguated by neighbor-line
// agreement. Format-only blame commits (rustfmt) are resolved against their
// parent. Prose anchor tokens remain only as a fallback when historical
// content cannot be matched. Number-for-number rewrites preserve line counts,
// so validator gates keyed to line positions in unrelated files stay intact.
//
// Usage: node agent-os/scripts/reanchor-line-refs.mjs [--write] [--verbose]
//   default is check mode: report drifted/unresolved citations, exit 0.
//   --write applies rewrites in place.

import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..', '..');
const CRATES = path.join(ROOT, 'agent-os', 'crates');
const WRITE = process.argv.includes('--write');
const VERBOSE = process.argv.includes('--verbose');

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'target' || entry.name.startsWith('.')) continue;
    const p = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(p, out);
    else if (entry.isFile() && p.endsWith('.rs')) out.push(p);
  }
  return out;
}

const PROGRAMS = path.join(ROOT, 'agent-os', 'programs');
const crateCache = new Map();
function crateOf(file) {
  const base = file.startsWith(PROGRAMS) ? PROGRAMS : CRATES;
  return path.relative(base, file).split(path.sep)[0];
}
function crateFile(crate, which) {
  const key = `${crate}/${which}`;
  if (!crateCache.has(key)) {
    const inCrates = path.join(CRATES, crate, 'src', `${which}.rs`);
    const inPrograms = path.join(PROGRAMS, crate, 'src', `${which}.rs`);
    const prefixed = path.join(CRATES, `covenant-${crate}`, 'src', `${which}.rs`);
    crateCache.set(
      key,
      fs.existsSync(inCrates) ? inCrates : fs.existsSync(inPrograms) ? inPrograms : fs.existsSync(prefixed) ? prefixed : null
    );
  }
  return crateCache.get(key);
}

const fileLines = new Map();
function linesOf(file) {
  if (!fileLines.has(file)) fileLines.set(file, fs.readFileSync(file, 'utf8').split('\n'));
  return fileLines.get(file);
}

// ---- git-history content matching -----------------------------------------
function git(...args) {
  return execFileSync('git', args, { cwd: ROOT, maxBuffer: 512 * 1024 * 1024 }).toString();
}
const blameCache = new Map(); // file -> sha per line (0-based)
function blameOf(file) {
  if (!blameCache.has(file)) {
    const shas = [];
    try {
      for (const l of git('blame', '--line-porcelain', '--', path.relative(ROOT, file)).split('\n')) {
        const m = /^([0-9a-f]{40}) \d+ \d+/.exec(l);
        if (m) shas.push(m[1]);
      }
    } catch { /* unblameable (untracked) */ }
    blameCache.set(file, shas);
  }
  return blameCache.get(file);
}
const subjectCache = new Map(); // sha -> isFormatOnly
function isFormatOnly(sha) {
  if (!subjectCache.has(sha)) {
    let s = '';
    try { s = git('show', '-s', '--format=%s', sha).trim(); } catch { /* ignore */ }
    subjectCache.set(sha, /^style\b|rustfmt|\bcargo fmt\b/i.test(s));
  }
  return subjectCache.get(sha);
}
const histCache = new Map(); // "sha:relPath" -> lines[] | null
function histLines(sha, relPath) {
  const key = `${sha}:${relPath}`;
  if (!histCache.has(key)) {
    try { histCache.set(key, git('show', key).split('\n')); }
    catch { histCache.set(key, null); }
  }
  return histCache.get(key);
}
// Locate historical line n (1-based, from lines H) in current lines L.
// Exact trimmed match first, then whitespace-normalized; candidates ranked by
// neighbor-line agreement (±2), tie-broken by distance to n. Accepts a unique
// exact hit, or any hit with >=2 agreeing neighbors.
function locate(H, L, n) {
  if (n < 1 || n > H.length) return null;
  const needle = (H[n - 1] ?? '').trim();
  if (!needle) return null;
  const neigh = k => (H[n - 1 + k] ?? '').trim();
  const scoreAt = j => {
    let s = 0;
    for (const k of [-2, -1, 1, 2]) {
      const h = neigh(k);
      if (h && (L[j + k] ?? '').trim() === h) s++;
    }
    return s;
  };
  const norm = t => t.replace(/\s+/g, ' ').trim();
  const passes = [l => l.trim() === needle, l => norm(l) === norm(needle)];
  for (const eq of passes) {
    const cand = [];
    for (let j = 0; j < L.length; j++) if (eq(L[j])) cand.push(j);
    if (cand.length === 0) continue;
    let best = -1, bestScore = -1;
    for (const j of cand) {
      const s = scoreAt(j);
      if (s > bestScore || (s === bestScore && Math.abs(j + 1 - n) < Math.abs(best + 1 - n))) {
        best = j; bestScore = s;
      }
    }
    if (cand.length === 1 || bestScore >= 2) return { line: best + 1, score: bestScore };
  }
  return null;
}

// Combined citation scanner. Explicit paths first so bare refs never double-match.
const CITE_RE = new RegExp(
  [
    // 1: crate, 2: lib|main, 3: start, 4: end?
    String.raw`(?:agent-os\/)?(?:crates\/|programs\/)?([A-Za-z0-9_-]+)\/src\/(lib|main)\.rs:(\d+)(?:-(\d+))?`,
    // 5: lib|main, 6: start, 7: end?
    String.raw`(?<![\w\/])(lib|main)\.rs:(\d+)(?:-(\d+))?`,
    // 8: start, 9: end?
    String.raw`\bat lines? (\d+)(?:-(\d+))?`,
  ].join('|'),
  'g',
);

// Anchor candidate tokens from a prose line, ordered by distance to citeIdx
// (tokens preceding the citation win ties — "`fn foo` at line N" style).
const STOPWORDS = new Set([
  'lib.rs', 'main.rs', 'src', 'the', 'at', 'line', 'lines', 'drift', 'check',
  'drift-check', 'see', 'fn', 'struct', 'enum', 'pub', 'let', 'const',
]);
function anchorCandidates(line, citeIdx) {
  const cands = [];
  const push = (tok, idx) => {
    tok = tok.trim();
    if (tok.length < 4 || STOPWORDS.has(tok)) return;
    if (/^[\d\s:,.-]+$/.test(tok)) return;
    if (/(?:lib|main)\.rs:\d/.test(tok) || /\bat lines? \d/.test(tok)) return;
    cands.push({ tok, idx });
  };
  for (const m of line.matchAll(/`([^`]+)`/g)) push(m[1], m.index);
  for (const m of line.matchAll(/"((?:[^"\\]|\\.){4,}?)"/g)) push(m[1], m.index);
  for (const m of line.matchAll(/\b(?:fn|struct|enum|trait|const|static|mod) ([A-Za-z_][A-Za-z0-9_]*)/g)) {
    push(`${m[0]}`, m.index);
  }
  for (const m of line.matchAll(/\b([A-Za-z_][A-Za-z0-9_]*(?:::|_)[A-Za-z0-9_:]+)\b/g)) push(m[1], m.index);
  // dotted paths/calls: issuer.clone(), event.issuer.pubkey
  for (const m of line.matchAll(/\b([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)+)/g)) push(m[1], m.index);
  // CamelCase type names: MemoryRecord, AuditEvent
  for (const m of line.matchAll(/\b([A-Z][a-z0-9]+(?:[A-Z][a-z0-9]+)+)\b/g)) push(m[1], m.index);
  // struct-field fragments: parent: None
  for (const m of line.matchAll(/\b([A-Za-z_][A-Za-z0-9_]*: [A-Za-z_][A-Za-z0-9_:.()]+)/g)) push(m[1], m.index);
  cands.sort((a, b) => {
    const da = Math.abs(a.idx - citeIdx) + (a.idx > citeIdx ? 15 : 0);
    const db = Math.abs(b.idx - citeIdx) + (b.idx > citeIdx ? 15 : 0);
    return da - db;
  });
  const seen = new Set();
  return cands.filter(c => !seen.has(c.tok) && seen.add(c.tok)).map(c => c.tok);
}

function findAnchor(citedFile, anchors, oldStart, skipLine) {
  for (const anchor of anchors) {
    const needle = anchor.replace(/\\"/g, '"');
    const hits = [];
    const lines = linesOf(citedFile);
    for (let i = 0; i < lines.length; i++) {
      if (i + 1 === skipLine) continue;
      if (lines[i].includes(needle)) hits.push(i + 1);
    }
    if (hits.length === 0) continue;
    if (hits.length === 1) return { line: hits[0], anchor };
    let best = hits[0];
    for (const h of hits) if (Math.abs(h - oldStart) < Math.abs(best - oldStart)) best = h;
    return { line: best, anchor };
  }
  return null;
}

const report = { total: 0, valid: 0, rewritten: 0, unresolved: [] };
const files = [...walk(CRATES), ...walk(PROGRAMS)];

for (const file of files) {
  const rel = path.relative(ROOT, file);
  const lines = linesOf(file);
  let changed = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!/(?:lib|main)\.rs:\d|\bat lines? \d/.test(line)) continue;
    const matches = [...line.matchAll(CITE_RE)];
    // right-to-left so splices keep earlier indices stable
    for (const m of matches.reverse()) {
      report.total++;
      let citedFile, start, end, numSpanStart, numText;
      if (m[1] !== undefined) {
        citedFile = crateFile(m[1], m[2]);
        start = +m[3]; end = m[4] ? +m[4] : null;
      } else if (m[5] !== undefined) {
        const c = crateOf(file);
        citedFile = crateFile(c, m[5]) ?? (m[5] === 'main' ? crateFile('covenant', 'main') : null);
        start = +m[6]; end = m[7] ? +m[7] : null;
      } else {
        citedFile = file; // "at line N" cites the surrounding file
        start = +m[8]; end = m[9] ? +m[9] : null;
      }
      const fail = reason => report.unresolved.push({ at: `${rel}:${i + 1}`, cite: m[0], reason });
      if (!citedFile || !fs.existsSync(citedFile)) { fail('cited file not found'); continue; }

      // Primary: historical-content match via the citing line's blame commit.
      // Uncommitted citing lines have no blame ancestry — never guess there.
      let newStart = null, newEnd = null, via = null;
      const blameSha = blameOf(file)[i];
      if (!blameSha || /^0+$/.test(blameSha)) { fail('citing line uncommitted; rerun after commit'); continue; }
      {
        const effSha = isFormatOnly(blameSha) ? `${blameSha}^` : blameSha;
        const relTarget = path.relative(ROOT, citedFile);
        const H = histLines(effSha, relTarget) ?? histLines(blameSha, relTarget);
        if (H) {
          const cur = linesOf(citedFile);
          const s = locate(H, cur, start);
          if (s) {
            newStart = s.line;
            via = `content@${effSha.slice(0, 10)}`;
            if (end !== null) {
              const e = locate(H, cur, end);
              newEnd = e && e.line >= s.line ? e.line : end + (s.line - start);
            }
          }
        }
      }

      // Fallback: prose anchor tokens (same line, then up to 3 lines back).
      if (newStart === null) {
        let anchors = anchorCandidates(line, m.index);
        for (let back = 1; back <= 3 && i - back >= 0; back++) {
          anchors = anchors.concat(anchorCandidates(lines[i - back], lines[i - back].length));
        }
        const seenA = new Set();
        anchors = anchors.filter(a => !seenA.has(a) && seenA.add(a));
        if (anchors.length === 0) { fail('no content match; no anchor candidates in prose'); continue; }
        const skipLine = citedFile === file ? i + 1 : 0;
        const found = findAnchor(citedFile, anchors, start, skipLine);
        if (!found) { fail(`no content match; anchor not found (tried: ${anchors.slice(0, 3).join(' | ')})`); continue; }
        newStart = found.line;
        newEnd = end !== null ? end + (found.line - start) : null;
        via = `anchor:${found.anchor.slice(0, 40)}`;
      }

      if (newStart === start && (end === null || newEnd === end)) { report.valid++; continue; }
      report.rewritten++;
      numText = end !== null ? `${start}-${end}` : `${start}`;
      const newText = end !== null ? `${newStart}-${newEnd}` : `${newStart}`;
      numSpanStart = m.index + m[0].lastIndexOf(numText);
      lines[i] = lines[i].slice(0, numSpanStart) + newText + lines[i].slice(numSpanStart + numText.length);
      changed = true;
      if (VERBOSE) console.log(`  ${rel}:${i + 1}  ${numText} -> ${newText}  (${via})`);
    }
  }
  if (changed && WRITE) fs.writeFileSync(file, lines.join('\n'));
}

console.log(`reanchor-line-refs: scanned ${files.length} files`);
console.log(`  citations: ${report.total}  valid: ${report.valid}  ${WRITE ? 'rewritten' : 'drifted'}: ${report.rewritten}  unresolved: ${report.unresolved.length}`);
for (const u of report.unresolved) console.log(`  UNRESOLVED ${u.at}  [${u.cite}]  ${u.reason}`);
if (!WRITE && report.rewritten > 0) console.log('  (check mode — rerun with --write to apply)');
