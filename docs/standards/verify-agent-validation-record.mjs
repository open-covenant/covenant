#!/usr/bin/env node

// Pure DAS-envelope verifier for the proposed record profile. It deliberately
// performs no network access. Full conformance also requires the direct-RPC
// Core owner, AgentIdentity plugin, registration PDA, account discriminator,
// and profile-specific evidence checks described by the proposal.

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

export const RECORD_TYPE = 'mpl.agent.validation-record.v1';
const FIXTURE_KIND = 'synthetic-off-chain-conformance';

const BASE58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const BASE58_INDEX = new Map(
  [...BASE58_ALPHABET].map((character, index) => [character, BigInt(index)]),
);
const PUBLIC_KEY_PATTERN = /^[1-9A-HJ-NP-Za-km-z]+$/;
const PROFILE_PATTERN = /^[a-z0-9][a-z0-9._-]{2,127}$/;
const TOKEN_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const HASH_PATTERN = /^[0-9a-f]{64}$/;
const NAMESPACE_PATTERN = /^[a-z0-9][a-z0-9.-]*$/;
const PROFILE_SCHEMA_PATTERN = '^[a-z0-9][a-z0-9._-]*$';
const TOKEN_SCHEMA_PATTERN = '^[a-z0-9][a-z0-9._-]*$';
const CANONICAL_PAYLOAD_KEYS = new Set([
  'type',
  'schema',
  'subject',
  'validator',
  'hashAlg',
  'responseHash',
  'tag',
  'recordedAt',
  'extensions',
]);
const REQUIRED_PAYLOAD_KEYS = new Set([
  'type',
  'schema',
  'subject',
  'validator',
  'hashAlg',
  'responseHash',
  'recordedAt',
]);
const PAYLOAD_KEYS = new Set([
  ...CANONICAL_PAYLOAD_KEYS,
  'hash_alg',
  'response_hash',
  'recorded_at',
]);
const CANONICAL_SUBJECT_KEYS = new Set(['registryProgram', 'asset', 'registration']);
const REQUIRED_SUBJECT_KEYS = new Set(['registryProgram', 'asset']);
const SUBJECT_KEYS = new Set([...CANONICAL_SUBJECT_KEYS, 'registry_program']);

function decodeBase58(value) {
  if (typeof value !== 'string' || value.length === 0) return null;

  let number = 0n;
  for (const character of value) {
    const digit = BASE58_INDEX.get(character);
    if (digit === undefined) return null;
    number = number * 58n + digit;
  }

  const bytes = [];
  while (number > 0n) {
    bytes.push(Number(number & 0xffn));
    number >>= 8n;
  }

  for (const character of value) {
    if (character !== '1') break;
    bytes.push(0);
  }

  return Uint8Array.from(bytes.reverse());
}

