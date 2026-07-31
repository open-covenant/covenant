#!/usr/bin/env node

import { createHash, randomUUID } from 'node:crypto';
import { link, open, realpath, unlink } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import timelinePackage from '@covenant-org/timeline/package.json' with { type: 'json' };
import {
  canonicalJson,
  contentDigest,
  parseJson,
  parseQueryV0Alpha3,
  parseRunDocumentV0Alpha3,
  reasonTemporalQueryV0Alpha3,
  verifyTemporalConclusionV0Alpha3,
} from '@covenant-org/timeline';

const observationSchema = 'covenant.release-temporal-observation.v1';
const reportSchema = 'covenant.timeline.release-verification.v1';
const releaseId = 'v0.1.0-alpha.1';
const repositoryId = 'open-covenant/covenant';
const taggedCommit = '94e7af53c2224aa40762c2061ac96cab34950b71';
const releaseUrl = 'https://github.com/open-covenant/covenant/releases/tag/v0.1.0-alpha.1';
const readinessSourcePath = 'docs/releases/v0.1.0-alpha.1/evidence.json';
const readinessSourceDigest = 'aca4d4767380d0a9244b827e5cf4959af81719f45e2703612ad66052b3995b39';
const timelinePackageName = '@covenant-org/timeline';
const timelinePackageVersion = '0.0.0-alpha.2';
const inputLimits = {
  observation: 64 * 1024,
  readinessSource: 1024 * 1024,
  report: 1024 * 1024,
  run: 256 * 1024,
};

function assertObject(value, label) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value;
}

function assertExactKeys(value, expected, label) {
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new Error(`${label} has an unexpected shape`);
  }
}

function daysInMonth(year, month) {
  if ([1, 3, 5, 7, 8, 10, 12].includes(month)) return 31;
  if ([4, 6, 9, 11].includes(month)) return 30;
  if (month !== 2) return 0;
  return year % 400 === 0 || (year % 4 === 0 && year % 100 !== 0) ? 29 : 28;
}

function parseUtcMilliseconds(value) {
  if (typeof value !== 'string') return null;
  const match =
    /^(?<year>\d{4})-(?<month>\d{2})-(?<day>\d{2})T(?<hour>\d{2}):(?<minute>\d{2}):(?<second>\d{2})(?:\.(?<millisecond>\d{3}))?Z$/u.exec(
      value,
    );
  if (match === null) return null;

  const { year, month, day, hour, minute, second, millisecond = '0' } = match.groups;
  const parts = [year, month, day, hour, minute, second, millisecond].map(Number);
  const [
    numericYear,
    numericMonth,
    numericDay,
    numericHour,
    numericMinute,
    numericSecond,
    numericMillisecond,
  ] = parts;
  if (
    numericMonth < 1 ||
    numericMonth > 12 ||
    numericDay < 1 ||
    numericDay > daysInMonth(numericYear, numericMonth) ||
    numericHour > 23 ||
    numericMinute > 59 ||
    numericSecond > 59
  ) {
    return null;
  }

  const adjustedYear = numericYear - Number(numericMonth <= 2);
  const era = Math.floor(adjustedYear / 400);
  const yearOfEra = adjustedYear - era * 400;
  const shiftedMonth = numericMonth + (numericMonth > 2 ? -3 : 9);
  const dayOfYear = Math.floor((153 * shiftedMonth + 2) / 5) + numericDay - 1;
  const dayOfEra =
    yearOfEra * 365 + Math.floor(yearOfEra / 4) - Math.floor(yearOfEra / 100) + dayOfYear;
  const daysSinceEpoch = era * 146_097 + dayOfEra - 719_468;
  return (
    daysSinceEpoch * 86_400_000 +
    numericHour * 3_600_000 +
    numericMinute * 60_000 +
    numericSecond * 1_000 +
    numericMillisecond
  );
}

