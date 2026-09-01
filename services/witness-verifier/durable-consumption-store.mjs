import { createHash } from 'node:crypto';
import { lstat, mkdir, open, readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

export const CANONICAL_CONSUMPTION_DIRECTORY = join(
  homedir(),
  '.config',
  'covenant',
  'witness-enforcement-v2',
  'consumptions-v1',
);

function canonicalJson(value) {
  if (value === null || typeof value === 'boolean' || typeof value === 'string') {
    return JSON.stringify(value);
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new Error('canonical JSON refuses non-finite numbers');
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (typeof value !== 'object' || value === undefined) {
    throw new Error('canonical JSON refuses unsupported values');
  }
  return `{${Object.keys(value)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
    .join(',')}}`;
}

export function canonicalConsumptionFile(consumptionKey) {
  if (!/^sha256:[0-9a-f]{64}$/.test(consumptionKey)) {
    throw new Error('W009 consumption key is invalid');
  }
  return join(CANONICAL_CONSUMPTION_DIRECTORY, `${consumptionKey.slice(7)}.json`);
}

export async function assertNoLegacyConsumptionAt(legacyDirectory, {
  runId,
  consumptionKey,
}) {
  const target = join(legacyDirectory, `${runId}.consumed.json`);
  let record;
  try {
    record = JSON.parse(await readFile(target, 'utf8'));
  } catch (error) {
    if (error?.code === 'ENOENT') return;
    throw new Error('W009 legacy consumption state is unreadable');
  }
  if (record.run_id !== runId || record.consumption_key !== consumptionKey) {
    throw new Error('W009 legacy consumption state conflicts with this grant');
  }
  throw new Error('W009 grant replay blocked: legacy durable consumption already exists');
}

export async function reserveDurablyAt(
  stateDirectory,
  { runId, consumptionKey, proposalHash },
) {
  if (!/^sha256:[0-9a-f]{64}$/.test(consumptionKey)) {
    throw new Error('W009 consumption key is invalid');
  }
  await mkdir(stateDirectory, { recursive: true, mode: 0o700 });
  const stateDirectoryStats = await lstat(stateDirectory);
  if (
    !stateDirectoryStats.isDirectory() ||
    stateDirectoryStats.isSymbolicLink() ||
    (stateDirectoryStats.mode & 0o077) !== 0
  ) {
    throw new Error('W009 durable state directory is unsafe');
  }
  const record = {
    schema: 'covenant.grant-consumption-reservation.v1',
    run_id: runId,
    consumption_key: consumptionKey,
    proposal_hash: proposalHash,
    reserved_at: new Date().toISOString(),
  };
  const bytes = Buffer.from(`${canonicalJson(record)}\n`, 'utf8');
  const target = join(stateDirectory, `${consumptionKey.slice(7)}.json`);
  let file;
  try {
    file = await open(target, 'wx', 0o600);
  } catch (error) {
    if (error?.code === 'EEXIST') {
      throw new Error('W009 grant replay blocked: durable consumption already exists');
    }
    throw error;
  }
  try {
    await file.writeFile(bytes);
    await file.sync();
  } finally {
    await file.close();
  }
  const directory = await open(stateDirectory, 'r');
  try {
    await directory.sync();
  } finally {
    await directory.close();
  }
  return {
    scheme: 'canonical_exclusive_fsync_file.v1',
    record,
    record_sha256: `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
  };
}

export async function reserveCanonicalConsumption(input) {
  await assertNoLegacyConsumptionAt(dirname(CANONICAL_CONSUMPTION_DIRECTORY), input);
  return reserveDurablyAt(CANONICAL_CONSUMPTION_DIRECTORY, input);
}
