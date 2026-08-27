import { describe, expect, it } from 'vitest';

import { stopStorageCopy } from './JobPanel';

describe('workload stop warning', () => {
  it('warns about ephemeral storage before and during confirmation', () => {
    expect(stopStorageCopy.idle).toContain('deleted when stopped');
    expect(stopStorageCopy.idle).toContain('Download your work first');
    expect(stopStorageCopy.armed).toContain('permanently deletes');
    expect(stopStorageCopy.armed).toContain('before confirming');
  });
});
