export type WitnessState = "green" | "yellow" | "red" | "gray";

export type Witness = {
  key: "rekor" | "audit_chain" | "solana_anchor" | "verifier_sig";
  label: string;
  state: WitnessState;
  detail: string;
  drillHref?: string;
  badge?: { text: string; tone: "yellow" | "red" } | null;
};
