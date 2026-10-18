type WorkspaceRelease = {
  accepted: boolean;
  releaseUrl: string | null;
  sourceUrl: string | null;
  documentationUrl: string | null;
  installation: string | null;
  acceptance: null | {
    checkedAt: string;
    evidenceUrl: string;
    storageProvider: string;
    sourceLeaseId: string;
    targetLeaseId: string;
    archiveSha256: string;
    ciphertextSha256: string;
    cleanupVerified: boolean;
    crossLeaseVerified: boolean;
  };
};

// Local preparation only. Provider acceptance, released-client recovery and
// independently authenticated access to both real leases remain release gates.
export const workspaceRelease: WorkspaceRelease = {
  accepted: false,
  releaseUrl: null,
  sourceUrl: null,
  documentationUrl: null,
  installation: null,
  acceptance: null,
};
