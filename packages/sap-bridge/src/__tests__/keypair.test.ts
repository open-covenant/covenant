import { afterAll, describe, expect, it } from 'vitest';
import { Keypair } from '@solana/web3.js';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { loadKeypairFromFile } from '../keypair.js';

const dir = mkdtempSync(join(tmpdir(), 'sap-keypair-'));
const tmpFile = (contents: string): string => {
  const path = join(dir, `${randomUUID()}.json`);
  writeFileSync(path, contents);
  return path;
};

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe('loadKeypairFromFile', () => {
  it('loads a CLI keypair file and reconstructs the signing identity', async () => {
    const kp = Keypair.generate();
    const path = tmpFile(JSON.stringify(Array.from(kp.secretKey)));
    const loaded = await loadKeypairFromFile(path);
    expect(loaded.publicKey.toBase58()).toBe(kp.publicKey.toBase58());
  });

  it('fails closed when the keypair file cannot be read', async () => {
    const path = join(dir, 'does-not-exist.json');
    await expect(loadKeypairFromFile(path)).rejects.toThrow(/cannot read keypair file/);
  });

  it('fails closed when the keypair file is not valid JSON', async () => {
    const path = tmpFile('-- not json --');
    await expect(loadKeypairFromFile(path)).rejects.toThrow(/cannot read keypair file/);
  });

  it('rejects a keypair file that is not a JSON byte array', async () => {
    const path = tmpFile(JSON.stringify({ secret: [1, 2, 3] }));
    await expect(loadKeypairFromFile(path)).rejects.toThrow(/not a JSON byte array/);
  });

  it('rejects a byte array containing a non-numeric element', async () => {
    const path = tmpFile(JSON.stringify([1, 2, 'ff', 4]));
    await expect(loadKeypairFromFile(path)).rejects.toThrow(/not a JSON byte array/);
  });
});
