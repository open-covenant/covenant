import assert from 'node:assert/strict';
import {describe, it} from 'node:test';
import {
  AGENT_IDENTITY_PROGRAM,
  COVENANT_COLLECTION,
  COVENANT_DATA_AUTHORITY,
  COVENANT_VALIDATION_HASH_ALG,
  COVENANT_VALIDATION_SCHEMA,
  LEGACY_HASH_ALG,
  LEGACY_RECORD_ASSET,
  LEGACY_RECORD_SCHEMA,
  LEGACY_RECORD_TYPE,
  VALIDATION_RECORD_TYPE,
  findValidationRecords,
  getPassportWithRpc,
  verifyLegacyAttestation,
  verifyValidationRecord,
  type Rpc,
} from './passport.js';

const AGENT = '4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc';
const OTHER_AGENT = '11111111111111111111111111111111';
const RESPONSE_HASH = 'ab'.repeat(32);

interface ValidationOptions {
  id?: string;
  subject?: string;
  recordedAt?: number;
  dataAuthority?: string;
  topLevelAuthority?: string;
  adapterSchema?: string;
  data?: Record<string, unknown>;
}

function validationAsset(options: ValidationOptions = {}): Record<string, unknown> {
  return {
    id: options.id ?? 'validation-record',
    interface: 'MplCoreAsset',
    external_plugins: [
      {
        type: 'AppData',
        authority: {address: options.topLevelAuthority ?? OTHER_AGENT},
        adapter_config: {
          schema: options.adapterSchema ?? 'Json',
          data_authority: {
            type: 'Address',
            address: options.dataAuthority ?? COVENANT_DATA_AUTHORITY,
          },
        },
        data: {
          type: VALIDATION_RECORD_TYPE,
          schema: COVENANT_VALIDATION_SCHEMA,
          subject: {
            registryProgram: AGENT_IDENTITY_PROGRAM,
            asset: options.subject ?? AGENT,
          },
          validator: COVENANT_DATA_AUTHORITY,
          hashAlg: COVENANT_VALIDATION_HASH_ALG,
          responseHash: RESPONSE_HASH,
          recordedAt: options.recordedAt ?? 1,
          ...options.data,
        },
      },
    ],
  };
}

function legacyAsset(): Record<string, unknown> {
  return {
    id: LEGACY_RECORD_ASSET,
    interface: 'MplCoreAsset',
    external_plugins: [
      {
        type: 'AppData',
        adapter_config: {
          schema: 'Json',
          data_authority: {type: 'Address', address: COVENANT_DATA_AUTHORITY},
        },
        data: {
          type: LEGACY_RECORD_TYPE,
          schema: LEGACY_RECORD_SCHEMA,
          subject: {registry: 'mpl-agent-014', asset: AGENT},
          validator: COVENANT_DATA_AUTHORITY,
          hashAlg: LEGACY_HASH_ALG,
          responseHash: RESPONSE_HASH,
          recordedAt: 1,
          covenant: {releaseScope: 'audit'},
        },
      },
    ],
  };
}

