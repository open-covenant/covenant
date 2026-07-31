// Agent passport: independent on-chain facts about an MPL Core agent asset.
// Reads are limited to the configured Solana RPC. On-chain document URIs are
// returned as data and are never fetched by this service.

import {
  address,
  getAddressEncoder,
  getProgramDerivedAddress,
  isAddress,
} from '@solana/kit';

export const AGENT_IDENTITY_PROGRAM = '1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p';
export const COVENANT_DATA_AUTHORITY = 'DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK';
export const COVENANT_COLLECTION = 'Duqs6dq1wXPcRqJVUCgSZxrkLRdg3oBfZ3ViER1kt6gC';
export const VALIDATION_RECORD_TYPE = 'mpl.agent.validation-record.v1';
export const COVENANT_VALIDATION_SCHEMA = 'org.opencovenant.audit-chain.v1';
export const COVENANT_VALIDATION_HASH_ALG = 'sha256-chain-v1';
export const LEGACY_RECORD_TYPE = 'https://eips.ethereum.org/EIPS/eip-8004#validation-v1';
export const LEGACY_RECORD_SCHEMA = 'covenant.audit-root.appdata.v2';
export const LEGACY_HASH_ALG = 'sha256-merkle';
export const LEGACY_RECORD_ASSET = '4A2fdNqmPiQrv3iYv6WY2mQ9eSQuBERhdeg4vk7G8vGG';
export const LEGACY_SUBJECT_ASSET = '4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc';

export interface Verdict {
  asset: string | null;
  recordAuthentic: boolean;
  evidenceVerified: null;
  policyAccepted: null;
  subjectRegistrationVerified: null;
  profile: string;
  legacy: boolean;
  subjectAsset: string | null;
  authority: string | null;
  responseHash: string | null;
  recordedAt: number | null;
  reasons: string[];
}

export interface ValidationRecords {
  count: number;
  latestObserved: Verdict | null;
  coverage: {
    method: 'validator-owned-assets';
    owner: string;
    pagesScanned: number;
    assetsScanned: number;
    truncated: boolean;
    complete: false;
  };
}

export interface AgentPassport {
  asset: {
    id: string;
    name: string;
    uri: string;
    owner: string;
    authority: string;
    inCovenantCollection: boolean;
    collection: string | null;
    burnt: boolean;
  };
  registry: {
    pda: string;
    accountOwnerMatches: boolean | null;
    identityPluginIndexed: boolean;
    registrationUri: string | null;
  };
  attestation: Verdict | null;
  validationRecords: ValidationRecords | null;
  legacyAttestation: Verdict | null;
}

export type Result =
  | {status: 200; body: AgentPassport}
  | {status: 400 | 404 | 502; body: {error: string}};

export type Rpc = (method: string, params: unknown) => Promise<unknown>;

const str = (value: unknown): string | undefined => (typeof value === 'string' ? value : undefined);
const obj = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
const field = (data: Record<string, unknown>, snake: string, camel: string): string | undefined =>
  str(data[snake]) ?? str(data[camel]);
const isHex64 = (value: string): boolean => /^[0-9a-f]{64}$/.test(value);
const isToken = (value: string): boolean => /^[a-z0-9][a-z0-9._-]{0,63}$/.test(value);
const isNamespace = (value: string): boolean => /^[a-z0-9][a-z0-9.-]*$/.test(value);

const PAYLOAD_KEYS = new Set([
  'type',
  'schema',
  'subject',
  'validator',
  'hashAlg',
  'hash_alg',
  'responseHash',
  'response_hash',
  'tag',
  'recordedAt',
  'recorded_at',
  'extensions',
]);
const SUBJECT_KEYS = new Set(['registryProgram', 'registry_program', 'asset', 'registration']);

function records(value: unknown): Array<Record<string, unknown>> {
  return Array.isArray(value) ? value.map(obj).filter((item): item is Record<string, unknown> => Boolean(item)) : [];
}

