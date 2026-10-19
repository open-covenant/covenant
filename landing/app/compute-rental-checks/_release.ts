type RentalChecksRelease = {
  accepted: boolean;
  sourceUrl: string | null;
  documentationUrl: string | null;
  releaseUrl: string | null;
  acceptanceUrl: string | null;
  acceptedAt: string | null;
};

// Activation requires a released-client rental with authenticated SSH, a GPU
// probe, reconciled settlement, provider cleanup and a verified live schedule.
export const rentalChecksRelease: RentalChecksRelease = {
  accepted: true,
  sourceUrl: "https://github.com/open-covenant/covenant/commit/5f0f006aaca4150a79f5fb56f8b5f25ba3eacdf9",
  documentationUrl: "https://github.com/open-covenant/covenant/blob/5f0f006aaca4150a79f5fb56f8b5f25ba3eacdf9/deploy/compute-canary/README.md",
  releaseUrl: "https://github.com/open-covenant/covenant/releases/tag/compute-v0.3.1",
  acceptanceUrl: "https://github.com/open-covenant/covenant/blob/ff0c51d71455d744f9f4a9b5bab13a75ae642c16/docs/compute-rental-checks-release-evidence.md",
  acceptedAt: "2026-10-19T01:15:41.881489Z",
};