function parseObservation(value, expectedId, expectedKind) {
  const observation = assertObject(value, expectedId);
  assertExactKeys(
    observation,
    ['schema', 'id', 'repository', 'release', 'tagCommit', 'source', 'fact'],
    expectedId,
  );
  if (
    observation.schema !== observationSchema ||
    observation.id !== expectedId ||
    observation.repository !== repositoryId ||
    observation.release !== releaseId ||
    observation.tagCommit !== taggedCommit
  ) {
    throw new Error(`${expectedId} identity does not match the release`);
  }

  const fact = assertObject(observation.fact, `${expectedId}.fact`);
  const required = ['kind', 'occurredAt', 'coordinateMs'];
  const optional = expectedKind === 'release.readiness-recorded' ? ['ready', 'commit'] : [];
  assertExactKeys(fact, [...required, ...optional], `${expectedId}.fact`);
  if (
    fact.kind !== expectedKind ||
    !Number.isSafeInteger(fact.coordinateMs) ||
    parseUtcMilliseconds(fact.occurredAt) !== fact.coordinateMs
  ) {
    throw new Error(`${expectedId} has an invalid temporal fact`);
  }
  if (
    expectedKind === 'release.readiness-recorded' &&
    (fact.ready !== true || fact.commit !== taggedCommit)
  ) {
    throw new Error('readiness evidence must record ready=true for the tagged commit');
  }
  assertObject(observation.source, `${expectedId}.source`);
  return observation;
}

function verifyGitHubSource(observation, expectedField) {
  const source = observation.source;
  assertExactKeys(source, ['kind', 'field', 'url'], `${observation.id}.source`);
  if (
    source.kind !== 'github-release' ||
    source.field !== expectedField ||
    source.url !== releaseUrl
  ) {
    throw new Error(`${observation.id} source is invalid`);
  }
}

function assertTimelineInstallation() {
  if (
    timelinePackage.name !== timelinePackageName ||
    timelinePackage.version !== timelinePackageVersion
  ) {
    throw new Error(
      `expected ${timelinePackageName}@${timelinePackageVersion}, found ${timelinePackage.name}@${timelinePackage.version}`,
    );
  }
}

async function readBytes(path, limit, label) {
  const file = await open(path, 'r');
  try {
    const metadata = await file.stat();
    if (!metadata.isFile()) throw new Error(`${label} is not a regular file`);
    if (metadata.size > limit) throw new Error(`${label} exceeds the ${limit}-byte limit`);

    const buffer = Buffer.alloc(limit + 1);
    let length = 0;
    while (length < buffer.length) {
      const { bytesRead } = await file.read(buffer, length, buffer.length - length, length);
      if (bytesRead === 0) break;
      length += bytesRead;
    }
    if (length > limit) throw new Error(`${label} exceeds the ${limit}-byte limit`);
    return buffer.subarray(0, length);
  } finally {
    await file.close();
  }
}

async function readJson(path, limit, label) {
  return parseJson((await readBytes(path, limit, label)).toString('utf8'));
}

async function readObservations(evidenceDirectory) {
  const definitions = [
    ['release-created', 'release.created'],
    ['readiness-recorded', 'release.readiness-recorded'],
    ['release-published', 'release.published'],
  ];
  return new Map(
    await Promise.all(
      definitions.map(async ([id, kind]) => {
        const observation = parseObservation(
          await readJson(
            resolve(evidenceDirectory, `${id}.json`),
            inputLimits.observation,
            `${id} observation`,
          ),
          id,
          kind,
        );
        return [id, observation];
      }),
    ),
  );
}

function isWithin(root, path) {
  const pathFromRoot = relative(root, path);
  return (
    pathFromRoot !== '' &&
    pathFromRoot !== '..' &&
    !pathFromRoot.startsWith(`..${sep}`) &&
    !isAbsolute(pathFromRoot)
  );
}

async function verifyTaggedCommitReadinessSource(observation, repositoryRoot) {
  const source = observation.source;
  assertExactKeys(source, ['kind', 'field', 'path', 'sha256'], 'readiness-recorded.source');
  if (
    source.kind !== 'release-evidence' ||
    source.field !== 'readiness.generated_at' ||
    source.path !== readinessSourcePath ||
    !/^[0-9a-f]{64}$/u.test(source.sha256)
  ) {
    throw new Error('readiness source is invalid');
  }

  const root = await realpath(repositoryRoot);
  const candidate = resolve(root, source.path);
  if (!isWithin(root, candidate)) {
    throw new Error('readiness source escapes the repository');
  }
  const sourcePath = await realpath(candidate);
  if (!isWithin(root, sourcePath)) {
    throw new Error('readiness source escapes the repository');
  }
  const bytes = await readBytes(
    sourcePath,
    inputLimits.readinessSource,
    'tagged-commit readiness source',
  );
  const digest = createHash('sha256').update(bytes).digest('hex');
  if (digest !== source.sha256 || digest !== readinessSourceDigest) {
    throw new Error('readiness source digest does not match');
  }

  const releaseEvidence = parseJson(bytes.toString('utf8'));
  if (
    releaseEvidence?.schema !== 'covenant.alpha-release-evidence.v1' ||
    releaseEvidence?.commit !== taggedCommit ||
    releaseEvidence?.readiness?.ready !== true ||
    releaseEvidence?.readiness?.generated_at !== observation.fact.occurredAt ||
    observation.fact.commit !== releaseEvidence.commit
  ) {
    throw new Error('readiness observation does not match its source');
  }
}