function adapterConfig(plugin: Record<string, unknown>): Record<string, unknown> | undefined {
  if (hasBothAliases(plugin, 'adapter_config', 'adapterConfig')) return undefined;
  return obj(plugin['adapter_config']) ?? obj(plugin['adapterConfig']);
}

function writeAuthority(plugin: Record<string, unknown>): string | null {
  const config = adapterConfig(plugin);
  if (!config || hasBothAliases(config, 'data_authority', 'dataAuthority')) return null;
  const authority = obj(config['data_authority']) ?? obj(config['dataAuthority']);
  if (authority?.['type'] !== 'Address') return null;
  return str(authority['address']) ?? null;
}

function adapterSchema(plugin: Record<string, unknown>): string | null {
  const config = adapterConfig(plugin);
  return str(config?.['schema']) ?? null;
}

function hasBothAliases(
  data: Record<string, unknown>,
  snake: string,
  camel: string,
): boolean {
  return data[snake] !== undefined && data[camel] !== undefined;
}

function verdict(
  asset: string | null,
  authority: string | null,
  profile: string,
  legacy: boolean,
  data: Record<string, unknown>,
  reasons: string[],
): Verdict {
  const subject = obj(data['subject']) ?? {};
  const recordedRaw = data['recorded_at'] ?? data['recordedAt'];
  return {
    asset,
    recordAuthentic: reasons.length === 0,
    evidenceVerified: null,
    policyAccepted: null,
    subjectRegistrationVerified: null,
    profile,
    legacy,
    subjectAsset: field(subject, 'asset', 'asset') ?? null,
    authority,
    responseHash: field(data, 'response_hash', 'responseHash') ?? null,
    recordedAt: Number.isSafeInteger(recordedRaw) ? (recordedRaw as number) : null,
    reasons,
  };
}

function matchingPlugins(
  asset: Record<string, unknown>,
  authority: string,
  type: string,
  schema: string,
): Array<Record<string, unknown>> {
  return records(asset['external_plugins']).filter((plugin) => {
    const data = obj(plugin['data']);
    return (
      plugin['type'] === 'AppData' &&
      adapterSchema(plugin) === 'Json' &&
      writeAuthority(plugin) === authority &&
      data?.['type'] === type &&
      data?.['schema'] === schema
    );
  });
}

async function registrationPda(asset: string): Promise<string> {
  const [pda] = await getProgramDerivedAddress({
    programAddress: address(AGENT_IDENTITY_PROGRAM),
    seeds: [Buffer.from('agent_identity'), getAddressEncoder().encode(address(asset))],
  });
  return pda;
}

