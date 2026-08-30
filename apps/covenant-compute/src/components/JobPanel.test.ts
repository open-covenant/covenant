import { describe, expect, it } from 'vitest';

import { slowProvisioningCopy, stopStorageCopy, unreportedFailureCopy } from './JobPanel';

describe('workload stop warning', () => {
  it('warns about ephemeral storage before and during confirmation', () => {
    expect(stopStorageCopy.idle).toContain('deleted when stopped');
    expect(stopStorageCopy.idle).toContain('Download your work first');
    expect(stopStorageCopy.armed).toContain('permanently deletes');
    expect(stopStorageCopy.armed).toContain('before confirming');
  });
});

describe('workload status copy', () => {
  it('offers a way out of a long provisioning wait', () => {
    expect(slowProvisioningCopy).toContain('longer than usual');
    expect(slowProvisioningCopy).toContain('stop the workspace at any time');
  });

  it('explains a failure the provider left unexplained', () => {
    expect(unreportedFailureCopy).toContain('did not report a reason');
  });
});
