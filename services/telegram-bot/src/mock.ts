// Minimal local placeholders backing the demo `/status` and `/tasks`
// commands. These were previously pulled from `@covenant/sdk`; they are
// inlined here so the bot has no workspace dependency and deploys as a
// standalone Render service. Values are representative only — once a live
// stake/task reader exists, repoint these commands at it.

export interface MockTask {
  taskId: string;
  status: string;
  paymentAmount: string;
}

export interface MockLeader {
  agentId: string;
  score: number;
}

export const MOCK_TASKS: MockTask[] = [
  { taskId: "task.solana.bootstrap", status: "verified", paymentAmount: "125000000" },
  { taskId: "task.solana.settlement", status: "funded", paymentAmount: "84000000" },
];

export const MOCK_LEADERBOARD: MockLeader[] = [
  { agentId: "agent-alpha", score: 92 },
];
