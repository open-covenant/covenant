// Pure rendering helpers for the live autonomy feed in AgentTerminal. Kept out
// of the client component so the event-to-line formatting can be unit-tested
// without a browser.

export type Line = { k: string; t: string };

export type AgentEvent =
  | { type: "transition"; taskId: string; from: string; to: string; actor: string; note: string }
  | { type: "commit"; hash: string; subject: string; stat: string };

export function stateKind(to: string): string {
  if (to === "integrated") return "add";
  if (to === "ready") return "write";
  if (to === "blocked" || to === "repair") return "del";
  if (to === "validation" || to === "cross_review" || to === "self_review") return "hunk";
  return "meta";
}

export function formatEvent(e: AgentEvent): Line[] {
  if (e.type === "commit") {
    const out: Line[] = [{ k: "commit", t: `commited ${e.hash}  # ${e.subject}` }];
    if (e.stat) out.push({ k: "meta", t: `  ${e.stat}` });
    out.push({ k: "blank", t: "" });
    return out;
  }
  if (e.type === "transition") {
    const out: Line[] = [{ k: stateKind(e.to), t: `[${e.to}] ${e.taskId}` }];
    if (e.note) out.push({ k: "ctx", t: `    ${e.note}` });
    out.push({ k: "blank", t: "" });
    return out;
  }
  return [];
}
