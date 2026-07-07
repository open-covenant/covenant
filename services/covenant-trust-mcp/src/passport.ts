// Agent passport: three independent on-chain facts about an MPL Core agent
// asset — the asset itself (DAS getAsset), its 014 Registry binding, and the
// Covenant attestation AppData plus its write authority. Pure reads; no daemon,
// no local state. Ported from the landing /api/agents/[asset] route so the paid
// product and the public page never drift.

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
  registry: { pda: string; registered: boolean; identityPlugin: boolean; registrationUri: string | null };
  attestation: { payload: AttestationPayload; authority: string | null; covenantAuthored: boolean } | null;
  doc: { name: string; image: string | null; description: string | null; listsThisAsset: boolean } | null;
}

export interface Result {
  status: number;
  body: AgentPassport | { error: string };
}

async function rpc(url: string, timeoutMs: number, method: string, params: unknown): Promise<unknown> {
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

export async function getPassport(rpcUrl: string, timeoutMs: number, asset: string): Promise<Result> {
  let assetPk: PublicKey;
  try {
    assetPk = new PublicKey(asset);
  } catch {
    return { status: 400, body: { error: 'not a valid Solana address' } };
  }

  let das: Record<string, unknown>;
  try {
    das = (await rpc(rpcUrl, timeoutMs, 'getAsset', { id: assetPk.toBase58() })) as Record<string, unknown>;
  } catch {
    return { status: 502, body: { error: 'asset lookup failed — DAS endpoint unavailable or asset not found' } };
  }
  if (!das || das['interface'] !== 'MplCoreAsset') {
    return { status: 404, body: { error: 'not an MPL Core asset — the 014 Registry binds Core assets only' } };
  }

  const content = (das['content'] ?? {}) as Record<string, unknown>;
  const metadata = (content['metadata'] ?? {}) as Record<string, unknown>;
  const ownership = (das['ownership'] ?? {}) as Record<string, unknown>;
  const authorities = (das['authorities'] ?? []) as Array<Record<string, unknown>>;
  const grouping = (das['grouping'] ?? []) as Array<Record<string, unknown>>;
  const externalPlugins = (das['external_plugins'] ?? []) as Array<Record<string, unknown>>;

  const collection =
    (grouping.find((g) => g['group_key'] === 'collection')?.['group_value'] as string | undefined) ?? null;

  const [pda] = PublicKey.findProgramAddressSync(
    [Buffer.from('agent_identity'), assetPk.toBytes()],
    new PublicKey(AGENT_IDENTITY_PROGRAM),
  );
  let registered = false;
  try {
    const info = (await rpc(rpcUrl, timeoutMs, 'getAccountInfo', [pda.toBase58(), { encoding: 'base64' }])) as {
      value: { owner: string } | null;
    } | null;
    registered = info?.value?.owner === AGENT_IDENTITY_PROGRAM;
  } catch {
    // leave registered=false
  }

  const identityPlugin = externalPlugins.find((p) => p['type'] === 'AgentIdentity');
  const registrationUri =
    ((identityPlugin?.['adapter_config'] as Record<string, unknown> | undefined)?.['uri'] as string | undefined) ?? null;

  const appData = externalPlugins.find((p) => p['type'] === 'AppData');
  let attestation: AgentPassport['attestation'] = null;
  if (appData) {
    const payload = normalizePayload((appData['data'] ?? {}) as Record<string, unknown>);
    if (payload && payload.schema === ATTESTATION_SCHEMA) {
      const authority =
        ((appData['authority'] as Record<string, unknown> | undefined)?.['address'] as string | undefined) ?? null;
      attestation = { payload, authority, covenantAuthored: authority === COVENANT_DATA_AUTHORITY };
    }
  }

  const jsonUri = (content['json_uri'] as string | undefined) ?? '';
  const docUri = registrationUri ?? jsonUri;
  let doc: AgentPassport['doc'] = null;
  if (docUri.startsWith('https://')) {
    try {
      const res = await fetch(docUri, { signal: AbortSignal.timeout(4000) });
      if (res.ok) {
        const d = (await res.json()) as Record<string, unknown>;
        const registrations = (d['registrations'] ?? []) as Array<Record<string, unknown>>;
        doc = {
          name: String(d['name'] ?? ''),
          image: typeof d['image'] === 'string' ? d['image'] : null,
          description: typeof d['description'] === 'string' ? d['description'] : null,
          listsThisAsset: registrations.some((r) => r['agentId'] === assetPk.toBase58()),
        };
      }
    } catch {
      // doc unreachable
    }
  }

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
      registry: { pda: pda.toBase58(), registered, identityPlugin: Boolean(identityPlugin), registrationUri },
      attestation,
      doc,
    },
  };
}