function eventById(run, id) {
  const event = run.events.find((candidate) => candidate.id === id);
  if (event === undefined) throw new Error(`run is missing ${id}`);
  return event;
}

function assertRunContract(run) {
  const contract = {
    schema: 'covenant.timeline.contract.v0alpha3',
    id: 'covenant.release.v0.1.0-alpha.1.temporal.v1',
    subject: {
      kind: 'release',
      id: 'open-covenant/covenant/v0.1.0-alpha.1',
    },
    axes: [
      {
        id: 'unix-ms',
        kind: 'metric',
        unit: 'millisecond',
        origin: 'unix.epoch',
      },
    ],
    contexts: [{ id: 'actual', mode: 'actual' }],
  };
  const points = [
    {
      schema: 'covenant.timeline.event.v0alpha3',
      id: 'event.release-publication-point',
      sequence: 0,
      type: 'point.declared',
      point: {
        id: 'artifacts-published',
        contextId: 'actual',
        axisId: 'unix-ms',
      },
    },
    {
      schema: 'covenant.timeline.event.v0alpha3',
      id: 'event.tagged-commit-readiness-point',
      sequence: 1,
      type: 'point.declared',
      point: {
        id: 'tagged-commit-readiness-recorded',
        contextId: 'actual',
        axisId: 'unix-ms',
      },
    },
  ];
  if (
    run.events.length !== 6 ||
    canonicalJson(run.contract) !== canonicalJson(contract) ||
    canonicalJson(run.events.slice(0, 2)) !== canonicalJson(points)
  ) {
    throw new Error('run does not match the Covenant release contract');
  }
}

function assertRunBindings(run, observations) {
  verifyGitHubSource(observations.get('release-created'), 'createdAt');
  verifyGitHubSource(observations.get('release-published'), 'publishedAt');

  const digests = Object.fromEntries(
    [...observations].map(([id, value]) => [id, contentDigest(value)]),
  );
  const expected = [
    {
      id: 'event.release-publication-provisional',
      sequence: 2,
      assertionId: 'release.publication.provisional.v1',
      pointId: 'artifacts-published',
      coordinateMs: observations.get('release-created').fact.coordinateMs,
      evidenceRef: digests['release-created'],
    },
    {
      id: 'event.tagged-commit-readiness',
      sequence: 3,
      assertionId: 'release.tagged-commit-readiness.v1',
      pointId: 'tagged-commit-readiness-recorded',
      coordinateMs: observations.get('readiness-recorded').fact.coordinateMs,
      evidenceRef: digests['readiness-recorded'],
    },
    {
      id: 'event.release-publication-authoritative',
      sequence: 4,
      assertionId: 'release.publication.github.v1',
      pointId: 'artifacts-published',
      coordinateMs: observations.get('release-published').fact.coordinateMs,
      evidenceRef: digests['release-published'],
    },
  ];

  for (const binding of expected) {
    const event = eventById(run, binding.id);
    const wanted = {
      schema: 'covenant.timeline.event.v0alpha3',
      id: binding.id,
      sequence: binding.sequence,
      type: 'coordinate.asserted',
      assertion: {
        id: binding.assertionId,
        contextId: 'actual',
        pointId: binding.pointId,
        coordinate: {
          minimum: binding.coordinateMs,
          maximum: binding.coordinateMs,
        },
        evidenceRefs: [binding.evidenceRef],
      },
    };
    if (canonicalJson(event) !== canonicalJson(wanted)) {
      throw new Error(`${binding.id} does not match its evidence`);
    }
  }

  const retraction = eventById(run, 'event.release-publication-reconciled');
  const wantedRetraction = {
    schema: 'covenant.timeline.event.v0alpha3',
    id: 'event.release-publication-reconciled',
    sequence: 5,
    type: 'assertion.retracted',
    assertionId: 'release.publication.provisional.v1',
    evidenceRefs: [digests['release-published']],
  };
  if (canonicalJson(retraction) !== canonicalJson(wantedRetraction)) {
    throw new Error('publication retraction does not match its evidence');
  }
  return digests;
}

