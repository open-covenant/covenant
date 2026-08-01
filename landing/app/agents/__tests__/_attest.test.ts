import { describe, expect, it, vi } from 'vitest';
import { findValidationRecords, inspectValidationRecord } from '../_attest';
import {
  ATTESTATION_HASH_ALG,
  ATTESTATION_SCHEMA_V2,
  ATTESTATION_TYPE,
  COVENANT_DATA_AUTHORITY,
  FEATURED_AGENT_ASSET,
} from '../_registry';

const RECORD_ASSET = '11111111111111111111111111111111';

function record(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: RECORD_ASSET,
    external_plugins: [
      {
        type: 'AppData',
        adapter_config: {
          data_authority: { address: COVENANT_DATA_AUTHORITY },
        },
        data: {
          type: ATTESTATION_TYPE,
          schema: ATTESTATION_SCHEMA_V2,
          hash_alg: ATTESTATION_HASH_ALG,
          response_hash: 'a'.repeat(64),
          validator: COVENANT_DATA_AUTHORITY,
          subject: { registry: 'mpl-agent-014', asset: FEATURED_AGENT_ASSET },
          tag: 'audit',
          covenant: {
            release_target: 'covenant',
            release_subject: 'witness-loop',
            release_scope: 'audit',
          },
          recorded_at: 42,
        },
        ...overrides,
      },
    ],
  };
}

describe('inspectValidationRecord', () => {
  it('reports a structural match without calling it verified', () => {
    expect(inspectValidationRecord(record(), COVENANT_DATA_AUTHORITY)).toMatchObject({
      asset: RECORD_ASSET,
      matchesExpectedEnvelope: true,
      evidenceSource: 'configured_das',
      subjectAsset: FEATURED_AGENT_ASSET,
      authority: COVENANT_DATA_AUTHORITY,
      reasons: [],
    });
  });

  it('does not accept a spoofed top-level plugin authority', () => {
    const observation = inspectValidationRecord(
      record({
        adapter_config: {},
        authority: { address: COVENANT_DATA_AUTHORITY },
      }),
      COVENANT_DATA_AUTHORITY,
    );

    expect(observation.matchesExpectedEnvelope).toBe(false);
    expect(observation.authority).toBeNull();
    expect(observation.reasons).toContain('AppData has no write authority');
  });

  it('rejects an out-of-range reported timestamp before UI formatting', () => {
    const asset = record();
    const plugin = (asset.external_plugins as Array<Record<string, unknown>>)[0];
    const data = plugin.data as Record<string, unknown>;
    data.recorded_at = Number.MAX_SAFE_INTEGER;

    const observation = inspectValidationRecord(asset, COVENANT_DATA_AUTHORITY);

    expect(observation.matchesExpectedEnvelope).toBe(false);
    expect(observation.recordedAt).toBeNull();
    expect(observation.reasons).toContain('recordedAt is outside the supported Unix-seconds range');
  });

  it('requires the complete v2 subject and Covenant envelope', () => {
    const asset = record();
    const plugin = (asset.external_plugins as Array<Record<string, unknown>>)[0];
    const data = plugin.data as Record<string, unknown>;
    data.subject = { asset: FEATURED_AGENT_ASSET };
    delete data.tag;
    delete data.covenant;

    const observation = inspectValidationRecord(asset, COVENANT_DATA_AUTHORITY);

    expect(observation.matchesExpectedEnvelope).toBe(false);
    expect(observation.reasons).toEqual(
      expect.arrayContaining([
        'subject.registry is not mpl-agent-014',
        'tag missing, empty, or unsafe',
        'covenant object missing',
        'covenant.releaseTarget missing, empty, or unsafe',
        'covenant.releaseSubject missing, empty, or unsafe',
        'covenant.releaseScope missing, empty, or unsafe',
      ]),
    );
  });

  it('requires subject.asset, recordedAt, and matching tag/scope', () => {
    const asset = record();
    const plugin = (asset.external_plugins as Array<Record<string, unknown>>)[0];
    const data = plugin.data as Record<string, unknown>;
    data.subject = { registry: 'mpl-agent-014' };
    data.tag = 'different-scope';
    delete data.recorded_at;

    const observation = inspectValidationRecord(asset, COVENANT_DATA_AUTHORITY);

    expect(observation.matchesExpectedEnvelope).toBe(false);
    expect(observation.reasons).toEqual(
      expect.arrayContaining([
        'subject.asset missing',
        'tag does not match covenant.releaseScope',
        'recordedAt missing',
      ]),
    );
  });

  it('requires the expected validator and hash fields', () => {
    const asset = record();
    const plugin = (asset.external_plugins as Array<Record<string, unknown>>)[0];
    const data = plugin.data as Record<string, unknown>;
    data.validator = 'So11111111111111111111111111111111111111112';
    data.hash_alg = 'keccak256';
    data.response_hash = 'A'.repeat(64);

    const observation = inspectValidationRecord(asset, COVENANT_DATA_AUTHORITY);

    expect(observation.matchesExpectedEnvelope).toBe(false);
    expect(observation.reasons).toEqual(
      expect.arrayContaining([
        `hashAlg is not ${ATTESTATION_HASH_ALG}`,
        'responseHash is not 64 lowercase hex',
        'validator field does not match the expected authority',
      ]),
    );
  });
});

describe('findValidationRecords', () => {
  it('returns only matching records reported by the configured DAS provider', async () => {
    const rpc = vi.fn(async () => ({
      items: [
        record(),
        record({
          data: {
            type: ATTESTATION_TYPE,
            schema: ATTESTATION_SCHEMA_V2,
            hash_alg: ATTESTATION_HASH_ALG,
            response_hash: 'b'.repeat(64),
            validator: COVENANT_DATA_AUTHORITY,
            subject: {
              registry: 'mpl-agent-014',
              asset: 'So11111111111111111111111111111111111111112',
            },
            tag: 'audit',
            covenant: {
              release_target: 'covenant',
              release_subject: 'witness-loop',
              release_scope: 'audit',
            },
            recorded_at: 43,
          },
        }),
      ],
    }));

    const lookup = await findValidationRecords(rpc, FEATURED_AGENT_ASSET, COVENANT_DATA_AUTHORITY);

    expect(lookup).toMatchObject({
      hasMatchingRecord: true,
      count: 1,
      truncated: false,
    });
    expect(rpc).toHaveBeenCalledWith('getAssetsByOwner', {
      ownerAddress: COVENANT_DATA_AUTHORITY,
      page: 1,
      limit: 1000,
    });
  });
});
