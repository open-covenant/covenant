import { chmodSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { loadKeystore, parseEnvFile, redact, REDACTED } from '../keystore.js';
import { KeystoreError } from '../errors.js';

const KEY = `0x${'ab'.repeat(32)}`;

function keyFile(contents: string, mode = 0o600): string {
  const dir = mkdtempSync(path.join(tmpdir(), 'desk-keys-'));
  const file = path.join(dir, 'keys.env');
  writeFileSync(file, contents, { mode });
  chmodSync(file, mode);
  return file;
}

describe('parseEnvFile', () => {
  it('reads plain, quoted, and exported lines', () => {
    const parsed = parseEnvFile(
      ['# comment', '', 'A=1', 'export B="two"', "C='three'", 'D=with=equals', 'bad-line'].join('\n'),
    );
    expect(parsed).toEqual({ A: '1', B: 'two', C: 'three', D: 'with=equals' });
  });
});

describe('loadKeystore', () => {
  it('reads the four desk keys from a 0600 file', () => {
    const file = keyFile(
      [
        `DESK_EVM_PRIVATE_KEY=${KEY}`,
        'DESK_LIGHTER_RH_ACCOUNT_INDEX=7',
        'UNRELATED=ignored',
      ].join('\n'),
    );
    const keys = loadKeystore({ file, env: {}, useKeychain: false });
    expect(keys.get('DESK_EVM_PRIVATE_KEY')).toBe(KEY);
    expect(keys.get('DESK_LIGHTER_RH_ACCOUNT_INDEX')).toBe('7');
    expect(keys.has('DESK_LIGHTER_RH_PRIVATE_KEY')).toBe(false);
    expect(keys.names().sort()).toEqual(['DESK_EVM_PRIVATE_KEY', 'DESK_LIGHTER_RH_ACCOUNT_INDEX']);
  });

  it('refuses a key file other users can read', () => {
    const file = keyFile(`DESK_EVM_PRIVATE_KEY=${KEY}`, 0o644);
    expect(() => loadKeystore({ file, env: {}, useKeychain: false })).toThrow(KeystoreError);
  });

  it('falls back to the environment when the file has no such key', () => {
    const file = keyFile('DESK_EVM_PRIVATE_KEY=' + KEY);
    const keys = loadKeystore({
      file,
      env: { DESK_LIGHTER_RH_API_KEY_INDEX: '3' } as NodeJS.ProcessEnv,
      useKeychain: false,
    });
    expect(keys.get('DESK_LIGHTER_RH_API_KEY_INDEX')).toBe('3');
  });

  it('runs without a key file and names the file when a key is required', () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'desk-keys-'));
    const file = path.join(dir, 'keys.env');
    const keys = loadKeystore({ file, env: {}, useKeychain: false });
    expect(keys.names()).toEqual([]);
    expect(() => keys.require('DESK_EVM_PRIVATE_KEY')).toThrow(/keys\.env/);
  });

  it('lists private keys as secrets and leaves indexes out', () => {
    const file = keyFile([`DESK_EVM_PRIVATE_KEY=${KEY}`, 'DESK_LIGHTER_RH_ACCOUNT_INDEX=7'].join('\n'));
    const keys = loadKeystore({ file, env: {}, useKeychain: false });
    expect(keys.secrets()).toEqual([KEY]);
  });
});

describe('redact', () => {
  it('replaces a known secret wherever it appears', () => {
    const out = redact({ note: `signed with ${KEY}` }, [KEY]);
    expect(out.note).toBe(`signed with ${REDACTED}`);
  });

  it('masks fields whose name reads like a credential', () => {
    const out = redact({ bearer: 'abc123', nested: { privateKey: KEY } }, []);
    expect(out.bearer).toBe(REDACTED);
    expect((out.nested as { privateKey: string }).privateKey).toBe(REDACTED);
  });

  it('keeps the words the desk uses for a traded token readable', () => {
    const out = redact(
      { tokens: 194, token: '0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9', tokensTracked: 30 },
      [],
    );
    expect(out.tokens).toBe(194);
    expect(out.token).toBe('0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9');
    expect(out.tokensTracked).toBe(30);
  });

  it('keeps addresses, transaction hashes, and pool ids readable', () => {
    const address = '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC';
    const poolId = '0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147';
    const out = redact({ address, poolId }, []);
    expect(out.address).toBe(address);
    expect(out.poolId).toBe(poolId);
  });

  it('masks a bearer token registered as a known value', () => {
    const bearer = 'a1'.repeat(32);
    const out = redact({ url: `http://127.0.0.1:46631/?token=${bearer}` }, [bearer]);
    expect(out.url).toBe(`http://127.0.0.1:46631/?token=${REDACTED}`);
  });

  it('masks a bare 32 byte hex string', () => {
    const out = redact({ note: 'ab'.repeat(32) }, []);
    expect(out.note).toBe(REDACTED);
  });

  it('turns bigints into strings and survives cycles', () => {
    const cyclic: Record<string, unknown> = { amount: 10n ** 18n };
    cyclic.self = cyclic;
    const out = redact(cyclic, []);
    expect(out.amount).toBe('1000000000000000000');
    expect(out.self).toBe('[circular]');
  });
});
