import { describe, expect, it } from 'vitest';

import { signerRoleForCommand } from '../worker-policy.js';

describe('worker signer policy', () => {
  it.each(['publish-agent', 'update-agent', 'attest-root'])(
    'gives %s only the payer role',
    (command) => {
      expect(signerRoleForCommand(command)).toBe('payer');
    },
  );

  it('gives attest-agent only the verifier role', () => {
    expect(signerRoleForCommand('attest-agent')).toBe('verifier');
  });

  it.each([
    'find-agent',
    'describe-agent',
    'find-by-protocol',
    'status',
    'stats',
    'unknown',
  ])('gives %s no signer role', (command) => {
    expect(signerRoleForCommand(command)).toBeNull();
  });
});
