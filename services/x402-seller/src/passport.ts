// Provider-backed observations about an MPL Core asset and its 014 Registry
// binding. These checks do not prove identity, capability, delivery, or claim
// truth. Registration URIs are returned as data and are never fetched here.

import { PublicKey } from '@solana/web3.js';

const AGENT_IDENTITY_PROGRAM = '1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p';
const COVENANT_DATA_AUTHORITY = '96GsGo69kVfPZffudCexfnsSi5EuhAyd278MuJPwzGdu';
const COVENANT_COLLECTION = 'Duqs6dq1wXPcRqJVUCgSZxrkLRdg3oBfZ3ViER1kt6gC';
const ATTESTATION_SCHEMA = 'covenant.audit-root.appdata.v1';

interface AttestationPayload {
  schema: string;
  rootHashHex: string;
  releaseTarget: string;
  releaseSubject: string;
  releaseScope: string;
  recordedAt: number;
}

export interface AgentRecord {
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
    registered: boolean | null;
    identityPlugin: boolean;
    registrationUri: string | null;
  };
  attestation: {
    payload: AttestationPayload;
    authority: string | null;
    matchesConfiguredAuthority: boolean;
    evidenceSource: 'configured_das';
  } | null;
  doc: {
    name: string;
    image: string | null;
    description: string | null;
    listsThisAsset: boolean;
  } | null;
  limitations: string[];
}

export interface Result {
  status: number;
  body: AgentRecord | { error: string };
}

const obj = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;

const objects = (value: unknown): Record<string, unknown>[] =>
  Array.isArray(value)
    ? value.map(obj).filter((item): item is Record<string, unknown> => item !== null)
    : [];

const str = (value: unknown): string | null => (typeof value === 'string' ? value : null);

function hasRegistryBinding(info: unknown, asset: PublicKey): boolean {
  const value = obj(obj(info)?.['value']);
  if (!value || value['owner'] !== AGENT_IDENTITY_PROGRAM) return false;
  const data = value['data'];
  if (!Array.isArray(data) || typeof data[0] !== 'string' || data[1] !== 'base64') return false;
  const bytes = Buffer.from(data[0], 'base64');
  return bytes.length === 40 && bytes.subarray(8).equals(asset.toBuffer());
}

function writeAuthority(plugin: Record<string, unknown>): string | null {
  const config = obj(plugin['adapter_config']) ?? obj(plugin['adapterConfig']);
  const authority =
    obj(config?.['data_authority']) ??
    obj(config?.['dataAuthority']) ??
    obj(plugin['data_authority']) ??
    obj(plugin['dataAuthority']);
  return str(authority?.['address']);
}

async function rpc(
  url: string,
  timeoutMs: number,
  method: string,
  params: unknown,
): Promise<unknown> {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!res.ok) throw new Error(`rpc ${method} http ${res.status}`);
  const body = (await res.json()) as { result?: unknown; error?: { message?: string } };
  if (body.error) throw new Error(`rpc ${method}: ${body.error.message ?? 'error'}`);
  return body.result;
}

// Helius indexes AppData with snake_cased keys; on-chain bytes are camelCase.
function normalizePayload(raw: Record<string, unknown>): AttestationPayload | null {
  const pick = (camel: string, snake: string): unknown => raw[camel] ?? raw[snake];
  const schema = pick('schema', 'schema');
  const root = pick('rootHashHex', 'root_hash_hex');
  if (typeof schema !== 'string' || typeof root !== 'string') return null;
  return {
    schema,
    rootHashHex: root,
    releaseTarget: String(pick('releaseTarget', 'release_target') ?? ''),
    releaseSubject: String(pick('releaseSubject', 'release_subject') ?? ''),
    releaseScope: String(pick('releaseScope', 'release_scope') ?? ''),
    recordedAt: Number(pick('recordedAt', 'recorded_at') ?? 0),
  };
}

export async function getPassport(
  rpcUrl: string,
  timeoutMs: number,
  asset: string,
): Promise<Result> {
  let assetPk: PublicKey;
  try {
    assetPk = new PublicKey(asset);
  } catch {
    return { status: 400, body: { error: 'not a valid Solana address' } };
  }

  let das: Record<string, unknown>;
  try {
    das = (await rpc(rpcUrl, timeoutMs, 'getAsset', { id: assetPk.toBase58() })) as Record<
      string,
      unknown
    >;
  } catch {
    return {
      status: 502,
      body: { error: 'asset lookup failed — DAS endpoint unavailable or asset not found' },
    };
  }
  if (!das || das['interface'] !== 'MplCoreAsset') {
    return {
      status: 404,
      body: { error: 'not an MPL Core asset — the 014 Registry binds Core assets only' },
    };
  }

  const content = obj(das['content']) ?? {};
  const metadata = obj(content['metadata']) ?? {};
  const ownership = obj(das['ownership']) ?? {};
  const authorities = objects(das['authorities']);
  const grouping = objects(das['grouping']);
  const externalPlugins = objects(das['external_plugins']);

  const collection =
    (grouping.find((g) => g['group_key'] === 'collection')?.['group_value'] as
      | string
      | undefined) ?? null;

  const [pda] = PublicKey.findProgramAddressSync(
    [Buffer.from('agent_identity'), assetPk.toBytes()],
    new PublicKey(AGENT_IDENTITY_PROGRAM),
  );
  let registered: boolean | null = null;
  try {
    const info = await rpc(rpcUrl, timeoutMs, 'getAccountInfo', [
      pda.toBase58(),
      { encoding: 'base64' },
    ]);
    registered = hasRegistryBinding(info, assetPk);
  } catch {
    registered = null;
  }

  const identityPlugin = externalPlugins.find((p) => p['type'] === 'AgentIdentity');
  const registrationUri = str(obj(identityPlugin?.['adapter_config'])?.['uri']);

  const appData = externalPlugins.find((p) => p['type'] === 'AppData');
  let attestation: AgentRecord['attestation'] = null;
  if (appData) {
    const payload = normalizePayload(obj(appData['data']) ?? {});
    if (payload && payload.schema === ATTESTATION_SCHEMA) {
      const authority = writeAuthority(appData);
      attestation = {
        payload,
        authority,
        matchesConfiguredAuthority: authority === COVENANT_DATA_AUTHORITY,
        evidenceSource: 'configured_das',
      };
    }
  }

  const jsonUri = str(content['json_uri']) ?? '';

  return {
    status: 200,
    body: {
      asset: {
        id: assetPk.toBase58(),
        name: String(metadata['name'] ?? ''),
        uri: jsonUri,
        owner: String(ownership['owner'] ?? ''),
        authority: String(authorities[0]?.['address'] ?? ''),
        inCovenantCollection: collection === COVENANT_COLLECTION,
        collection,
        burnt: das['burnt'] === true,
      },
      registry: {
        pda: pda.toBase58(),
        registered,
        identityPlugin: Boolean(identityPlugin),
        registrationUri,
      },
      attestation,
      doc: null,
      limitations: [
        'Configured RPC and DAS responses are provider observations, not account proofs.',
        'Registration and AppData presence do not prove identity, capability, delivery, reputation, or claim truth.',
        'Untrusted registration and metadata URIs are not fetched by this service.',
      ],
    },
  };
}
