// The Covenant audit gate: a Core Oracle external plugin whose base_address is
// the covenant-oracle ["oracle", asset] PDA. The plugin makes MPL Core read the
// live verdict from that PDA during the agent's lifecycle events, so transfer
// (and, extended, execute) only succeeds while the audit is in policy. The
// gating program is source-verified on mainnet; pinning base_address to the
// derived PDA under COVENANT_ORACLE_PROGRAM is what ties this gate to it.

import { PublicKey } from "@solana/web3.js";
import { COVENANT_ORACLE_PROGRAM } from "@/app/agents/_registry";

export type AuditGate = {
  /** A Covenant oracle plugin gates this asset (base_address is our PDA). */
  gated: boolean;
  programId: string;
  oraclePda: string;
  /** Lifecycle events the oracle can veto, e.g. ["transfer"]. */
  gatedEvents: string[];
  /** true = in policy (transferable), false = out of policy (vetoed), null = unread. */
  inPolicy: boolean | null;
  /** The gating program is source-verified on-chain (pinned by programId). */
  programVerified: boolean;
};

type Rpc = (method: string, params: unknown) => Promise<unknown>;

export async function readAuditGate(
  rpc: Rpc,
  externalPlugins: Array<Record<string, unknown>>,
  assetPk: PublicKey,
): Promise<AuditGate | null> {
  const oracle = externalPlugins.find((p) => p["type"] === "Oracle");
  if (!oracle) return null;

  const cfg = (oracle["adapter_config"] ?? {}) as Record<string, unknown>;
  const baseAddress = (cfg["base_address"] as string | undefined) ?? null;
  const [pda] = PublicKey.findProgramAddressSync(
    [Buffer.from("oracle"), assetPk.toBytes()],
    new PublicKey(COVENANT_ORACLE_PROGRAM),
  );
  const gated = baseAddress === pda.toBase58();
  const gatedEvents = Object.keys((oracle["lifecycle_checks"] ?? {}) as Record<string, unknown>);

  let inPolicy: boolean | null = null;
  if (gated) {
    try {
      const info = (await rpc("getAccountInfo", [
        pda.toBase58(),
        { encoding: "base64" },
      ])) as { value: { data: [string, string] } | null } | null;
      const b64 = info?.value?.data?.[0];
      if (b64) {
        // OracleState = disc(8) + OracleValidation::V1 { tag(1), create, transfer, burn, update }.
        // The transfer verdict sits at byte 10: 2 = Pass (in policy), 1 = Rejected.
        const v = Buffer.from(b64, "base64")[10];
        inPolicy = v === 2 ? true : v === 1 ? false : null;
      }
    } catch {
      // leave inPolicy null — the gate renders an "unknown verdict" state
    }
  }

  return {
    gated,
    programId: COVENANT_ORACLE_PROGRAM,
    oraclePda: pda.toBase58(),
    gatedEvents,
    inPolicy,
    programVerified: gated,
  };
}
