import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { canonicalJson, parseJson } from '@covenant-org/timeline';
import {
  generateReleaseReport,
  main,
  serializeReport,
  verifyStoredReleaseReport,
} from './verify.mjs';

const directory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(directory, '../../../..');
const releaseDirectory = resolve(repositoryRoot, 'docs/releases/v0.1.0-alpha.1/timeline');
const evidenceDirectory = join(releaseDirectory, 'evidence');
const runPath = join(releaseDirectory, 'run.json');
const reportPath = join(releaseDirectory, 'verification.json');
const evidenceNames = ['release-created.json', 'readiness-recorded.json', 'release-published.json'];

const inputs = {
  evidenceDirectory,
  reportPath,
  repositoryRoot,
  runPath,
};

test('verifies the stored tagged-commit receipts without regenerating them', async () => {
  const report = await verifyStoredReleaseReport(inputs);
  assert.equal(report.commit, '94e7af53c2224aa40762c2061ac96cab34950b71');
});

test('regenerates the checked report byte-for-byte', async () => {
  const generated = await generateReleaseReport(inputs);
  assert.equal(serializeReport(generated), await readFile(reportPath, 'utf8'));
});

test('rejects a run whose corrected publication time was altered', async () => {
  const temporary = await mkdtemp(join(tmpdir(), 'covenant-timeline-run-'));
  try {
    const run = parseJson(await readFile(runPath, 'utf8'));
    run.events[4].assertion.coordinate.minimum += 1;
    run.events[4].assertion.coordinate.maximum += 1;
    const alteredRunPath = join(temporary, 'run.json');
    await writeFile(alteredRunPath, canonicalJson(run), 'utf8');
    await assert.rejects(
      verifyStoredReleaseReport({ ...inputs, runPath: alteredRunPath }),
      /does not match its evidence/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('rejects tagged-commit readiness that no longer matches the source bytes', async () => {
  const temporary = await copyEvidence((name, observation) => {
    if (name === 'readiness-recorded.json') observation.source.sha256 = '0'.repeat(64);
  });
  try {
    await assert.rejects(
      verifyStoredReleaseReport({ ...inputs, evidenceDirectory: temporary }),
      /readiness source digest does not match/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('rejects readiness for a different commit', async () => {
  const temporary = await copyEvidence((name, observation) => {
    if (name === 'readiness-recorded.json') {
      observation.fact.commit = 'a13a3481834a76e7868e85fac88ce2e618365a6a';
    }
  });
  try {
    await assert.rejects(
      verifyStoredReleaseReport({ ...inputs, evidenceDirectory: temporary }),
      /tagged commit/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('rejects an observation for a different tag commit', async () => {
  const temporary = await copyEvidence((name, observation) => {
    if (name === 'release-created.json') {
      observation.tagCommit = 'a13a3481834a76e7868e85fac88ce2e618365a6a';
    }
  });
  try {
    await assert.rejects(
      verifyStoredReleaseReport({ ...inputs, evidenceDirectory: temporary }),
      /identity does not match the release/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('rejects non-canonical UTC timestamps', async () => {
  const temporary = await copyEvidence((name, observation) => {
    if (name === 'release-created.json') {
      observation.fact.occurredAt = '2026-05-28T10:33:12+02:00';
    }
  });
  try {
    await assert.rejects(
      verifyStoredReleaseReport({ ...inputs, evidenceDirectory: temporary }),
      /invalid temporal fact/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('rejects a tagged-commit readiness source symlink outside the repository', async () => {
  const temporary = await mkdtemp(join(tmpdir(), 'covenant-timeline-repository-'));
  try {
    const source = join(temporary, 'docs/releases/v0.1.0-alpha.1/evidence.json');
    await mkdir(dirname(source), { recursive: true });
    await symlink(join(repositoryRoot, 'docs/releases/v0.1.0-alpha.1/evidence.json'), source);

    await assert.rejects(
      verifyStoredReleaseReport({ ...inputs, repositoryRoot: temporary }),
      /readiness source escapes the repository/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('rejects an altered stored proof receipt', async () => {
  const temporary = await mkdtemp(join(tmpdir(), 'covenant-timeline-report-'));
  try {
    const report = parseJson(await readFile(reportPath, 'utf8'));
    report.cuts[2].conclusion.receipt.semanticResultDigest = `sha256:${'0'.repeat(64)}`;
    const alteredReportPath = join(temporary, 'verification.json');
    await writeFile(alteredReportPath, canonicalJson(report), 'utf8');

    await assert.rejects(
      verifyStoredReleaseReport({ ...inputs, reportPath: alteredReportPath }),
      /proof did not verify/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('rejects altered stored report metadata', async () => {
  const temporary = await mkdtemp(join(tmpdir(), 'covenant-timeline-report-'));
  try {
    const report = parseJson(await readFile(reportPath, 'utf8'));
    report.runDigest = `sha256:${'0'.repeat(64)}`;
    const alteredReportPath = join(temporary, 'verification.json');
    await writeFile(alteredReportPath, canonicalJson(report), 'utf8');

    await assert.rejects(
      verifyStoredReleaseReport({ ...inputs, reportPath: alteredReportPath }),
      /runDigest does not match/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('rejects a run above the input limit', async () => {
  const temporary = await mkdtemp(join(tmpdir(), 'covenant-timeline-run-'));
  try {
    const oversizedRunPath = join(temporary, 'run.json');
    await writeFile(oversizedRunPath, Buffer.alloc(256 * 1024 + 1));
    await assert.rejects(
      generateReleaseReport({ ...inputs, runPath: oversizedRunPath }),
      /Timeline run exceeds the 262144-byte limit/u,
    );
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

test('writes generated reports atomically without clobbering', async () => {
  const temporary = await mkdtemp(join(tmpdir(), 'covenant-timeline-output-'));
  try {
    const outputPath = join(temporary, 'verification.json');
    const args = [
      '--generate',
      '--run',
      runPath,
      '--evidence-dir',
      evidenceDirectory,
      '--repository-root',
      repositoryRoot,
      '--output',
      outputPath,
    ];
    assert.equal(await main(args, quietIo()), 0);
    const original = await readFile(outputPath, 'utf8');
    assert.equal(original, await readFile(reportPath, 'utf8'));

    const stderr = captureStream();
    assert.equal(await main(args, { stdout: captureStream(), stderr }), 1);
    assert.match(stderr.output, /EEXIST/u);
    assert.equal(await readFile(outputPath, 'utf8'), original);
    assert.deepEqual(await readdir(temporary), ['verification.json']);
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
});

async function copyEvidence(mutate) {
  const temporary = await mkdtemp(join(tmpdir(), 'covenant-timeline-evidence-'));
  for (const name of evidenceNames) {
    const observation = parseJson(await readFile(join(evidenceDirectory, name), 'utf8'));
    mutate(name, observation);
    await writeFile(join(temporary, name), canonicalJson(observation), 'utf8');
  }
  return temporary;
}

function captureStream() {
  return {
    output: '',
    write(chunk) {
      this.output += chunk;
    },
  };
}

function quietIo() {
  return { stdout: captureStream(), stderr: captureStream() };
}