function query(run, recordedThrough) {
  return parseQueryV0Alpha3(
    {
      schema: 'covenant.timeline.query.v0alpha3',
      id: 'query.tagged-commit-readiness-minus-publication',
      contextId: 'actual',
      recordedThrough,
      type: 'difference.bounds',
      fromPointId: 'artifacts-published',
      toPointId: 'tagged-commit-readiness-recorded',
    },
    run,
  );
}

function evaluateCut(run, name, recordedThrough) {
  const temporalQuery = query(run, recordedThrough);
  const conclusion = reasonTemporalQueryV0Alpha3(run, temporalQuery);
  if (!verifyTemporalConclusionV0Alpha3(run, temporalQuery, conclusion)) {
    throw new Error(`${name} proof did not verify`);
  }
  return { name, recordedThrough, conclusion, verified: true };
}

function assertExpectedResults(cuts) {
  const expected = [
    {
      type: 'difference.bounds',
      status: 'bounded',
      minimum: 513_698,
      maximum: 513_698,
    },
    {
      type: 'difference.bounds',
      status: 'inconsistent',
      minimum: null,
      maximum: null,
    },
    {
      type: 'difference.bounds',
      status: 'bounded',
      minimum: 360_698,
      maximum: 360_698,
    },
  ];
  if (
    cuts.length !== expected.length ||
    cuts.some(
      (cut, index) => canonicalJson(cut.conclusion.result) !== canonicalJson(expected[index]),
    )
  ) {
    throw new Error('release chronology results do not match the expected cuts');
  }
}

async function loadReleaseInputs({ evidenceDirectory, repositoryRoot, runPath }) {
  assertTimelineInstallation();
  const observations = await readObservations(evidenceDirectory);
  await verifyTaggedCommitReadinessSource(observations.get('readiness-recorded'), repositoryRoot);
  const run = parseRunDocumentV0Alpha3(await readJson(runPath, inputLimits.run, 'Timeline run'));
  assertRunContract(run);

  const evidenceDigests = assertRunBindings(run, observations);
  return { evidenceDigests, run };
}

function buildReport(run, evidenceDigests, cuts) {
  return {
    schema: reportSchema,
    repository: repositoryId,
    release: releaseId,
    commit: taggedCommit,
    timeline: {
      package: timelinePackageName,
      version: timelinePackageVersion,
      contract: 'v0alpha3',
    },
    runDigest: contentDigest(run),
    evidenceDigests,
    cuts,
    reconciliation: {
      provisionalLagMs: 513_698,
      authoritativeLagMs: 360_698,
      correctedByMs: 153_000,
    },
  };
}

export async function generateReleaseReport({ evidenceDirectory, repositoryRoot, runPath }) {
  const { evidenceDigests, run } = await loadReleaseInputs({
    evidenceDirectory,
    repositoryRoot,
    runPath,
  });
  const cuts = [
    evaluateCut(run, 'before', 3),
    evaluateCut(run, 'transition', 4),
    evaluateCut(run, 'after', 5),
  ];
  assertExpectedResults(cuts);
  return buildReport(run, evidenceDigests, cuts);
}

function assertStoredReport(report, run, evidenceDigests) {
  assertExactKeys(
    assertObject(report, 'verification report'),
    [
      'schema',
      'repository',
      'release',
      'commit',
      'timeline',
      'runDigest',
      'evidenceDigests',
      'cuts',
      'reconciliation',
    ],
    'verification report',
  );
  const expected = buildReport(run, evidenceDigests, report.cuts);
  for (const key of [
    'schema',
    'repository',
    'release',
    'commit',
    'timeline',
    'runDigest',
    'evidenceDigests',
    'reconciliation',
  ]) {
    if (canonicalJson(report[key]) !== canonicalJson(expected[key])) {
      throw new Error(`verification report ${key} does not match the release`);
    }
  }
  if (!Array.isArray(report.cuts) || report.cuts.length !== 3) {
    throw new Error('verification report must contain exactly three cuts');
  }

  const expectedCuts = [
    ['before', 3],
    ['transition', 4],
    ['after', 5],
  ];
  for (let index = 0; index < expectedCuts.length; index += 1) {
    const cut = assertObject(report.cuts[index], `verification report cut ${index}`);
    assertExactKeys(
      cut,
      ['name', 'recordedThrough', 'conclusion', 'verified'],
      `verification report cut ${index}`,
    );
    const [name, recordedThrough] = expectedCuts[index];
    if (cut.name !== name || cut.recordedThrough !== recordedThrough || cut.verified !== true) {
      throw new Error(`verification report cut ${index} identity does not match`);
    }

    const conclusion = assertObject(cut.conclusion, `verification report cut ${index} conclusion`);
    assertExactKeys(
      conclusion,
      ['schema', 'queryId', 'result', 'receipt'],
      `verification report cut ${index} conclusion`,
    );
    const temporalQuery = query(run, recordedThrough);
    if (
      conclusion.schema !== 'covenant.timeline.conclusion.v0alpha3' ||
      conclusion.queryId !== temporalQuery.id ||
      !verifyTemporalConclusionV0Alpha3(run, temporalQuery, conclusion)
    ) {
      throw new Error(`verification report cut ${index} proof did not verify`);
    }
  }
  assertExpectedResults(report.cuts);
}

