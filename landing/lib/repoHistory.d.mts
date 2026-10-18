export const LOOP_AUTHOR: string;

export function readHistory(opts?: { repoRoot?: string | null; url?: string }): {
  commits: number | null;
  head: string | null;
  reason?: string;
};
