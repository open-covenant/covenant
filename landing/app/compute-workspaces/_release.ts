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

const source = "https://github.com/open-covenant/covenant/blob/97550b20f256469d0b58e7b2a35e22cc0745b7fd";

// The installed client, both funded leases, provider teardown and permanent
// empty ciphertext fences passed acceptance before this page was activated.
export const workspaceRelease: WorkspaceRelease = {
  accepted: true,
  releaseUrl: "https://github.com/open-covenant/covenant/releases/tag/compute-v0.3.0",
  sourceUrl: `${source}/agent-os/crates/covenant-compute-buyer/src/workspace_cli.rs`,
  documentationUrl: `${source}/docs/compute-workspaces.md`,
  installation: `# Installs the checksum-pinned Compute 0.3.0 release for macOS or Linux.
curl -fsSL https://opencovenant.org/install/compute.sh | sh
export PATH="$HOME/.local/bin:$PATH"
export COVENANT_COMPUTE_COORDINATOR_URL=https://compute-api.opencovenant.org`,
  acceptance: {
    checkedAt: "18 Oct 2026 · 22:47 UTC",
    evidenceUrl: "https://github.com/open-covenant/covenant/blob/56792f5606f6bd884a8789a214507c3df7a216d5/docs/compute-workspace-release-evidence.md",
    storageProvider: "Cloudflare R2",
    sourceLeaseId: "64b6d628-396e-40cc-bd3c-169146348ec8",
    targetLeaseId: "e356d88c-88d6-4320-b9a7-a0bdaf013605",
    archiveSha256: "5be69dd8664560c210e9a629b18fb73d5de501e5f8ee2be0188af78978ce50eb",
    ciphertextSha256: "036fedf977fd837c95156449609d0a0c580b2595b5731678ad9a323b1cccb524",
    cleanupVerified: true,
    crossLeaseVerified: true,
  },
};