describe('verifyValidationRecord', () => {
  it('verifies record authenticity without inventing evidence or policy verdicts', async () => {
    assert.deepEqual(await verifyValidationRecord(validationAsset(), COVENANT_DATA_AUTHORITY), {
      asset: 'validation-record',
      recordAuthentic: true,
      evidenceVerified: null,
      policyAccepted: null,
      subjectRegistrationVerified: null,
      profile: COVENANT_VALIDATION_SCHEMA,
      legacy: false,
      subjectAsset: AGENT,
      authority: COVENANT_DATA_AUTHORITY,
      responseHash: RESPONSE_HASH,
      recordedAt: 1,
      reasons: [],
    });
  });

  it('selects by Json schema and AppData write authority, not top-level authority', async () => {
    const wrongWriter = await verifyValidationRecord(
      validationAsset({
        dataAuthority: OTHER_AGENT,
        topLevelAuthority: COVENANT_DATA_AUTHORITY,
      }),
      COVENANT_DATA_AUTHORITY,
    );
    assert.equal(wrongWriter.recordAuthentic, false);
    assert.deepEqual(wrongWriter.reasons, [
      'no AppData adapter matches the pinned authority, Json encoding, record type, and profile',
    ]);

    const binary = await verifyValidationRecord(
      validationAsset({adapterSchema: 'Binary'}),
      COVENANT_DATA_AUTHORITY,
    );
    assert.equal(binary.recordAuthentic, false);
  });

  it('requires an unambiguous Address data-authority variant', async () => {
    const wrongVariant = validationAsset();
    const plugin = (wrongVariant.external_plugins as Array<Record<string, any>>)[0];
    plugin.adapter_config.data_authority.type = 'Owner';
    assert.equal(
      (await verifyValidationRecord(wrongVariant, COVENANT_DATA_AUTHORITY)).recordAuthentic,
      false,
    );

    const ambiguous = validationAsset();
    const ambiguousPlugin = (ambiguous.external_plugins as Array<Record<string, any>>)[0];
    ambiguousPlugin.adapterConfig = ambiguousPlugin.adapter_config;
    assert.equal(
      (await verifyValidationRecord(ambiguous, COVENANT_DATA_AUTHORITY)).recordAuthentic,
      false,
    );
  });

  it('rejects malformed profile fields and unknown fields', async () => {
    const cases: Array<[Record<string, unknown>, string]> = [
      [{hashAlg: 'sha256-merkle'}, `hashAlg is not ${COVENANT_VALIDATION_HASH_ALG}`],
      [{validator: OTHER_AGENT}, 'validator does not match the pinned data authority'],
      [{responseHash: 'AB'.repeat(32)}, 'responseHash is not 64 lowercase hex characters'],
      [{recordedAt: -1}, 'recordedAt is not a non-negative safe integer'],
      [{extensions: []}, 'extensions is not an object'],
      [
        {subject: {registryProgram: OTHER_AGENT, asset: AGENT}},
        `subject.registryProgram is not ${AGENT_IDENTITY_PROGRAM}`,
      ],
      [
        {hash_alg: COVENANT_VALIDATION_HASH_ALG},
        'hashAlg aliases are ambiguous',
      ],
      [{unexpected: true}, 'unknown top-level field unexpected'],
    ];

    for (const [data, reason] of cases) {
      const result = await verifyValidationRecord(validationAsset({data}), COVENANT_DATA_AUTHORITY);
      assert.equal(result.recordAuthentic, false);
      assert.ok(result.reasons.includes(reason), `missing reason: ${reason}`);
    }
  });

  it('requires the record asset itself to be an MPL Core asset', async () => {
    const asset = validationAsset();
    asset.interface = 'V1_NFT';
    const result = await verifyValidationRecord(asset, COVENANT_DATA_AUTHORITY);
    assert.equal(result.recordAuthentic, false);
    assert.deepEqual(result.reasons, ['record asset interface is not MplCoreAsset']);
  });

  it('can bind a direct record to the requested agent', async () => {
    const result = await verifyValidationRecord(
      validationAsset({subject: OTHER_AGENT}),
      COVENANT_DATA_AUTHORITY,
      AGENT,
    );
    assert.equal(result.recordAuthentic, false);
    assert.ok(result.reasons.includes('subject.asset does not match the requested agent'));
  });

  it('keeps the deployed legacy profile behind an explicit verifier', () => {
    const result = verifyLegacyAttestation(legacyAsset(), COVENANT_DATA_AUTHORITY);
    assert.equal(result.recordAuthentic, true);
    assert.equal(result.legacy, true);
    assert.equal(result.profile, LEGACY_RECORD_SCHEMA);
    assert.equal(result.subjectAsset, AGENT);
  });
});