export async function verifyStoredReleaseReport({
  evidenceDirectory,
  reportPath,
  repositoryRoot,
  runPath,
}) {
  const { evidenceDigests, run } = await loadReleaseInputs({
    evidenceDirectory,
    repositoryRoot,
    runPath,
  });
  const report = await readJson(reportPath, inputLimits.report, 'verification report');
  assertStoredReport(report, run, evidenceDigests);
  return report;
}

export function serializeReport(report) {
  return `${JSON.stringify(JSON.parse(canonicalJson(report)), null, 2)}\n`;
}

async function writeNewAtomic(outputPath, contents) {
  const parent = dirname(outputPath);
  const temporaryPath = join(parent, `.${basename(outputPath)}.${process.pid}.${randomUUID()}.tmp`);
  let operationError;
  try {
    const file = await open(temporaryPath, 'wx', 0o644);
    try {
      await file.writeFile(contents, 'utf8');
      await file.sync();
    } finally {
      await file.close();
    }
    await link(temporaryPath, outputPath);
  } catch (error) {
    operationError = error;
    throw error;
  } finally {
    try {
      await unlink(temporaryPath);
    } catch (error) {
      if (error.code !== 'ENOENT' && operationError === undefined) throw error;
    }
  }
  await syncDirectory(parent);
}

async function syncDirectory(path) {
  const directory = await open(path, 'r');
  try {
    await directory.sync();
  } catch (error) {
    if (!['EINVAL', 'ENOTSUP'].includes(error.code)) throw error;
  } finally {
    await directory.close();
  }
}

function parseArguments(args) {
  const options = {};
  const names = {
    '--run': 'run',
    '--evidence-dir': 'evidenceDirectory',
    '--repository-root': 'repositoryRoot',
    '--report': 'report',
    '--output': 'output',
  };
  for (let index = 0; index < args.length; index += 1) {
    const key = args[index];
    if (key === '--generate') {
      if (options.generate === true) throw new Error('--generate was repeated');
      options.generate = true;
      continue;
    }

    const value = args[index + 1];
    if (names[key] === undefined || value === undefined || value.startsWith('--')) {
      throw new Error(
        'usage: verify.mjs --run <path> --evidence-dir <path> --repository-root <path> (--report <path> | --generate [--output <path>])',
      );
    }
    const name = names[key];
    if (options[name] !== undefined) throw new Error(`${key} was repeated`);
    options[name] = value;
    index += 1;
  }
  if (
    options.run === undefined ||
    options.evidenceDirectory === undefined ||
    options.repositoryRoot === undefined
  ) {
    throw new Error('run, evidence directory, and repository root are required');
  }
  if (options.generate === true) {
    if (options.report !== undefined) throw new Error('--report cannot be used with --generate');
  } else {
    if (options.report === undefined) throw new Error('--report is required for verification');
    if (options.output !== undefined) throw new Error('--output can only be used with --generate');
  }
  return options;
}

export async function main(
  args = process.argv.slice(2),
  { stdout = process.stdout, stderr = process.stderr } = {},
) {
  try {
    const options = parseArguments(args);
    const input = {
      evidenceDirectory: resolve(options.evidenceDirectory),
      repositoryRoot: resolve(options.repositoryRoot),
      runPath: resolve(options.run),
    };
    if (options.generate === true) {
      const report = await generateReleaseReport(input);
      const output = serializeReport(report);
      if (options.output === undefined) {
        stdout.write(output);
      } else {
        await writeNewAtomic(resolve(options.output), output);
      }
    } else {
      await verifyStoredReleaseReport({
        ...input,
        reportPath: resolve(options.report),
      });
      stdout.write('verified Covenant release Timeline report\n');
    }
    return 0;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    stderr.write(`verify Covenant release Timeline: ${message}\n`);
    return 1;
  }
}

const isEntrypoint =
  process.argv[1] !== undefined && pathToFileURL(process.argv[1]).href === import.meta.url;

if (isEntrypoint) process.exitCode = await main();
