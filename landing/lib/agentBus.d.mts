export type AgentEvent =
  | {
      type: "transition";
      ts: string;
      taskId: string;
      from: string;
      to: string;
      actor: string;
      note: string;
    }
  | { type: "commit"; hash: string; subject: string; stat: string };

export const bus: {
  ring: AgentEvent[];
  subscribe(cb: (e: AgentEvent) => void): () => void;
  publish(e: AgentEvent): void;
  startLocalTail(): void;
};

export function clean(s: unknown): string;
export function findRepoRoot(startDir?: string): string | null;
export function parseTransitionLine(line: string): AgentEvent | null;
export function commitEvent(root: string, hash: string): AgentEvent;
