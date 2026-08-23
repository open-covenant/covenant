import { createHash } from 'node:crypto';
import { Keypair, PublicKey } from '@solana/web3.js';
import { describe, expect, it } from 'vitest';
import {
  DevnetCanaryError,
  decodeEscrowState,
  inspectLoaderV3Deployment,
  inspectSbpfArtifact,
  parseDevnetCanaryArgs,
  validateDevnetRpcUrl,
} from './devnet-canary.js';

const LOADER = new PublicKey('BPFLoaderUpgradeab1e11111111111111111111111');

function artifact(sbpfVersion = 2): Buffer {
  const data = Buffer.alloc(96);
  Buffer.from([0x7f, 0x45, 0x4c, 0x46]).copy(data);
  data[4] = 2;
  data[5] = 1;
  data.writeUInt16LE(247, 18);
  data.writeUInt32LE(sbpfVersion, 48);
  return data;
}

function requiredArgs(): string[] {
  return [
    '--rpc-url-file',
    'rpc.secret',
    '--program-id',
    Keypair.generate().publicKey.toBase58(),
    '--artifact',
    'program.so',
    '--artifact-sha256',
    'ab'.repeat(32),
    '--artifact-commit',
    'cd'.repeat(20),
    '--authority-keypair',
    'authority.json',
    '--claimant-keypair',
    'claimant.json',
    '--adversary-keypair',
    'adversary.json',
    '--output',
    'receipt.json',
  ];
}

describe('devnet canary boundary', () => {
  it('defaults to read-only and requires an explicit execute flag', () => {
    const dryRun = parseDevnetCanaryArgs(requiredArgs());
    expect(dryRun.execute).toBe(false);
    expect(dryRun.amountLamports).toBe(1_000_000);
    expect(dryRun.expirySeconds).toBe(90);

    const execute = parseDevnetCanaryArgs([
      ...requiredArgs(),
      '--amount-lamports',
      '2000000',
      '--expiry-seconds',
      '120',
      '--execute',
    ]);
    expect(execute.execute).toBe(true);
    expect(execute.amountLamports).toBe(2_000_000);
    expect(execute.expirySeconds).toBe(120);
  });

  it('rejects missing, duplicate, and out-of-range arguments', () => {
    expect(() => parseDevnetCanaryArgs([])).toThrowError(DevnetCanaryError);
    expect(() =>
      parseDevnetCanaryArgs([...requiredArgs(), '--output', 'second.json']),
    ).toThrowError('duplicate_argument');
    expect(() => parseDevnetCanaryArgs([...requiredArgs(), '--expiry-seconds', '30'])).toThrowError(
      'numeric_argument_out_of_range',
    );
  });

  it('only accepts credential-free HTTPS RPC URLs without mainnet markers', () => {
    expect(validateDevnetRpcUrl('https://api.devnet.solana.com')).toBe(
      'https://api.devnet.solana.com/',
    );
    for (const value of [
      'http://api.devnet.solana.com',
      'https://api.mainnet-beta.solana.com',
      ['https://user:secret', 'api.devnet.solana.com'].join(String.fromCharCode(64)),
      'https://localhost',
      'not-a-url',
    ]) {
      expect(() => validateDevnetRpcUrl(value)).toThrowError(DevnetCanaryError);
    }
  });

  it('pins the exact SBPFv2 artifact bytes', () => {
    const data = artifact();
    const hash = createHash('sha256').update(data).digest('hex');
    expect(inspectSbpfArtifact(data, hash)).toEqual({
      sha256: hash,
      bytes: data.length,
      sbpfVersion: 2,
    });

    const changed = Buffer.from(data);
    changed[63] = 1;
    expect(() => inspectSbpfArtifact(changed, hash)).toThrowError('artifact_hash_mismatch');
    for (const unsupportedVersion of [1, 3]) {
      const unsupported = artifact(unsupportedVersion);
      const unsupportedHash = createHash('sha256').update(unsupported).digest('hex');
      expect(() => inspectSbpfArtifact(unsupported, unsupportedHash)).toThrowError(
        'artifact_not_sbpf_v2',
      );
    }
  });

  it('requires loader-v3 deployment bytes to equal the local artifact', () => {
    const programId = Keypair.generate().publicKey;
    const programDataAddress = Keypair.generate().publicKey;
    const data = artifact();
    const hash = createHash('sha256').update(data).digest('hex');
    const program = {
      data: Buffer.concat([uint32(2), programDataAddress.toBuffer()]),
      executable: true,
      lamports: 1,
      owner: LOADER,
      rentEpoch: 0,
    };
    const programData = {
      data: Buffer.concat([programDataHeader(true), data]),
      executable: false,
      lamports: 1,
      owner: LOADER,
      rentEpoch: 0,
    };

    expect(inspectLoaderV3Deployment(programId, program, programData, data, hash)).toEqual({
      upgradeAuthorityPresent: true,
    });

    const changed = Buffer.from(programData.data);
    changed[changed.length - 1] ^= 1;
    expect(() =>
      inspectLoaderV3Deployment(programId, program, { ...programData, data: changed }, data, hash),
    ).toThrowError('deployed_artifact_mismatch');
  });

  it('decodes the fixed state layout and rejects wrong magic', () => {
    const data = Buffer.alloc(236);
    Buffer.from('4d5a4b4553433100', 'hex').copy(data);
    data[8] = 1;
    data[9] = 1;
    const authority = Keypair.generate().publicKey;
    authority.toBuffer().copy(data, 12);
    Buffer.alloc(32, 7).copy(data, 76);
    data.writeBigUInt64LE(1_000_000n, 108);
    data.writeBigInt64LE(10n, 116);
    data.writeBigInt64LE(100n, 124);
    Buffer.alloc(32, 8).copy(data, 140);

    expect(decodeEscrowState(data)).toMatchObject({
      status: 1,
      amountLamports: 1_000_000n,
      createdAt: 10n,
      offerExpiresAt: 100n,
    });
    data[0] = 0;
    expect(() => decodeEscrowState(data)).toThrowError('invalid_escrow_state');
  });
});

function uint32(value: number): Buffer {
  const data = Buffer.alloc(4);
  data.writeUInt32LE(value);
  return data;
}

function programDataHeader(hasAuthority: boolean): Buffer {
  const data = Buffer.alloc(45);
  data.writeUInt32LE(3, 0);
  data[12] = hasAuthority ? 1 : 0;
  if (hasAuthority) Keypair.generate().publicKey.toBuffer().copy(data, 13);
  return data;
}