export async function verifyValidationRecord(
  asset: Record<string, unknown>,
  authority: string,
  expectedSubject?: string,
): Promise<Verdict> {
  const id = str(asset['id']) ?? null;
  if (asset['interface'] !== 'MplCoreAsset') {
    return verdict(id, null, COVENANT_VALIDATION_SCHEMA, false, {}, [
      'record asset interface is not MplCoreAsset',
    ]);
  }
  const plugins = matchingPlugins(
    asset,
    authority,
    VALIDATION_RECORD_TYPE,
    COVENANT_VALIDATION_SCHEMA,
  );
  if (plugins.length !== 1) {
    const reason =
      plugins.length === 0
        ? 'no AppData adapter matches the pinned authority, Json encoding, record type, and profile'
        : 'multiple AppData adapters match; record is ambiguous';
    return verdict(id, null, COVENANT_VALIDATION_SCHEMA, false, {}, [reason]);
  }

  const plugin = plugins[0];
  const data = obj(plugin['data']) ?? {};
  const dataAuthority = writeAuthority(plugin);
  const reasons: string[] = [];

  for (const key of Object.keys(data)) {
    if (!PAYLOAD_KEYS.has(key)) reasons.push(`unknown top-level field ${key}`);
  }
  if (data['type'] !== VALIDATION_RECORD_TYPE) {
    reasons.push(`type is not ${VALIDATION_RECORD_TYPE}`);
  }
  if (data['schema'] !== COVENANT_VALIDATION_SCHEMA) {
    reasons.push(`schema is not ${COVENANT_VALIDATION_SCHEMA}`);
  }
  if (data['validator'] !== authority) {
    reasons.push('validator does not match the pinned data authority');
  }
  if (!isAddress(str(data['validator']) ?? '')) {
    reasons.push('validator is not a 32-byte Solana public key');
  }

  const subject = obj(data['subject']);
  if (!subject) {
    reasons.push('subject is not an object');
  } else {
    for (const key of Object.keys(subject)) {
      if (!SUBJECT_KEYS.has(key)) reasons.push(`unknown subject field ${key}`);
    }
    if (hasBothAliases(subject, 'registry_program', 'registryProgram')) {
      reasons.push('subject registryProgram aliases are ambiguous');
    }
    const registryProgram = field(subject, 'registry_program', 'registryProgram');
    if (registryProgram !== AGENT_IDENTITY_PROGRAM) {
      reasons.push(`subject.registryProgram is not ${AGENT_IDENTITY_PROGRAM}`);
    }
    const subjectAsset = str(subject['asset']);
    if (!subjectAsset || !isAddress(subjectAsset)) {
      reasons.push('subject.asset is not a 32-byte Solana public key');
    } else if (expectedSubject !== undefined && subjectAsset !== expectedSubject) {
      reasons.push('subject.asset does not match the requested agent');
    }
    const registration = str(subject['registration']);
    if (registration !== undefined) {
      if (!isAddress(registration)) {
        reasons.push('subject.registration is not a 32-byte Solana public key');
      } else if (subjectAsset && isAddress(subjectAsset)) {
        const expected = await registrationPda(subjectAsset);
        if (registration !== expected) {
          reasons.push('subject.registration does not match the derived AgentIdentityV1 PDA');
        }
      }
    }
  }

  if (hasBothAliases(data, 'hash_alg', 'hashAlg')) {
    reasons.push('hashAlg aliases are ambiguous');
  }
  const hashAlg = field(data, 'hash_alg', 'hashAlg');
  if (hashAlg !== COVENANT_VALIDATION_HASH_ALG) {
    reasons.push(`hashAlg is not ${COVENANT_VALIDATION_HASH_ALG}`);
  }
  if (hasBothAliases(data, 'response_hash', 'responseHash')) {
    reasons.push('responseHash aliases are ambiguous');
  }
  const responseHash = field(data, 'response_hash', 'responseHash');
  if (!responseHash || !isHex64(responseHash)) {
    reasons.push('responseHash is not 64 lowercase hex characters');
  }
  if (hasBothAliases(data, 'recorded_at', 'recordedAt')) {
    reasons.push('recordedAt aliases are ambiguous');
  }
  const recordedRaw = data['recorded_at'] ?? data['recordedAt'];
  if (!Number.isSafeInteger(recordedRaw) || (recordedRaw as number) < 0) {
    reasons.push('recordedAt is not a non-negative safe integer');
  }
  const tag = str(data['tag']);
  if (tag !== undefined && !isToken(tag)) reasons.push('tag is not a valid token');
  const extensions = data['extensions'];
  if (extensions !== undefined) {
    const extensionObject = obj(extensions);
    if (!extensionObject) {
      reasons.push('extensions is not an object');
    } else {
      for (const namespace of Object.keys(extensionObject)) {
        if (!isNamespace(namespace)) reasons.push(`invalid extension namespace ${namespace}`);
      }
    }
  }

  return verdict(id, dataAuthority, COVENANT_VALIDATION_SCHEMA, false, data, reasons);
}

