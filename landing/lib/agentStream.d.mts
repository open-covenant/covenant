export type StreamLine = { k: string; t: string };
export type Stream = { commits: number; lines: StreamLine[] };

export function clean(s: unknown): string | null;

export function findRepoRoot(startDir?: string): string | null;

export function generateStream(opts?: {
  repoRoot?: string | null;
  maxCommits?: number;
  maxFilesPerCommit?: number;
  maxBodyLines?: number;
  maxTotalLines?: number;
}): Stream;
