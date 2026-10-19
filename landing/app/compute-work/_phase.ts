export type WorkPilotPhase = "awaiting_funding" | "live";

// Server-only release setting. Funding is still established by the public ledger,
// never by this display switch. Missing or invalid settings remain unfunded.
export function workPilotPhase(value = process.env.COVENANT_COMPUTE_WORK_PHASE): WorkPilotPhase {
  return value === "live" ? "live" : "awaiting_funding";
}