describe('findValidationRecords', () => {
  it('reports the latest matching record and incomplete discovery coverage', async () => {
    const pages: number[] = [];
    const firstPage = [
      validationAsset({id: 'older', recordedAt: 10}),
      ...Array.from({length: 999}, (_, index) => ({id: `unrelated-${index}`})),
    ];
    const secondPage = [
      validationAsset({id: 'latest', recordedAt: 20}),
      validationAsset({id: 'other-subject', subject: OTHER_AGENT, recordedAt: 30}),
    ];
    const rpc: Rpc = async (method, params) => {
      assert.equal(method, 'getAssetsByOwner');
      const page = (params as {page: number}).page;
      pages.push(page);
      return {items: page === 1 ? firstPage : secondPage};
    };

    const result = await findValidationRecords(rpc, AGENT, COVENANT_DATA_AUTHORITY);

    assert.deepEqual(pages, [1, 2]);
    assert.equal(result.count, 2);
    assert.equal(result.latestObserved?.asset, 'latest');
    assert.deepEqual(result.coverage, {
      method: 'validator-owned-assets',
      owner: COVENANT_DATA_AUTHORITY,
      pagesScanned: 2,
      assetsScanned: 1002,
      truncated: false,
      complete: false,
    });
  });

  it('stops after five full pages and marks the window truncated', async () => {
    let calls = 0;
    const fullPage = Array.from({length: 1000}, (_, index) => ({id: `unrelated-${index}`}));
    const rpc: Rpc = async () => {
      calls += 1;
      return {items: fullPage};
    };

    const result = await findValidationRecords(rpc, AGENT, COVENANT_DATA_AUTHORITY);

    assert.equal(calls, 5);
    assert.equal(result.count, 0);
    assert.equal(result.latestObserved, null);
    assert.equal(result.coverage.truncated, true);
    assert.equal(result.coverage.complete, false);
    assert.equal(result.coverage.assetsScanned, 5000);
  });
});

describe('getPassportWithRpc', () => {
  it('returns on-chain URIs without fetching them and labels legacy evidence separately', async () => {
    const registrationUri = 'http://127.0.0.1/private-registration';
    const jsonUri = 'http://169.254.169.254/latest/meta-data';
    const methods: string[] = [];
    let assetLookups = 0;
    const rpc: Rpc = async (method) => {
      methods.push(method);
      if (method === 'getAsset') {
        assetLookups += 1;
        if (assetLookups === 2) return legacyAsset();
        return {
          id: AGENT,
          interface: 'MplCoreAsset',
          burnt: false,
          content: {metadata: {name: 'Agent X'}, json_uri: jsonUri},
          ownership: {owner: OTHER_AGENT},
          authorities: [{address: COVENANT_DATA_AUTHORITY}],
          grouping: [{group_key: 'collection', group_value: COVENANT_COLLECTION}],
          external_plugins: [{type: 'AgentIdentity', adapter_config: {uri: registrationUri}}],
        };
      }
      if (method === 'getAccountInfo') return {value: {owner: AGENT_IDENTITY_PROGRAM}};
      if (method === 'getAssetsByOwner') return {items: []};
      throw new Error(`unexpected RPC method ${method}`);
    };

    const originalFetch = globalThis.fetch;
    let fetchCalls = 0;
    globalThis.fetch = async () => {
      fetchCalls += 1;
      throw new Error('URI fetch attempted');
    };

    try {
      const result = await getPassportWithRpc(rpc, AGENT);
      assert.equal(result.status, 200);
      assert.equal(result.body.asset.uri, jsonUri);
      assert.equal(result.body.registry.registrationUri, registrationUri);
      assert.equal(result.body.registry.accountOwnerMatches, true);
      assert.equal(result.body.attestation, null);
      assert.equal(result.body.validationRecords?.count, 0);
      assert.equal(result.body.validationRecords?.coverage.complete, false);
      assert.equal(result.body.legacyAttestation?.recordAuthentic, true);
      assert.equal(fetchCalls, 0);
      assert.deepEqual(methods.sort(), [
        'getAccountInfo',
        'getAsset',
        'getAsset',
        'getAssetsByOwner',
      ]);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it('rejects a DAS response for a different asset', async () => {
    const rpc: Rpc = async () => ({
      id: OTHER_AGENT,
      interface: 'MplCoreAsset',
    });

    assert.deepEqual(await getPassportWithRpc(rpc, AGENT), {
      status: 502,
      body: {error: 'asset lookup returned a different asset'},
    });
  });

  it('does not relabel a different asset as the pinned legacy record', async () => {
    let assetLookups = 0;
    const rpc: Rpc = async (method) => {
      if (method === 'getAsset') {
        assetLookups += 1;
        if (assetLookups === 2) return {...legacyAsset(), id: 'different-record'};
        return {
          id: AGENT,
          interface: 'MplCoreAsset',
          external_plugins: [],
        };
      }
      if (method === 'getAccountInfo') return {value: null};
      if (method === 'getAssetsByOwner') return {items: []};
      throw new Error(`unexpected RPC method ${method}`);
    };

    const result = await getPassportWithRpc(rpc, AGENT);
    assert.equal(result.status, 200);
    assert.equal(result.body.legacyAttestation, null);
  });
});