function isPublicKey(value) {
  return decodeBase58(value)?.length === 32;
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function aliasedField(object, snake, camel) {
  const hasSnake = isObject(object) && hasOwn(object, snake);
  const hasCamel = isObject(object) && hasOwn(object, camel);
  return {
    ambiguous: hasSnake && hasCamel,
    value: hasSnake ? object[snake] : hasCamel ? object[camel] : undefined,
  };
}

function inspectAdapter(plugin, expectedValidator) {
  const reasons = [];
  const configField = aliasedField(plugin, 'adapter_config', 'adapterConfig');
  if (configField.ambiguous) {
    return {
      matches: false,
      reasons: ['ambiguous duplicate aliases adapter_config and adapterConfig'],
    };
  }
  if (!isObject(configField.value)) {
    return { matches: false, reasons: [] };
  }

  const authorityField = aliasedField(configField.value, 'data_authority', 'dataAuthority');
  if (authorityField.ambiguous) {
    return {
      matches: false,
      reasons: ['ambiguous duplicate aliases data_authority and dataAuthority'],
    };
  }
  if (!isObject(authorityField.value) || authorityField.value.address !== expectedValidator) {
    return { matches: false, reasons: [] };
  }
  if (authorityField.value.type !== 'Address') {
    reasons.push('data authority type is not Address');
  }
  if (configField.value.schema !== 'Json') {
    reasons.push('AppData adapter schema is not Json');
  }
  return { matches: reasons.length === 0, reasons };
}

function payloadField(payload, snake, camel) {
  return payload?.[snake] ?? payload?.[camel];
}

function hasOwn(object, key) {
  return Object.prototype.hasOwnProperty.call(object, key);
}

function sameSet(actual, expected) {
  return (
    Array.isArray(actual) &&
    actual.length === expected.size &&
    new Set(actual).size === expected.size &&
    actual.every((value) => expected.has(value))
  );
}

function assertSchemaParity(schema) {
  const failures = [];
  const require = (condition, message) => {
    if (!condition) failures.push(message);
  };
  const properties = schema?.properties ?? {};
  const subject = schema?.$defs?.subject ?? {};
  const publicKey = schema?.$defs?.solanaPublicKey ?? {};

  require(schema?.type === 'object', 'root type');
  require(schema?.additionalProperties === false, 'root additionalProperties');
  require(sameSet(schema?.required, REQUIRED_PAYLOAD_KEYS), 'root required fields');
  require(sameSet(Object.keys(properties), CANONICAL_PAYLOAD_KEYS), 'root property set');
  require(properties.type?.const === RECORD_TYPE, 'record type');
  require(properties.schema?.type === 'string', 'schema type');
  require(properties.schema?.minLength === 3, 'schema minLength');
  require(properties.schema?.maxLength === 128, 'schema maxLength');
  require(properties.schema?.pattern === PROFILE_SCHEMA_PATTERN, 'schema pattern');
  require(properties.subject?.$ref === '#/$defs/subject', 'subject reference');
  require(properties.validator?.$ref === '#/$defs/solanaPublicKey', 'validator reference');
  require(properties.hashAlg?.type === 'string', 'hashAlg type');
  require(properties.hashAlg?.minLength === 1, 'hashAlg minLength');
  require(properties.hashAlg?.maxLength === 64, 'hashAlg maxLength');
  require(properties.hashAlg?.pattern === TOKEN_SCHEMA_PATTERN, 'hashAlg pattern');
  require(properties.responseHash?.type === 'string', 'responseHash type');
  require(properties.responseHash?.pattern === HASH_PATTERN.source, 'responseHash pattern');
  require(properties.tag?.type === 'string', 'tag type');
  require(properties.tag?.minLength === 1, 'tag minLength');
  require(properties.tag?.maxLength === 64, 'tag maxLength');
  require(properties.tag?.pattern === TOKEN_SCHEMA_PATTERN, 'tag pattern');
  require(properties.recordedAt?.type === 'integer', 'recordedAt type');
  require(properties.recordedAt?.minimum === 0, 'recordedAt minimum');
  require(properties.recordedAt?.maximum === Number.MAX_SAFE_INTEGER, 'recordedAt maximum');
  require(properties.extensions?.type === 'object', 'extensions type');
  require(properties.extensions?.propertyNames?.pattern ===
    NAMESPACE_PATTERN.source, 'extension namespace pattern');
  require(properties.extensions?.additionalProperties === true, 'extensions additionalProperties');

  require(subject.type === 'object', 'subject type');
  require(subject.additionalProperties === false, 'subject additionalProperties');
  require(sameSet(subject.required, REQUIRED_SUBJECT_KEYS), 'subject required fields');
  require(sameSet(
    Object.keys(subject.properties ?? {}),
    CANONICAL_SUBJECT_KEYS,
  ), 'subject property set');
  for (const key of CANONICAL_SUBJECT_KEYS) {
    require(subject.properties?.[key]?.$ref ===
      '#/$defs/solanaPublicKey', `subject.${key} reference`);
  }

  require(publicKey.type === 'string', 'public key type');
  require(publicKey.minLength === 32, 'public key minLength');
  require(publicKey.maxLength === 44, 'public key maxLength');
  require(publicKey.pattern === PUBLIC_KEY_PATTERN.source, 'public key pattern');

  if (failures.length > 0) {
    throw new Error(`schema/verifier contract mismatch: ${failures.join(', ')}`);
  }
}

function validatePayload(
  payload,
  expectedValidator,
  expectedSchema,
  expectedRegistryProgram,
  supportedHashAlgorithms,
) {
  const reasons = [];

  if (!isObject(payload)) {
    return ['AppData payload is not an object'];
  }
  for (const key of Object.keys(payload)) {
    if (!PAYLOAD_KEYS.has(key)) reasons.push(`unknown top-level field ${key}`);
  }
  for (const [snake, camel] of [
    ['hash_alg', 'hashAlg'],
    ['response_hash', 'responseHash'],
    ['recorded_at', 'recordedAt'],
  ]) {
    if (hasOwn(payload, snake) && hasOwn(payload, camel)) {
      reasons.push(`ambiguous duplicate aliases ${snake} and ${camel}`);
    }
  }
  if (payload.type !== RECORD_TYPE) {
    reasons.push(`type is not ${RECORD_TYPE}`);
  }
  if (payload.schema !== expectedSchema) {
    reasons.push(`schema is not ${expectedSchema}`);
  }
  if (typeof payload.schema !== 'string' || !PROFILE_PATTERN.test(payload.schema)) {
    reasons.push('schema is not a valid profile identifier');
  }
  if (payload.validator !== expectedValidator) {
    reasons.push('validator does not match the pinned data authority');
  }
  if (!isPublicKey(payload.validator)) {
    reasons.push('validator is not a 32-byte Solana public key');
  }

  const subject = payload.subject;
  if (!isObject(subject)) {
    reasons.push('subject is not an object');
  } else {
    for (const key of Object.keys(subject)) {
      if (!SUBJECT_KEYS.has(key)) reasons.push(`unknown subject field ${key}`);
    }
    if (hasOwn(subject, 'registry_program') && hasOwn(subject, 'registryProgram')) {
      reasons.push('ambiguous duplicate aliases registry_program and registryProgram');
    }
    const registryProgram = subject.registry_program ?? subject.registryProgram;
    if (!isPublicKey(registryProgram)) {
      reasons.push('subject.registryProgram is not a 32-byte Solana public key');
    } else if (registryProgram !== expectedRegistryProgram) {
      reasons.push('subject.registryProgram does not match the pinned registry program');
    }
    if (!isPublicKey(subject.asset)) {
      reasons.push('subject.asset is not a 32-byte Solana public key');
    }
    if (subject.registration !== undefined && !isPublicKey(subject.registration)) {
      reasons.push('subject.registration is not a 32-byte Solana public key');
    }
  }

  const hashAlg = payloadField(payload, 'hash_alg', 'hashAlg');
  if (typeof hashAlg !== 'string' || !TOKEN_PATTERN.test(hashAlg)) {
    reasons.push('hashAlg is not a valid algorithm identifier');
  } else if (!supportedHashAlgorithms.includes(hashAlg)) {
    reasons.push(`unsupported hashAlg ${hashAlg}`);
  }
  const responseHash = payloadField(payload, 'response_hash', 'responseHash');
  if (typeof responseHash !== 'string' || !HASH_PATTERN.test(responseHash)) {
    reasons.push('responseHash is not 64 lowercase hex characters');
  }
  const recordedAt = payloadField(payload, 'recorded_at', 'recordedAt');
  if (!Number.isSafeInteger(recordedAt) || recordedAt < 0) {
    reasons.push('recordedAt is not a non-negative safe integer');
  }
  if (
    payload.tag !== undefined &&
    (typeof payload.tag !== 'string' || !TOKEN_PATTERN.test(payload.tag))
  ) {
    reasons.push('tag is not a valid token');
  }
  if (payload.extensions !== undefined) {
    if (!isObject(payload.extensions)) {
      reasons.push('extensions is not an object');
    } else {
      for (const namespace of Object.keys(payload.extensions)) {
        if (!NAMESPACE_PATTERN.test(namespace)) {
          reasons.push(`invalid extension namespace ${namespace}`);
        }
      }
    }
  }

  return reasons;
}

export function verifyDasRecord(
  asset,
  { expectedValidator, expectedSchema, expectedRegistryProgram, supportedHashAlgorithms },
) {
  const reasons = [];

  if (!isPublicKey(expectedValidator)) {
    reasons.push('expectedValidator is not a 32-byte Solana public key');
  }
  if (typeof expectedSchema !== 'string' || !PROFILE_PATTERN.test(expectedSchema)) {
    reasons.push('expectedSchema is not a valid profile identifier');
  }
  if (!isPublicKey(expectedRegistryProgram)) {
    reasons.push('expectedRegistryProgram is not a 32-byte Solana public key');
  }
  if (
    !Array.isArray(supportedHashAlgorithms) ||
    supportedHashAlgorithms.length === 0 ||
    supportedHashAlgorithms.some(
      (algorithm) => typeof algorithm !== 'string' || !TOKEN_PATTERN.test(algorithm),
    )
  ) {
    reasons.push('supportedHashAlgorithms is not a non-empty algorithm list');
  }
  if (asset?.interface !== 'MplCoreAsset') {
    reasons.push('asset interface is not MplCoreAsset');
  }

  const plugins = Array.isArray(asset?.external_plugins) ? asset.external_plugins : [];
  const candidates = [];
  const selectionReasons = [];
  for (const plugin of plugins) {
    const data = plugin?.data;
    if (
      plugin?.type === 'AppData' &&
      data?.type === RECORD_TYPE &&
      data?.schema === expectedSchema
    ) {
      const inspection = inspectAdapter(plugin, expectedValidator);
      selectionReasons.push(...inspection.reasons);
      if (inspection.matches) candidates.push(plugin);
    }
  }
  reasons.push(...selectionReasons);

  if (candidates.length === 0) {
    if (selectionReasons.length === 0) {
      reasons.push(
        'no AppData adapter matches the pinned Address data authority, Json encoding, record type, and profile schema',
      );
    }
  } else if (candidates.length > 1) {
    reasons.push('multiple AppData adapters match; record is ambiguous');
  } else {
    reasons.push(
      ...validatePayload(
        candidates[0].data,
        expectedValidator,
        expectedSchema,
        expectedRegistryProgram,
        Array.isArray(supportedHashAlgorithms) ? supportedHashAlgorithms : [],
      ),
    );
  }

  return {
    asset: asset?.id ?? null,
    valid: reasons.length === 0,
    reasons,
  };
}

async function readJson(path) {
  return JSON.parse(await readFile(path, 'utf8'));
}

async function runVectors() {
  const directory = new URL('.', import.meta.url);
  const schema = await readJson(
    fileURLToPath(new URL('agent-validation-record-v1.schema.json', directory)),
  );
  const fixtures = await readJson(
    fileURLToPath(new URL('agent-validation-record-v1.vectors.json', directory)),
  );

  assertSchemaParity(schema);
  if (fixtures.recordType !== RECORD_TYPE) {
    throw new Error('fixture record type does not match verifier record type');
  }
  if (fixtures.fixtureKind !== FIXTURE_KIND) {
    throw new Error('fixtures are not marked as synthetic off-chain data');
  }

  let failures = 0;
  for (const vector of fixtures.vectors) {
    const options = {
      expectedValidator: fixtures.expectedValidator,
      expectedSchema: fixtures.expectedSchema,
      expectedRegistryProgram: fixtures.expectedRegistryProgram,
      supportedHashAlgorithms: fixtures.supportedHashAlgorithms,
      ...vector.options,
    };
    const verdict = verifyDasRecord(vector.asset, options);
    const reasonMatches =
      vector.reasonIncludes === undefined ||
      verdict.reasons.some((reason) => reason.includes(vector.reasonIncludes));
    const passed = verdict.valid === vector.valid && reasonMatches;

    process.stdout.write(`${passed ? 'ok' : 'not ok'} - ${vector.name}\n`);
    if (!passed) {
      failures += 1;
      process.stdout.write(`  expected valid=${vector.valid}, got ${JSON.stringify(verdict)}\n`);
    }
  }

  if (failures > 0) {
    throw new Error(`${failures} validation vector(s) failed`);
  }
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length === 0 || args[0] === '--test') {
    await runVectors();
    return;
  }

  const assetIndex = args.indexOf('--asset');
  const validatorIndex = args.indexOf('--validator');
  const schemaIndex = args.indexOf('--schema');
  const registryProgramIndex = args.indexOf('--registry-program');
  const hashAlgorithmIndex = args.indexOf('--hash-alg');
  if (
    assetIndex < 0 ||
    validatorIndex < 0 ||
    schemaIndex < 0 ||
    registryProgramIndex < 0 ||
    hashAlgorithmIndex < 0 ||
    !args[assetIndex + 1] ||
    !args[validatorIndex + 1] ||
    !args[schemaIndex + 1] ||
    !args[registryProgramIndex + 1] ||
    !args[hashAlgorithmIndex + 1]
  ) {
    throw new Error(
      'usage: verify-agent-validation-record.mjs --test | --asset <das-json> --validator <pubkey> --schema <profile> --registry-program <pubkey> --hash-alg <algorithm>',
    );
  }

  const verdict = verifyDasRecord(await readJson(args[assetIndex + 1]), {
    expectedValidator: args[validatorIndex + 1],
    expectedSchema: args[schemaIndex + 1],
    expectedRegistryProgram: args[registryProgramIndex + 1],
    supportedHashAlgorithms: [args[hashAlgorithmIndex + 1]],
  });
  process.stdout.write(`${JSON.stringify(verdict, null, 2)}\n`);
  if (!verdict.valid) process.exitCode = 1;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  });
}