export function verifyLegacyAttestation(
  asset: Record<string, unknown>,
  authority: string,
): Verdict {
  const id = str(asset['id']) ?? null;
  if (asset['interface'] !== 'MplCoreAsset') {
    return verdict(id, null, LEGACY_RECORD_SCHEMA, true, {}, [
      'legacy record asset interface is not MplCoreAsset',
    ]);
  }
  const plugins = matchingPlugins(asset, authority, LEGACY_RECORD_TYPE, LEGACY_RECORD_SCHEMA);
  if (plugins.length !== 1) {
    const reason =
      plugins.length === 0
        ? 'no AppData adapter matches the explicit legacy Covenant profile'
        : 'multiple legacy AppData adapters match; record is ambiguous';
    return verdict(id, null, LEGACY_RECORD_SCHEMA, true, {}, [reason]);
  }

  const plugin = plugins[0];
  const data = obj(plugin['data']) ?? {};
  const reasons: string[] = [];
  if (field(data, 'hash_alg', 'hashAlg') !== LEGACY_HASH_ALG) {
    reasons.push(`legacy hashAlg is not ${LEGACY_HASH_ALG}`);
  }
  if (data['validator'] !== authority) {
    reasons.push('legacy validator does not match the pinned data authority');
  }
  const subject = obj(data['subject']) ?? {};
  const subjectAsset = str(subject['asset']);
  if (!subjectAsset || !isAddress(subjectAsset)) {
    reasons.push('legacy subject.asset is not a 32-byte Solana public key');
  }
  const responseHash = field(data, 'response_hash', 'responseHash');
  if (!responseHash || !isHex64(responseHash)) {
    reasons.push('legacy responseHash is not 64 lowercase hex characters');
  }
  const recordedRaw = data['recorded_at'] ?? data['recordedAt'];
  if (!Number.isSafeInteger(recordedRaw) || (recordedRaw as number) < 0) {
    reasons.push('legacy recordedAt is not a non-negative safe integer');
  }
  return verdict(id, writeAuthority(plugin), LEGACY_RECORD_SCHEMA, true, data, reasons);
}

export async function findValidationRecords(
  rpc: Rpc,
  agent: string,
  authority: string,
): Promise<ValidationRecords> {
  const maxPages = 5;
  const verified: Verdict[] = [];
  let truncated = false;
  let pagesScanned = 0;
  let assetsScanned = 0;

  for (let page = 1; page <= maxPages; page += 1) {
    const response = obj(
      await rpc('getAssetsByOwner', {
        ownerAddress: authority,
        page,
        limit: 1000,
      }),
    );
    if (!response || !Array.isArray(response['items'])) {
      throw new Error('getAssetsByOwner returned no items array');
    }
    const rawItems = response['items'];
    pagesScanned += 1;
    assetsScanned += rawItems.length;

    for (const item of rawItems) {
      const asset = obj(item);
      if (!asset) continue;
      const current = await verifyValidationRecord(asset, authority);
      if (current.recordAuthentic && current.subjectAsset === agent) verified.push(current);
    }

    if (rawItems.length === 0 || rawItems.length < 1000) break;
    if (page === maxPages) truncated = true;
  }

  const latest = verified.reduce<Verdict | null>(
    (current, verdict) =>
      current && (current.recordedAt ?? 0) >= (verdict.recordedAt ?? 0) ? current : verdict,
    null,
  );

  return {
    count: verified.length,
    latestObserved: latest,
    coverage: {
      method: 'validator-owned-assets',
      owner: authority,
      pagesScanned,
      assetsScanned,
      truncated,
      complete: false,
    },
  };
}

async function jsonRpc(url: string, timeoutMs: number, method: string, params: unknown): Promise<unknown> {
  const response = await fetch(url, {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({jsonrpc: '2.0', id: 1, method, params}),
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!response.ok) throw new Error(`rpc ${method} http ${response.status}`);

  const body = obj(await response.json());
  const error = obj(body?.['error']);
  if (error) throw new Error(`rpc ${method}: ${str(error['message']) ?? 'error'}`);
  return body?.['result'];
}

export async function getPassportWithRpc(rpc: Rpc, asset: string): Promise<Result> {
  if (!isAddress(asset)) {
    return {status: 400, body: {error: 'not a valid Solana address'}};
  }
  const assetAddress = address(asset);

  let das: Record<string, unknown> | undefined;
  try {
    das = obj(await rpc('getAsset', {id: assetAddress}));
  } catch {
    return {status: 502, body: {error: 'asset lookup failed — DAS endpoint unavailable or asset not found'}};
  }
  if (!das || das['interface'] !== 'MplCoreAsset') {
    return {status: 404, body: {error: 'not an MPL Core asset — the 014 Registry binds Core assets only'}};
  }
  if (str(das['id']) !== assetAddress) {
    return {status: 502, body: {error: 'asset lookup returned a different asset'}};
  }

  const content = obj(das['content']) ?? {};
  const metadata = obj(content['metadata']) ?? {};
  const ownership = obj(das['ownership']) ?? {};
  const authorities = records(das['authorities']);
  const grouping = records(das['grouping']);
  const externalPlugins = records(das['external_plugins']);

  const collectionGroup = grouping.find(
    (group) => field(group, 'group_key', 'groupKey') === 'collection',
  );
  const collection = collectionGroup ? field(collectionGroup, 'group_value', 'groupValue') ?? null : null;

  const pda = await registrationPda(assetAddress);

  const identityPlugin = externalPlugins.find((plugin) => plugin['type'] === 'AgentIdentity');
  const identityConfig = obj(identityPlugin?.['adapter_config']) ?? obj(identityPlugin?.['adapterConfig']);
  const registrationUri = str(identityConfig?.['uri']) ?? null;
  const hasDirectValidation = records(das['external_plugins']).some((plugin) => {
    const data = obj(plugin['data']);
    return (
      plugin['type'] === 'AppData' &&
      data?.['type'] === VALIDATION_RECORD_TYPE &&
      data?.['schema'] === COVENANT_VALIDATION_SCHEMA
    );
  });
  const attestation = hasDirectValidation
    ? await verifyValidationRecord(das, COVENANT_DATA_AUTHORITY, assetAddress)
    : null;

  const [accountOwnerMatches, validationRecords, legacyAttestation] = await Promise.all([
    rpc('getAccountInfo', [pda, {encoding: 'base64'}])
      .then((info) => {
        const response = obj(info);
        if (!response || !Object.hasOwn(response, 'value')) return null;
        const value = obj(response['value']);
        return value === undefined ? false : str(value['owner']) === AGENT_IDENTITY_PROGRAM;
      })
      .catch(() => null as boolean | null),
    findValidationRecords(rpc, assetAddress, COVENANT_DATA_AUTHORITY).catch(
      () => null as ValidationRecords | null,
    ),
    assetAddress === LEGACY_SUBJECT_ASSET
      ? rpc('getAsset', {id: LEGACY_RECORD_ASSET})
          .then((value) => {
            const legacy = obj(value);
            if (!legacy || str(legacy['id']) !== LEGACY_RECORD_ASSET) return null;
            const result = verifyLegacyAttestation(legacy, COVENANT_DATA_AUTHORITY);
            return result.recordAuthentic && result.subjectAsset === assetAddress ? result : null;
          })
          .catch(() => null as Verdict | null)
      : Promise.resolve(null),
  ]);

  return {
    status: 200,
    body: {
      asset: {
        id: assetAddress,
        name: str(metadata['name']) ?? '',
        uri: field(content, 'json_uri', 'jsonUri') ?? '',
        owner: str(ownership['owner']) ?? '',
        authority: str(authorities[0]?.['address']) ?? '',
        inCovenantCollection: collection === COVENANT_COLLECTION,
        collection,
        burnt: das['burnt'] === true,
      },
      registry: {
        pda,
        accountOwnerMatches,
        identityPluginIndexed: Boolean(identityPlugin),
        registrationUri,
      },
      attestation,
      validationRecords,
      legacyAttestation,
    },
  };
}

export async function getPassport(rpcUrl: string, timeoutMs: number, asset: string): Promise<Result> {
  return getPassportWithRpc((method, params) => jsonRpc(rpcUrl, timeoutMs, method, params), asset);
}
